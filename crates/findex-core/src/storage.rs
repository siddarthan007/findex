use crate::discovery::DiscoveredFile;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Sled(#[from] sled::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("UTF-8 parsing error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub id: String,        // SCIP-style stable ID: "path/to/file.rs#SymbolName"
    pub name: String,      // Name of the symbol: "main"
    pub kind: String,      // "Function", "Class", "Interface", "Struct", etc.
    pub signature: String, // Fully qualified signature: "pub fn main() -> Result<()>"
    pub file_path: String, // Relative/absolute path to file
    pub start_line: usize, // 1-indexed
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub docstring: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub token_count: usize,
    #[serde(default)]
    pub ast_hash: Option<String>,
    #[serde(default)]
    pub qualified_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Chunk {
    pub id: String,
    pub parent_symbol_id: String,
    pub file_path: String,
    pub chunk_index: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    pub token_count: usize,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EdgeType {
    #[default]
    Calls,
    Imports,
    Defines,
    References,
    Inherits,
    Contains,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edge {
    pub src: String,
    pub dst: String,
    pub edge_type: EdgeType,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
}

pub struct Storage {
    _db: sled::Db,
    metadata: sled::Tree,
    files: sled::Tree,
    symbols: sled::Tree,
    edges: sled::Tree,
    symbols_by_file: sled::Tree,
    chunks: sled::Tree,
    chunks_by_symbol: sled::Tree,
    /// In-memory name -> symbol-ids lookup. Persisting this as a secondary Sled
    /// tree turned out to be a Phase-0 throughput bottleneck, so we keep it in
    /// memory and rebuild it lazily from the primary `symbols` tree.
    symbols_by_name: Mutex<Option<HashMap<String, Vec<String>>>>,
    edges_by_src: sled::Tree,
    edges_by_dst: sled::Tree,
}

const IDX_SEP: char = '\0';

impl Storage {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        let db = sled::open(path)?;
        let metadata = db.open_tree("metadata")?;
        let files = db.open_tree("files")?;
        let symbols = db.open_tree("symbols")?;
        let edges = db.open_tree("edges")?;
        let symbols_by_file = db.open_tree("symbols_by_file")?;
        let chunks = db.open_tree("chunks")?;
        let chunks_by_symbol = db.open_tree("chunks_by_symbol")?;
        // Legacy `symbols_by_name` Sled tree may exist from earlier versions; ignore it.
        let _ = db.drop_tree("symbols_by_name");
        let edges_by_src = db.open_tree("edges_by_src")?;
        let edges_by_dst = db.open_tree("edges_by_dst")?;

        Ok(Self {
            _db: db,
            metadata,
            files,
            symbols,
            edges,
            symbols_by_file,
            chunks,
            chunks_by_symbol,
            symbols_by_name: Mutex::new(None),
            edges_by_src,
            edges_by_dst,
        })
    }

    // --- File operations ---

    pub fn index_format_version(&self) -> Result<u32, StorageError> {
        let Some(value) = self.metadata.get("index_format_version")? else {
            return Ok(0);
        };
        let bytes: [u8; 4] = match value.as_ref().try_into() {
            Ok(bytes) => bytes,
            Err(_) => return Ok(0),
        };
        Ok(u32::from_be_bytes(bytes))
    }

    pub fn set_index_format_version(&self, version: u32) -> Result<(), StorageError> {
        self.metadata
            .insert("index_format_version", &version.to_be_bytes())?;
        Ok(())
    }

    /// Persist typed auxiliary state without coupling the primary schema to a
    /// feature. Merkle snapshots, task records and runtime diagnostics use
    /// namespaced keys here.
    pub fn set_metadata<T: Serialize>(&self, key: &str, value: &T) -> Result<(), StorageError> {
        self.metadata
            .insert(key.as_bytes(), serde_json::to_vec(value)?)?;
        Ok(())
    }

    pub fn get_metadata<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StorageError> {
        self.metadata
            .get(key.as_bytes())?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(StorageError::from))
            .transpose()
    }

    pub fn remove_metadata(&self, key: &str) -> Result<(), StorageError> {
        self.metadata.remove(key.as_bytes())?;
        Ok(())
    }

    fn edge_key(edge: &Edge) -> String {
        format!("{}:{}:{:?}", edge.src, edge.dst, edge.edge_type)
    }

    fn index_key(prefix: &str, suffix: &str) -> Vec<u8> {
        format!("{}{}{}", prefix, IDX_SEP, suffix).into_bytes()
    }

    fn scan_prefix(&self, tree: &sled::Tree, prefix: &str) -> Result<Vec<String>, StorageError> {
        let prefix_bytes = format!("{}{}", prefix, IDX_SEP).into_bytes();
        let mut out = Vec::new();
        for item in tree.scan_prefix(&prefix_bytes) {
            let (key, _) = item?;
            let key_str = String::from_utf8(key.to_vec())?;
            let mut parts = key_str.split(IDX_SEP);
            let _first = parts.next();
            if let Some(second) = parts.next() {
                out.push(second.to_string());
            }
        }
        Ok(out)
    }

    fn remove_index_key(
        &self,
        tree: &sled::Tree,
        prefix: &str,
        suffix: &str,
    ) -> Result<(), StorageError> {
        tree.remove(Self::index_key(prefix, suffix))?;
        Ok(())
    }

    pub fn save_file(&self, file: &DiscoveredFile) -> Result<(), StorageError> {
        let key = file.path.to_string_lossy().to_string();
        let value = serde_json::to_vec(file)?;
        self.files.insert(key.as_bytes(), value)?;
        Ok(())
    }

    pub fn get_file(&self, path: &Path) -> Result<Option<DiscoveredFile>, StorageError> {
        let key = path.to_string_lossy().to_string();
        if let Some(val_bytes) = self.files.get(key.as_bytes())? {
            let file: DiscoveredFile = serde_json::from_slice(&val_bytes)?;
            Ok(Some(file))
        } else {
            Ok(None)
        }
    }

    pub fn delete_file(&self, path: &Path) -> Result<(), StorageError> {
        let key = path.to_string_lossy().to_string();
        self.files.remove(key.as_bytes())?;

        // When a file is deleted, we should also clean up its symbols and edges
        self.delete_symbols_for_file(path)?;
        Ok(())
    }

    pub fn list_files(&self) -> Result<Vec<DiscoveredFile>, StorageError> {
        let mut files = Vec::new();
        for item in self.files.iter() {
            let (_, val) = item?;
            let file: DiscoveredFile = serde_json::from_slice(&val)?;
            files.push(file);
        }
        Ok(files)
    }

    // --- Symbol operations ---

    pub fn save_symbol(&self, symbol: &Symbol) -> Result<(), StorageError> {
        let value = serde_json::to_vec(symbol)?;
        self.symbols.insert(symbol.id.as_bytes(), value)?;
        self.symbols_by_file
            .insert(Self::index_key(&symbol.file_path, &symbol.id), &[])?;
        if let Some(ref mut cache) = *self.symbols_by_name.lock().unwrap() {
            cache
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.id.clone());
        }
        Ok(())
    }

    pub fn save_symbols_batch(&self, symbols: &[Symbol]) -> Result<(), StorageError> {
        let mut batch = sled::Batch::default();
        for symbol in symbols {
            let value = serde_json::to_vec(symbol)?;
            batch.insert(symbol.id.as_bytes(), value);
        }
        self.symbols.apply_batch(batch)?;

        let mut by_file_batch = sled::Batch::default();
        for symbol in symbols {
            by_file_batch.insert(Self::index_key(&symbol.file_path, &symbol.id), &[] as &[u8]);
        }
        self.symbols_by_file.apply_batch(by_file_batch)?;

        if let Some(ref mut cache) = *self.symbols_by_name.lock().unwrap() {
            for symbol in symbols {
                cache
                    .entry(symbol.name.clone())
                    .or_default()
                    .push(symbol.id.clone());
            }
        }
        Ok(())
    }

    pub fn get_symbol(&self, id: &str) -> Result<Option<Symbol>, StorageError> {
        if let Some(val_bytes) = self.symbols.get(id.as_bytes())? {
            let symbol: Symbol = serde_json::from_slice(&val_bytes)?;
            Ok(Some(symbol))
        } else {
            Ok(None)
        }
    }

    pub fn delete_symbols_for_file(&self, path: &Path) -> Result<(), StorageError> {
        let path_str = path.to_string_lossy().to_string();
        let ids = self.list_symbol_ids_for_file(path)?;

        let mut batch = sled::Batch::default();
        for id in &ids {
            self.delete_chunks_for_symbol(id)?;
            if let Some(symbol) = self.get_symbol(id)? {
                if let Some(ref mut cache) = *self.symbols_by_name.lock().unwrap() {
                    if let Some(list) = cache.get_mut(&symbol.name) {
                        list.retain(|x| x != id);
                        if list.is_empty() {
                            cache.remove(&symbol.name);
                        }
                    }
                }
            }
            batch.remove(id.as_bytes());
        }
        self.symbols.apply_batch(batch)?;

        // Remove the file index entries
        let prefix = format!("{}{}", path_str, IDX_SEP);
        let mut file_batch = sled::Batch::default();
        for item in self.symbols_by_file.scan_prefix(prefix.as_bytes()) {
            let (key, _) = item?;
            file_batch.remove(key.as_ref());
        }
        self.symbols_by_file.apply_batch(file_batch)?;

        for id in ids {
            self.delete_edges_for_symbol(&id)?;
        }

        Ok(())
    }

    /// Returns the symbol IDs associated with a given file path.
    pub fn list_symbol_ids_for_file(&self, path: &Path) -> Result<Vec<String>, StorageError> {
        let path_str = path.to_string_lossy().to_string();
        self.scan_prefix(&self.symbols_by_file, &path_str)
    }

    pub fn list_symbols(&self) -> Result<Vec<Symbol>, StorageError> {
        let mut list = Vec::new();
        for item in self.symbols.iter() {
            let (_, val) = item?;
            let symbol: Symbol = serde_json::from_slice(&val)?;
            list.push(symbol);
        }
        Ok(list)
    }

    pub fn get_symbols_by_file(&self, path: &Path) -> Result<Vec<Symbol>, StorageError> {
        let ids = self.list_symbol_ids_for_file(path)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(sym) = self.get_symbol(&id)? {
                out.push(sym);
            }
        }
        Ok(out)
    }

    fn load_name_cache(&self) -> Result<HashMap<String, Vec<String>>, StorageError> {
        let mut cache: HashMap<String, Vec<String>> = HashMap::new();
        for item in self.symbols.iter() {
            let (_, val) = item?;
            let symbol: Symbol = serde_json::from_slice(&val)?;
            cache.entry(symbol.name).or_default().push(symbol.id);
        }
        Ok(cache)
    }

    pub fn get_symbols_by_name(&self, name: &str) -> Result<Vec<Symbol>, StorageError> {
        let ids = {
            let mut cache = self.symbols_by_name.lock().unwrap();
            if cache.is_none() {
                *cache = Some(self.load_name_cache()?);
            }
            cache
                .as_ref()
                .and_then(|m| m.get(name))
                .cloned()
                .unwrap_or_default()
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(sym) = self.get_symbol(&id)? {
                out.push(sym);
            }
        }
        Ok(out)
    }

    // --- Chunk operations ---

    pub fn save_chunks_batch(&self, chunks: &[Chunk]) -> Result<(), StorageError> {
        let mut primary = sled::Batch::default();
        let mut by_symbol = sled::Batch::default();
        for chunk in chunks {
            primary.insert(chunk.id.as_bytes(), serde_json::to_vec(chunk)?);
            by_symbol.insert(
                Self::index_key(&chunk.parent_symbol_id, &chunk.id),
                &[] as &[u8],
            );
        }
        self.chunks.apply_batch(primary)?;
        self.chunks_by_symbol.apply_batch(by_symbol)?;
        Ok(())
    }

    pub fn get_chunk(&self, id: &str) -> Result<Option<Chunk>, StorageError> {
        self.chunks
            .get(id.as_bytes())?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(StorageError::from))
            .transpose()
    }

    pub fn list_chunks(&self) -> Result<Vec<Chunk>, StorageError> {
        let mut chunks = Vec::new();
        for item in self.chunks.iter() {
            let (_, value) = item?;
            chunks.push(serde_json::from_slice(&value)?);
        }
        Ok(chunks)
    }

    pub fn list_chunk_ids_for_symbol(&self, symbol_id: &str) -> Result<Vec<String>, StorageError> {
        self.scan_prefix(&self.chunks_by_symbol, symbol_id)
    }

    pub fn get_chunks_by_symbol(&self, symbol_id: &str) -> Result<Vec<Chunk>, StorageError> {
        let ids = self.list_chunk_ids_for_symbol(symbol_id)?;
        let mut chunks: Vec<Chunk> = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(value) = self.chunks.get(id.as_bytes())? {
                chunks.push(serde_json::from_slice(&value)?);
            }
        }
        chunks.sort_by_key(|chunk| chunk.chunk_index);
        Ok(chunks)
    }

    pub fn delete_chunks_for_symbol(&self, symbol_id: &str) -> Result<(), StorageError> {
        let ids = self.list_chunk_ids_for_symbol(symbol_id)?;
        let mut primary = sled::Batch::default();
        for id in &ids {
            primary.remove(id.as_bytes());
        }
        self.chunks.apply_batch(primary)?;

        let prefix = format!("{}{}", symbol_id, IDX_SEP);
        let mut secondary = sled::Batch::default();
        for item in self.chunks_by_symbol.scan_prefix(prefix.as_bytes()) {
            let (key, _) = item?;
            secondary.remove(key.as_ref());
        }
        self.chunks_by_symbol.apply_batch(secondary)?;
        Ok(())
    }

    pub fn get_edges_by_src(&self, src: &str) -> Result<Vec<Edge>, StorageError> {
        let keys = self.scan_prefix(&self.edges_by_src, src)?;
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(bytes) = self.edges.get(key.as_bytes())? {
                let edge: Edge = serde_json::from_slice(&bytes)?;
                out.push(edge);
            }
        }
        Ok(out)
    }

    pub fn get_edges_by_dst(&self, dst: &str) -> Result<Vec<Edge>, StorageError> {
        let keys = self.scan_prefix(&self.edges_by_dst, dst)?;
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(bytes) = self.edges.get(key.as_bytes())? {
                let edge: Edge = serde_json::from_slice(&bytes)?;
                out.push(edge);
            }
        }
        Ok(out)
    }

    // --- Edge operations ---

    pub fn save_edge(&self, edge: &Edge) -> Result<(), StorageError> {
        let key = Self::edge_key(edge);
        let value = serde_json::to_vec(edge)?;
        self.edges.insert(key.as_bytes(), value)?;
        self.edges_by_src
            .insert(Self::index_key(&edge.src, &key), &[])?;
        self.edges_by_dst
            .insert(Self::index_key(&edge.dst, &key), &[])?;
        Ok(())
    }

    pub fn save_edges_batch(&self, edges: &[Edge]) -> Result<(), StorageError> {
        let mut batch = sled::Batch::default();
        for edge in edges {
            let key = Self::edge_key(edge);
            let value = serde_json::to_vec(edge)?;
            batch.insert(key.as_bytes(), value);
        }
        self.edges.apply_batch(batch)?;

        let mut by_src_batch = sled::Batch::default();
        let mut by_dst_batch = sled::Batch::default();
        for edge in edges {
            let key = Self::edge_key(edge);
            by_src_batch.insert(Self::index_key(&edge.src, &key), &[] as &[u8]);
            by_dst_batch.insert(Self::index_key(&edge.dst, &key), &[] as &[u8]);
        }
        self.edges_by_src.apply_batch(by_src_batch)?;
        self.edges_by_dst.apply_batch(by_dst_batch)?;
        Ok(())
    }

    pub fn get_edge_by_key(&self, key: &str) -> Result<Option<Edge>, StorageError> {
        if let Some(bytes) = self.edges.get(key.as_bytes())? {
            let edge: Edge = serde_json::from_slice(&bytes)?;
            Ok(Some(edge))
        } else {
            Ok(None)
        }
    }

    pub fn list_edges(&self) -> Result<Vec<Edge>, StorageError> {
        let mut list = Vec::new();
        for item in self.edges.iter() {
            let (_, val) = item?;
            let edge: Edge = serde_json::from_slice(&val)?;
            list.push(edge);
        }
        Ok(list)
    }

    pub fn delete_edges_for_symbol(&self, symbol_id: &str) -> Result<(), StorageError> {
        let src_keys = self.scan_prefix(&self.edges_by_src, symbol_id)?;
        let dst_keys = self.scan_prefix(&self.edges_by_dst, symbol_id)?;
        let mut seen = std::collections::HashSet::new();
        let mut edges_to_remove: Vec<(String, Edge)> = Vec::new();
        for key in src_keys.iter().chain(dst_keys.iter()) {
            if !seen.insert(key.clone()) {
                continue;
            }
            if let Some(bytes) = self.edges.get(key.as_bytes())? {
                let edge: Edge = serde_json::from_slice(&bytes)?;
                edges_to_remove.push((key.clone(), edge));
            }
        }

        let mut batch = sled::Batch::default();
        for (key, _) in &edges_to_remove {
            batch.remove(key.as_bytes());
        }
        self.edges.apply_batch(batch)?;

        // Remove index entries for this symbol
        let src_prefix = format!("{}{}", symbol_id, IDX_SEP);
        let mut src_batch = sled::Batch::default();
        for item in self.edges_by_src.scan_prefix(src_prefix.as_bytes()) {
            let (key, _) = item?;
            src_batch.remove(key.as_ref());
        }
        self.edges_by_src.apply_batch(src_batch)?;

        let dst_prefix = format!("{}{}", symbol_id, IDX_SEP);
        let mut dst_batch = sled::Batch::default();
        for item in self.edges_by_dst.scan_prefix(dst_prefix.as_bytes()) {
            let (key, _) = item?;
            dst_batch.remove(key.as_ref());
        }
        self.edges_by_dst.apply_batch(dst_batch)?;

        // Clean up the other-end index entries for the removed edges
        for (key, edge) in &edges_to_remove {
            self.remove_index_key(&self.edges_by_src, &edge.src, key)?;
            self.remove_index_key(&self.edges_by_dst, &edge.dst, key)?;
        }

        Ok(())
    }

    /// Delete derived edges produced by one resolver while preserving parser
    /// and other analysis edges.
    pub fn delete_edges_with_tag(&self, tag: &str) -> Result<usize, StorageError> {
        let matching: Vec<Edge> = self
            .list_edges()?
            .into_iter()
            .filter(|edge| edge.tags.iter().any(|candidate| candidate == tag))
            .collect();
        for edge in &matching {
            let key = Self::edge_key(edge);
            self.edges.remove(key.as_bytes())?;
            self.remove_index_key(&self.edges_by_src, &edge.src, &key)?;
            self.remove_index_key(&self.edges_by_dst, &edge.dst, &key)?;
        }
        Ok(matching.len())
    }

    /// Clear all data in the storage trees.
    pub fn clear(&self) -> Result<(), StorageError> {
        self.metadata.clear()?;
        self.files.clear()?;
        self.symbols.clear()?;
        self.edges.clear()?;
        self.symbols_by_file.clear()?;
        self.chunks.clear()?;
        self.chunks_by_symbol.clear()?;
        self.symbols_by_name.lock().unwrap().take();
        self.edges_by_src.clear()?;
        self.edges_by_dst.clear()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_storage_operations() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("findex_db");
        let storage = Storage::open(db_path).unwrap();
        assert_eq!(storage.index_format_version().unwrap(), 0);
        storage.set_index_format_version(2).unwrap();
        assert_eq!(storage.index_format_version().unwrap(), 2);

        // 1. Test File Ingestion
        let file_path = Path::new("src/main.rs");
        let disc_file = DiscoveredFile {
            path: file_path.to_path_buf(),
            hash: [0u8; 32],
            size: 100,
        };
        storage.save_file(&disc_file).unwrap();

        let retrieved_file = storage.get_file(file_path).unwrap().unwrap();
        assert_eq!(retrieved_file, disc_file);

        // 2. Test Symbol Operations
        let symbol = Symbol {
            id: "src/main.rs#main".to_string(),
            name: "main".to_string(),
            kind: "Function".to_string(),
            signature: "fn main()".to_string(),
            file_path: "src/main.rs".to_string(),
            start_line: 1,
            start_col: 1,
            end_line: 3,
            end_col: 1,
            docstring: Some("Main entrypoint".to_string()),
            parent_id: None,
            children: vec![],
            language: "rust".to_string(),
            token_count: 3,
            ast_hash: None,
            qualified_name: None,
        };
        storage.save_symbol(&symbol).unwrap();

        let chunk = Chunk {
            id: "src/main.rs#main::chunk:0".to_string(),
            parent_symbol_id: symbol.id.clone(),
            file_path: symbol.file_path.clone(),
            chunk_index: 0,
            start_line: 1,
            end_line: 3,
            text: "fn main() {}".to_string(),
            token_count: 4,
        };
        storage
            .save_chunks_batch(std::slice::from_ref(&chunk))
            .unwrap();

        let retrieved_sym = storage.get_symbol("src/main.rs#main").unwrap().unwrap();
        assert_eq!(retrieved_sym, symbol);

        // 3. Test Edge Operations
        let edge = Edge {
            src: "src/main.rs#main".to_string(),
            dst: "src/utils.rs#help".to_string(),
            edge_type: EdgeType::Calls,
            ..Default::default()
        };
        storage.save_edge(&edge).unwrap();

        let edges = storage.list_edges().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0], edge);

        // 4. Test Deletion cascade
        storage.delete_file(file_path).unwrap();
        assert!(storage.get_file(file_path).unwrap().is_none());
        assert!(storage.get_symbol("src/main.rs#main").unwrap().is_none());
        assert!(storage.get_chunk(&chunk.id).unwrap().is_none());
        assert_eq!(storage.list_edges().unwrap().len(), 0);

        storage.clear().unwrap();
        assert_eq!(storage.index_format_version().unwrap(), 0);
    }
}
