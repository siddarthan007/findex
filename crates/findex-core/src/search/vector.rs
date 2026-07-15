use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use usearch::{new_index, Index, IndexOptions, MetricKind, ScalarKind};

#[derive(Error, Debug)]
pub enum VectorError {
    #[error("USearch error: {0}")]
    USearch(String),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Embedder error: {0}")]
    Embedder(String),
}

pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> Vec<f32>;
    fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        texts.iter().map(|text| self.embed(text)).collect()
    }
    fn dimension(&self) -> usize;
    /// Stable identity for persisted-index compatibility. Implementations must
    /// include every setting that can change generated vectors.
    fn fingerprint(&self) -> String {
        format!("embedder:{}", self.dimension())
    }
    /// Release heavyweight inference state after inactivity. Returns whether
    /// anything was released. Lightweight embedders can keep the default.
    fn release_idle_resources(&self, _idle_for: std::time::Duration) -> bool {
        false
    }
}

/// Quantization scheme for the vector index.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Quantization {
    /// Brain-float16 (default): good accuracy, ~2x compression.
    #[default]
    BF16,
    /// 8-bit integer quantization: ~4x compression.
    I8,
    /// 1-bit per dimension: ~32x compression.
    B1,
}

impl std::str::FromStr for Quantization {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bf16" => Ok(Quantization::BF16),
            "i8" | "int8" => Ok(Quantization::I8),
            "b1" | "binary" | "1bit" => Ok(Quantization::B1),
            other => Err(format!("unknown quantization scheme: {}", other)),
        }
    }
}

fn scalar_kind(q: Quantization) -> ScalarKind {
    match q {
        Quantization::BF16 => ScalarKind::BF16,
        Quantization::I8 => ScalarKind::I8,
        Quantization::B1 => ScalarKind::B1,
    }
}

/// A mock embedder that creates deterministic vectors based on text content hash
pub struct MockEmbedder {
    dimension: usize,
}

impl MockEmbedder {
    pub fn new(dimension: usize) -> Self {
        Self { dimension }
    }
}

impl Embedder for MockEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dimension];
        let hash = blake3::hash(text.as_bytes());
        let bytes = hash.as_bytes();

        // Fill dimensions deterministically using the blake3 hash bytes
        for (i, value) in vec.iter_mut().enumerate() {
            let byte_idx = (i * 7) % bytes.len();
            let sign = if bytes[byte_idx].is_multiple_of(2) {
                1.0
            } else {
                -1.0
            };
            *value = sign * ((bytes[byte_idx] as f32) / 255.0);
        }

        // L2 Normalize the vector
        let sq_sum: f32 = vec.iter().map(|x| x * x).sum();
        let norm = sq_sum.sqrt();
        if norm > 0.0 {
            for val in vec.iter_mut() {
                *val /= norm;
            }
        }

        vec
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

pub struct VectorIndex {
    index: Index,
    index_path: Option<PathBuf>,
    mapping_path: Option<PathBuf>,
    quantization: Quantization,
    fingerprint: String,
    // Mapping from u64 (USearch tag) -> String (Symbol ID)
    tag_to_id: HashMap<u64, String>,
    // Mapping from String (Symbol ID) -> u64 (USearch tag)
    id_to_tag: HashMap<String, u64>,
    next_tag: u64,
}

#[derive(Serialize, Deserialize)]
struct SavedMapping {
    tag_to_id: Vec<(u64, String)>,
    next_tag: u64,
    #[serde(default)]
    quantization: Quantization,
    /// Persisted explicitly because USearch can load an older graph whose
    /// dimensionality differs from the currently selected embedding model.
    #[serde(default)]
    dimension: usize,
    #[serde(default)]
    fingerprint: String,
}

impl VectorIndex {
    fn build_options(dimension: usize, quantization: Quantization) -> IndexOptions {
        IndexOptions {
            dimensions: dimension,
            metric: MetricKind::Cos,
            quantization: scalar_kind(quantization),
            connectivity: 16,
            // Lower expansion_add keeps ingestion fast while still producing a usable graph.
            // This can be raised for final production indexes at the cost of build time.
            expansion_add: 32,
            expansion_search: 64,
            multi: false,
        }
    }

    pub fn open_or_create<P: AsRef<Path>>(
        dir_path: P,
        dimension: usize,
    ) -> Result<Self, VectorError> {
        Self::open_or_create_with_quantization(dir_path, dimension, Quantization::default())
    }

    pub fn open_or_create_with_quantization<P: AsRef<Path>>(
        dir_path: P,
        dimension: usize,
        quantization: Quantization,
    ) -> Result<Self, VectorError> {
        Self::open_or_create_with_fingerprint(dir_path, dimension, quantization, "")
    }

    pub fn open_or_create_with_fingerprint<P: AsRef<Path>>(
        dir_path: P,
        dimension: usize,
        quantization: Quantization,
        fingerprint: &str,
    ) -> Result<Self, VectorError> {
        let dir = dir_path.as_ref();
        if !dir.exists() {
            std::fs::create_dir_all(dir).ok();
        }

        let index_path = dir.join("index.usearch");
        let mapping_path = dir.join("mapping.json");

        let options = Self::build_options(dimension, quantization);
        let index = new_index(&options)
            .map_err(|e| VectorError::USearch(format!("Failed to create index: {:?}", e)))?;

        let mut tag_to_id = HashMap::new();
        let mut id_to_tag = HashMap::new();
        let mut next_tag = 0;

        if index_path.exists() && mapping_path.exists() {
            let mapping_content = std::fs::read_to_string(&mapping_path)?;
            let saved: SavedMapping = serde_json::from_str(&mapping_content)?;
            if saved.dimension == dimension
                && saved.quantization == quantization
                && saved.fingerprint == fingerprint
            {
                index.load(index_path.to_str().unwrap_or("")).map_err(|e| {
                    VectorError::USearch(format!("Failed to load HNSW index: {:?}", e))
                })?;
                next_tag = saved.next_tag;
                for (tag, id) in saved.tag_to_id {
                    id_to_tag.insert(id.clone(), tag);
                    tag_to_id.insert(tag, id);
                }
            }
        }

        Ok(Self {
            index,
            index_path: Some(index_path),
            mapping_path: Some(mapping_path),
            quantization,
            fingerprint: fingerprint.to_string(),
            tag_to_id,
            id_to_tag,
            next_tag,
        })
    }

    pub fn create_in_ram(dimension: usize) -> Result<Self, VectorError> {
        Self::create_in_ram_with_quantization(dimension, Quantization::default())
    }

    pub fn create_in_ram_with_quantization(
        dimension: usize,
        quantization: Quantization,
    ) -> Result<Self, VectorError> {
        let options = Self::build_options(dimension, quantization);
        let index = new_index(&options)
            .map_err(|e| VectorError::USearch(format!("Failed to create RAM index: {:?}", e)))?;

        Ok(Self {
            index,
            index_path: None,
            mapping_path: None,
            quantization,
            fingerprint: String::new(),
            tag_to_id: HashMap::new(),
            id_to_tag: HashMap::new(),
            next_tag: 0,
        })
    }

    /// Clears all items in the index and the mappings.
    pub fn clear(&mut self) -> Result<(), VectorError> {
        // Unfortunately USearch index doesn't have a direct clear/reset API in the Rust SDK,
        // so we re-create the index and clear mappings.
        let options = Self::build_options(self.index.dimensions(), self.quantization);
        self.index = new_index(&options)
            .map_err(|e| VectorError::USearch(format!("Failed to recreate index: {:?}", e)))?;
        self.tag_to_id.clear();
        self.id_to_tag.clear();
        self.next_tag = 0;
        self.save().ok();
        Ok(())
    }

    /// Indexes a symbol and updates the internal mappings.
    /// If a vector for `symbol_id` already exists, it is removed first to guarantee
    /// unique tags and avoid USearch duplicate-key errors.
    pub fn add_symbol(
        &mut self,
        symbol_id: &str,
        text: &str,
        embedder: &dyn Embedder,
    ) -> Result<(), VectorError> {
        // Remove any existing vector for this symbol before re-adding.
        if self.id_to_tag.contains_key(symbol_id) {
            self.remove_symbol(symbol_id)?;
        }

        let vec = embedder.embed(text);
        self.add_embedding(symbol_id, &vec)
    }

    fn add_embedding(&mut self, symbol_id: &str, vector: &[f32]) -> Result<(), VectorError> {
        if self.id_to_tag.contains_key(symbol_id) {
            self.remove_symbol(symbol_id)?;
        }

        // Reserve capacity dynamically if needed
        let current_size = self.index.size();
        if current_size >= self.index.capacity() {
            self.index
                .reserve(current_size + 128)
                .map_err(|e| VectorError::USearch(format!("Reserve capacity failed: {:?}", e)))?;
        }

        let tag = self.next_tag;
        self.next_tag += 1;
        self.id_to_tag.insert(symbol_id.to_string(), tag);
        self.tag_to_id.insert(tag, symbol_id.to_string());

        self.index
            .add(tag, vector)
            .map_err(|e| VectorError::USearch(format!("Failed to add vector: {:?}", e)))?;

        Ok(())
    }

    /// Add multiple documents with one embedder batch. Batch size is bounded
    /// by `FINDEX_EMBEDDING_BATCH_SIZE` (default 32) to stay within local VRAM.
    pub fn add_symbols_batch(
        &mut self,
        symbols: &[(&str, &str)],
        embedder: &dyn Embedder,
    ) -> Result<(), VectorError> {
        let batch_size = std::env::var("FINDEX_EMBEDDING_BATCH")
            .or_else(|_| std::env::var("FINDEX_EMBEDDING_BATCH_SIZE"))
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(32)
            .min(256);
        for batch in symbols.chunks(batch_size) {
            let texts: Vec<&str> = batch.iter().map(|(_, text)| *text).collect();
            let embeddings = embedder.embed_batch(&texts);
            if embeddings.len() != batch.len() {
                return Err(VectorError::Embedder(format!(
                    "embedder returned {} vectors for {} inputs",
                    embeddings.len(),
                    batch.len()
                )));
            }
            for ((symbol_id, _), embedding) in batch.iter().zip(embeddings.iter()) {
                self.add_embedding(symbol_id, embedding)?;
            }
        }
        Ok(())
    }

    /// Removes a symbol from the vector index and mappings.
    pub fn remove_symbol(&mut self, symbol_id: &str) -> Result<(), VectorError> {
        if let Some(&tag) = self.id_to_tag.get(symbol_id) {
            self.index
                .remove(tag)
                .map_err(|e| VectorError::USearch(format!("Failed to remove vector: {:?}", e)))?;
            self.id_to_tag.remove(symbol_id);
            self.tag_to_id.remove(&tag);
        }
        Ok(())
    }

    /// Incrementally updates the vector index: removes deleted symbols and adds/updates new ones.
    pub fn update_symbols(
        &mut self,
        added: &[(crate::storage::Symbol, String)],
        deleted_ids: &[String],
        embedder: &dyn Embedder,
    ) -> Result<(), VectorError> {
        for id in deleted_ids {
            self.remove_symbol(id)?;
        }
        let batch: Vec<(&str, &str)> = added
            .iter()
            .map(|(symbol, body)| (symbol.id.as_str(), body.as_str()))
            .collect();
        self.add_symbols_batch(&batch, embedder)?;
        Ok(())
    }

    /// Returns the number of vectors currently stored in the index.
    pub fn size(&self) -> usize {
        self.index.size()
    }

    /// Persists index and mappings to disk.
    pub fn save(&self) -> Result<(), VectorError> {
        if let (Some(idx_p), Some(map_p)) = (&self.index_path, &self.mapping_path) {
            self.index
                .save(idx_p.to_str().unwrap_or(""))
                .map_err(|e| VectorError::USearch(format!("Failed to save HNSW: {:?}", e)))?;

            let saved = SavedMapping {
                tag_to_id: self.tag_to_id.clone().into_iter().collect(),
                next_tag: self.next_tag,
                quantization: self.quantization,
                dimension: self.index.dimensions(),
                fingerprint: self.fingerprint.clone(),
            };
            let content = serde_json::to_string(&saved)?;
            std::fs::write(map_p, content)?;
        }
        Ok(())
    }

    /// Queries the vector index and returns a list of matching symbol IDs and their cosine distances.
    pub fn search(
        &self,
        query_str: &str,
        limit: usize,
        embedder: &dyn Embedder,
    ) -> Result<Vec<(String, f32)>, VectorError> {
        let vec = embedder.embed(query_str);

        let matches = self
            .index
            .search(&vec, limit)
            .map_err(|e| VectorError::USearch(format!("Failed to query index: {:?}", e)))?;

        let mut results = Vec::new();
        for (tag, dist) in matches.keys.into_iter().zip(matches.distances) {
            if let Some(symbol_id) = self.tag_to_id.get(&tag) {
                // USearch returns cosine distance (lower is better); expose a
                // conventional similarity score so downstream reranking can
                // consistently treat higher values as better.
                results.push((symbol_id.clone(), 1.0 - dist));
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_search() {
        let embedder = MockEmbedder::new(128);
        let mut index = VectorIndex::create_in_ram(128).unwrap();

        index
            .add_symbol("sym1", "fn test_function() {}", &embedder)
            .unwrap();
        index
            .add_symbol("sym2", "class ClassRunner {}", &embedder)
            .unwrap();

        let res = index.search("test_function", 10, &embedder).unwrap();
        assert_eq!(res.len(), 2); // returns both sorted by distance
        assert_eq!(res[0].0, "sym1"); // first match is exact name match
    }

    #[test]
    fn incompatible_persisted_dimension_starts_empty() {
        let directory = tempfile::tempdir().unwrap();
        let embedder = MockEmbedder::new(32);
        let mut original = VectorIndex::open_or_create(directory.path(), 32).unwrap();
        original.add_symbol("symbol", "code", &embedder).unwrap();
        original.save().unwrap();

        let replacement = VectorIndex::open_or_create(directory.path(), 64).unwrap();
        assert_eq!(replacement.size(), 0);
        assert_eq!(replacement.index.dimensions(), 64);
    }

    #[test]
    fn incompatible_embedding_fingerprint_starts_empty() {
        let directory = tempfile::tempdir().unwrap();
        let embedder = MockEmbedder::new(32);
        let mut original = VectorIndex::open_or_create_with_fingerprint(
            directory.path(),
            32,
            Quantization::BF16,
            "model-a",
        )
        .unwrap();
        original.add_symbol("symbol", "code", &embedder).unwrap();
        original.save().unwrap();

        let replacement = VectorIndex::open_or_create_with_fingerprint(
            directory.path(),
            32,
            Quantization::BF16,
            "model-b",
        )
        .unwrap();
        assert_eq!(replacement.size(), 0);
    }
}
