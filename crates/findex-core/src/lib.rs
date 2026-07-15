pub mod discovery;
pub mod graph_pruning;
pub mod graph_query;
pub mod intelligence;
pub mod mcp;
pub mod mcp_http;
mod mcp_tasks;
pub mod merkle;
pub mod models;
pub mod parser;
pub mod resolver;
pub mod runtime;
pub mod search;
pub mod semantic_diff;
pub mod skeleton;
#[cfg(feature = "stack-graphs")]
pub mod stack_graphs;
pub mod storage;
pub mod structural_locality;
pub mod taint;
pub mod token_budget;
pub mod updater;
pub mod vfs;
pub mod watch;

use rayon::prelude::*;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;
use thiserror::Error;

use crate::discovery::{discover_files, DiscoveredFile};
use crate::parser::{parse_code, ParserError};
use crate::search::chunker::chunk_symbol;
use crate::search::hybrid::rrf_merge;
use crate::search::lexical::{LexicalError, LexicalIndex};
use crate::search::local_embedder::create_embedder;
use crate::search::mmr::mmr_diversify;
use crate::search::vector::{Quantization, VectorError, VectorIndex};
use crate::skeleton::generate_skeleton;
use crate::skeleton::pagerank::{
    compute_pagerank, compute_personalized_pagerank, PersonalizationConfig,
};
use crate::storage::{Edge, Storage, StorageError, Symbol};

/// Bump when persisted symbols/chunks require a one-time source reparse.
const INDEX_FORMAT_VERSION: u32 = 3;

type ParsedFile = (DiscoveredFile, Vec<Symbol>, Vec<Edge>, Vec<String>, usize);
type ParseResult = Result<ParsedFile, IngestionError>;

/// Resolve the vector quantization scheme from the environment.
fn vector_quantization() -> Quantization {
    std::env::var("FINDEX_VECTOR_QUANTIZATION")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_default()
}

#[derive(Error, Debug)]
pub enum IngestionError {
    #[error("Discovery error: {0}")]
    Discovery(#[from] discovery::DiscoveryError),
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("Parser error: {0}")]
    Parser(#[from] ParserError),
    #[error("Lexical index error: {0}")]
    Lexical(#[from] LexicalError),
    #[error("Vector index error: {0}")]
    Vector(#[from] VectorError),
    #[error("Reranker error: {0}")]
    Reranker(String),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestionStats {
    pub total_files: usize,
    pub parsed_files: usize,
    pub deleted_files: usize,
    pub total_bytes: u64,
    pub total_lines: usize,
    pub duration_ms: u64,
    /// Root digest of the persisted repository Merkle tree.
    #[serde(default)]
    pub merkle_root: String,
    /// Number of Merkle nodes visited to identify the changed subtrees.
    #[serde(default)]
    pub merkle_nodes_visited: usize,
    #[serde(default)]
    pub stack_graph_edges: usize,
}

/// Options controlling which indexes are built during ingestion.
#[derive(Debug, Clone, Copy)]
pub struct IngestionOptions {
    /// Build the Tantivy lexical index.
    pub build_lexical_index: bool,
    /// Build the USearch vector index.
    pub build_vector_index: bool,
}

impl Default for IngestionOptions {
    fn default() -> Self {
        // By default build the symbol graph and lexical index, but leave the
        // USearch vector index lazy: it is built on first semantic/hybrid query.
        Self {
            build_lexical_index: true,
            build_vector_index: false,
        }
    }
}

/// Orchestrates the ingestion of a codebase.
/// Builds the symbol graph and lexical index. The USearch vector index is lazy:
/// it is constructed on first semantic/hybrid search to keep ingestion fast.
pub fn ingest_codebase<P: AsRef<Path>, D: AsRef<Path>>(
    root_dir: P,
    db_dir: D,
    storage: &Storage,
) -> Result<IngestionStats, IngestionError> {
    ingest_codebase_with_options(root_dir, db_dir, storage, IngestionOptions::default())
}

/// Full ingestion including the USearch vector index. Use when you want all
/// indexes ready immediately (e.g., for benchmarks or server warm-up).
pub fn ingest_codebase_full<P: AsRef<Path>, D: AsRef<Path>>(
    root_dir: P,
    db_dir: D,
    storage: &Storage,
) -> Result<IngestionStats, IngestionError> {
    ingest_codebase_with_options(
        root_dir,
        db_dir,
        storage,
        IngestionOptions {
            build_lexical_index: true,
            build_vector_index: true,
        },
    )
}

/// Phase-0 ingestion: builds the symbol graph and storage only. Lexical and
/// vector indexes are Phase-1 retrieval concerns and are skipped here.
pub fn ingest_codebase_phase0<P: AsRef<Path>, D: AsRef<Path>>(
    root_dir: P,
    db_dir: D,
    storage: &Storage,
) -> Result<IngestionStats, IngestionError> {
    ingest_codebase_with_options(
        root_dir,
        db_dir,
        storage,
        IngestionOptions {
            build_lexical_index: false,
            build_vector_index: false,
        },
    )
}

/// Builds (or rebuilds) the USearch vector index from all symbols currently in storage.
pub fn build_vector_index<P: AsRef<Path>>(
    db_dir: P,
    storage: &Storage,
) -> Result<(), IngestionError> {
    let embedder = create_embedder(128);
    build_vector_index_with_embedder(db_dir, storage, embedder.as_ref())
}

/// Rebuild the Tantivy lexical index from primary symbol storage.
pub fn build_lexical_index<P: AsRef<Path>>(
    db_dir: P,
    storage: &Storage,
) -> Result<(), IngestionError> {
    let documents = stored_retrieval_documents(storage)?;
    let (symbols, bodies): (Vec<_>, Vec<_>) = documents.into_iter().unzip();

    let index = LexicalIndex::open_or_create(db_dir.as_ref().join("lexical"))?;
    index.index_symbols(&symbols, &bodies)?;
    Ok(())
}

/// Rebuild the vector index with an already-loaded embedder. Long-lived
/// callers should prefer this to avoid reloading ONNX sessions.
pub fn build_vector_index_with_embedder<P: AsRef<Path>>(
    db_dir: P,
    storage: &Storage,
    embedder: &dyn crate::search::vector::Embedder,
) -> Result<(), IngestionError> {
    let db_path = db_dir.as_ref();
    let retrieval_documents = stored_retrieval_documents(storage)?;

    let vector_dir = db_path.join("vector");
    let dimension = embedder.dimension();
    let mut vector_index = VectorIndex::open_or_create_with_fingerprint(
        vector_dir,
        dimension,
        vector_quantization(),
        &embedder.fingerprint(),
    )?;
    vector_index.clear()?;
    let documents: Vec<(&str, &str)> = retrieval_documents
        .iter()
        .map(|(symbol, body)| (symbol.id.as_str(), body.as_str()))
        .collect();
    vector_index.add_symbols_batch(&documents, embedder)?;
    vector_index.save()?;
    Ok(())
}

fn ingest_codebase_with_options<P: AsRef<Path>, D: AsRef<Path>>(
    root_dir: P,
    db_dir: D,
    storage: &Storage,
    options: IngestionOptions,
) -> Result<IngestionStats, IngestionError> {
    let start_time = Instant::now();
    let root = root_dir.as_ref();
    let db_path = db_dir.as_ref();

    // 1. Discover all files on disk
    let discovered = discover_files(root)?;
    let total_files = discovered.len();

    // 2. Compare the repository Merkle roots. The recursive comparison stops
    // at equal subtrees and returns relative paths only for changed leaves.
    let indexed = storage.list_files()?;
    let requires_format_migration = storage.index_format_version()? < INDEX_FORMAT_VERSION;

    let current_merkle = crate::merkle::MerkleSnapshot::from_files(root, &discovered);
    let previous_merkle = storage.get_metadata::<crate::merkle::MerkleSnapshot>("merkle:v1")?;
    let merkle_diff = previous_merkle
        .as_ref()
        .map(|previous| previous.diff(&current_merkle));
    let merkle_nodes_visited = merkle_diff.as_ref().map_or(
        current_merkle.root.children.len().saturating_add(1),
        |diff| diff.visited_nodes,
    );
    let merkle_root = current_merkle.root_hash_hex();

    // Group indexed files by path for fast lookup
    let indexed_map: std::collections::HashMap<PathBuf, DiscoveredFile> =
        indexed.into_iter().map(|f| (f.path.clone(), f)).collect();

    // Build a reverse index of existing symbols by file path so we can delete
    // old symbols without a full scan per file.
    let existing_symbols = storage.list_symbols()?;
    let mut symbols_by_file: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for sym in &existing_symbols {
        symbols_by_file
            .entry(sym.file_path.clone())
            .or_default()
            .push(sym.id.clone());
    }

    let mut to_parse = Vec::new();
    let mut current_paths = std::collections::HashSet::new();

    let changed_absolute: std::collections::HashSet<PathBuf> = merkle_diff
        .as_ref()
        .map(|diff| diff.changed.iter().map(|path| root.join(path)).collect())
        .unwrap_or_default();

    // Check for added or modified files. Legacy databases without a snapshot
    // fall back to the leaf hash comparison for the one migration run.
    for file in &discovered {
        current_paths.insert(file.path.clone());
        if requires_format_migration {
            to_parse.push(file.clone());
        } else if merkle_diff.is_some() {
            if changed_absolute.contains(&file.path) {
                // Modified file
                to_parse.push(file.clone());
            }
        } else if let Some(existing) = indexed_map.get(&file.path) {
            if existing.hash != file.hash {
                to_parse.push(file.clone());
            }
        } else {
            // New file
            to_parse.push(file.clone());
        }
    }

    let parsed_files = to_parse.len();
    let mut deleted_files = 0;
    let mut total_bytes = 0;
    let mut total_lines = 0;

    // Count deleted files before deciding on the fast path.
    for path in indexed_map.keys() {
        if !current_paths.contains(path) {
            deleted_files += 1;
        }
    }

    // Fast path: nothing changed, skip all parsing and indexing.
    if to_parse.is_empty() && deleted_files == 0 && !requires_format_migration {
        storage.set_metadata("index:root", &root.to_string_lossy().to_string())?;
        return Ok(IngestionStats {
            total_files,
            parsed_files: 0,
            deleted_files: 0,
            total_bytes: 0,
            total_lines: 0,
            duration_ms: 0,
            merkle_root,
            merkle_nodes_visited,
            stack_graph_edges: storage
                .get_metadata::<crate::stack_graphs::StackGraphStats>("stack-graphs:last")?
                .map_or(0, |stats| stats.resolved_edges),
        });
    }

    // 3. Parse changed files in parallel using Rayon
    let parse_results: Vec<ParseResult> = to_parse
        .into_par_iter()
        .map(|file_info| {
            // Memory-map the file and borrow its contents as &str for zero-copy parsing.
            // The Mmap is kept alive for the duration of this closure so that AST nodes
            // and symbol bodies can reference the source text.
            let file = File::open(&file_info.path)?;
            // SAFETY: memmap2 is safe for read-only maps as long as the file is not
            // truncated or modified concurrently by another process. We only read the
            // mapped bytes and drop the mapping before writing to the same path.
            let mmap = unsafe { memmap2::Mmap::map(&file)? };
            let content = if file_info.size == 0 {
                std::borrow::Cow::Borrowed("")
            } else {
                String::from_utf8_lossy(&mmap)
            };
            let content_ref: &str = &content;

            let line_count = content_ref.lines().count();

            // Parse symbols and edges
            let (symbols, edges) = match parse_code(&file_info.path, content_ref) {
                Ok(res) => res,
                Err(ParserError::Unsupported(_)) => {
                    // Gracefully skip unsupported languages, returning empty symbols
                    (Vec::new(), Vec::new())
                }
                Err(e) => return Err(IngestionError::Parser(e)),
            };

            // Extract symbol bodies only when retrieval indexes/chunks need them.
            // Phase-0 ingestion skips this to preserve its throughput gate.
            let mut bodies = Vec::new();
            if options.build_lexical_index || options.build_vector_index {
                for sym in &symbols {
                    bodies.push(extract_symbol_body(
                        content_ref,
                        sym.start_line,
                        sym.end_line,
                    ));
                }
            }

            Ok((file_info, symbols, edges, bodies, line_count))
        })
        .collect();

    // 4. Save results to Sled database sequentially and collect incremental index deltas.
    let mut deleted_symbol_ids: Vec<String> = Vec::new();
    let mut added_symbols_with_bodies: Vec<(Symbol, String)> = Vec::new();
    let mut new_symbols = Vec::new();
    let mut new_edges = Vec::new();
    let mut new_chunks = Vec::new();
    let mut files_to_save = Vec::new();

    for res in parse_results {
        let (file_info, symbols, edges, bodies, line_count) = res?;
        total_bytes += file_info.size;
        total_lines += line_count;

        files_to_save.push(file_info.clone());

        // Mark old symbols from this file as deleted.
        let path_str = file_info.path.to_string_lossy().to_string();
        if let Some(old_ids) = symbols_by_file.remove(&path_str) {
            collect_deleted_document_ids(storage, &old_ids, &mut deleted_symbol_ids)?;
        }

        // Split large symbols at code-shaped boundaries and index/store chunks.
        let mut file_chunks = Vec::new();
        for (sym, body) in symbols.iter().zip(bodies.iter()) {
            for chunk in chunk_symbol(sym, body, 256) {
                let mut document = sym.clone();
                document.id = chunk.id.clone();
                document.start_line = chunk.start_line;
                document.end_line = chunk.end_line;
                if chunk.id != sym.id {
                    document.parent_id = Some(sym.id.clone());
                }
                added_symbols_with_bodies.push((document, retrieval_text(sym, &chunk.text)));
                file_chunks.push(chunk);
            }
        }
        new_chunks.extend(file_chunks);
        new_symbols.extend(symbols);
        new_edges.extend(edges);
    }

    // Collect symbol IDs from deleted files and remove their file records.
    for path in indexed_map.keys() {
        if !current_paths.contains(path) {
            let path_str = path.to_string_lossy().to_string();
            if let Some(old_ids) = symbols_by_file.remove(&path_str) {
                collect_deleted_document_ids(storage, &old_ids, &mut deleted_symbol_ids)?;
            }
            storage.delete_file(path)?;
        }
    }

    // Persist file records, symbols, and edges in batches.
    for file_info in files_to_save {
        // Remove the previous primary records and their outgoing/containment
        // edges before writing the new parse. Merely deleting search-index
        // documents leaves stale symbols behind when line-based IDs change.
        storage.delete_symbols_for_file(&file_info.path)?;
        storage.save_file(&file_info)?;
    }
    storage.save_symbols_batch(&new_symbols)?;
    storage.save_chunks_batch(&new_chunks)?;
    storage.save_edges_batch(&new_edges)?;

    // 5. Incrementally update Tantivy and USearch indexes.
    if options.build_lexical_index {
        let lexical_dir = db_path.join("lexical");
        let lexical_index = LexicalIndex::open_or_create(lexical_dir)?;
        lexical_index.update_symbols(&added_symbols_with_bodies, &deleted_symbol_ids)?;
    }

    if options.build_vector_index {
        let vector_dir = db_path.join("vector");
        let embedder = create_embedder(128);
        let dimension = embedder.dimension();
        let mut vector_index = VectorIndex::open_or_create_with_quantization(
            vector_dir,
            dimension,
            vector_quantization(),
        )?;
        vector_index.update_symbols(
            &added_symbols_with_bodies,
            &deleted_symbol_ids,
            embedder.as_ref(),
        )?;
        vector_index.save()?;
    }

    #[cfg(feature = "stack-graphs")]
    let stack_graph_edges = {
        let stats =
            crate::stack_graphs::resolve_into_storage(root, storage).unwrap_or_else(|error| {
                crate::stack_graphs::StackGraphStats {
                    enabled: true,
                    message: error,
                    ..Default::default()
                }
            });
        let count = stats.resolved_edges;
        storage.set_metadata("stack-graphs:last", &stats)?;
        count
    };
    #[cfg(not(feature = "stack-graphs"))]
    let stack_graph_edges = 0;

    // Mark the migration complete only after primary and retrieval indexes
    // have been updated successfully. An interrupted run will retry safely.
    storage.set_index_format_version(INDEX_FORMAT_VERSION)?;
    storage.set_metadata("merkle:v1", &current_merkle)?;
    storage.set_metadata("index:root", &root.to_string_lossy().to_string())?;

    let duration_ms = start_time.elapsed().as_millis() as u64;

    Ok(IngestionStats {
        total_files,
        parsed_files,
        deleted_files,
        total_bytes,
        total_lines,
        duration_ms,
        merkle_root,
        merkle_nodes_visited,
        stack_graph_edges,
    })
}

/// Helper function to slice symbol body lines from file contents
fn extract_symbol_body(content: &str, start_line: usize, end_line: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if start_line > 0 && start_line <= lines.len() {
        let end = end_line.min(lines.len());
        lines[(start_line - 1)..end].join("\n")
    } else {
        String::new()
    }
}

fn retrieval_text(symbol: &Symbol, body: &str) -> String {
    match symbol.docstring.as_deref() {
        Some(docstring) if !docstring.is_empty() => {
            format!("{}\n{}\n{}", symbol.signature, docstring, body)
        }
        _ => format!("{}\n{}", symbol.signature, body),
    }
}

fn collect_deleted_document_ids(
    storage: &Storage,
    symbol_ids: &[String],
    deleted: &mut Vec<String>,
) -> Result<(), StorageError> {
    for symbol_id in symbol_ids {
        deleted.extend(storage.list_chunk_ids_for_symbol(symbol_id)?);
        deleted.push(symbol_id.clone());
    }
    deleted.sort();
    deleted.dedup();
    Ok(())
}

fn stored_retrieval_documents(storage: &Storage) -> Result<Vec<(Symbol, String)>, IngestionError> {
    let chunks = storage.list_chunks()?;
    if !chunks.is_empty() {
        let mut documents = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            if let Some(parent) = storage.get_symbol(&chunk.parent_symbol_id)? {
                let mut document = parent.clone();
                document.id = chunk.id;
                document.start_line = chunk.start_line;
                document.end_line = chunk.end_line;
                if document.id != parent.id {
                    document.parent_id = Some(parent.id.clone());
                }
                documents.push((document, retrieval_text(&parent, &chunk.text)));
            }
        }
        return Ok(documents);
    }

    let symbols = storage.list_symbols()?;
    let mut documents = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        let path = Path::new(&symbol.file_path);
        let body = if path.exists() {
            let file = File::open(path)?;
            // SAFETY: this read-only map is held only while extracting text;
            // Findex never mutates the source file through the mapping.
            let mmap = unsafe { memmap2::Mmap::map(&file)? };
            let content = String::from_utf8_lossy(&mmap);
            extract_symbol_body(&content, symbol.start_line, symbol.end_line)
        } else {
            String::new()
        };
        documents.push((symbol.clone(), retrieval_text(&symbol, &body)));
    }
    Ok(documents)
}

/// Ensures the USearch vector index is populated. Builds it lazily from storage
/// when a semantic/hybrid query is issued and the index is empty.
fn ensure_vector_index(
    db_path: &Path,
    storage: &Storage,
    embedder: &dyn crate::search::vector::Embedder,
) -> Result<VectorIndex, IngestionError> {
    let vector_dir = db_path.join("vector");
    let dimension = embedder.dimension();
    let fingerprint = embedder.fingerprint();
    let vector_index = VectorIndex::open_or_create_with_fingerprint(
        &vector_dir,
        dimension,
        vector_quantization(),
        &fingerprint,
    )?;
    let chunk_count = storage.list_chunks()?.len();
    let document_count = if chunk_count > 0 {
        chunk_count
    } else {
        storage.list_symbols()?.len()
    };
    if vector_index.size() != document_count {
        build_vector_index_with_embedder(db_path, storage, embedder)?;
        Ok(VectorIndex::open_or_create_with_fingerprint(
            &vector_dir,
            dimension,
            vector_quantization(),
            &fingerprint,
        )?)
    } else {
        Ok(vector_index)
    }
}

fn ensure_lexical_index(db_path: &Path, storage: &Storage) -> Result<LexicalIndex, IngestionError> {
    let lexical_dir = db_path.join("lexical");
    let index = LexicalIndex::open_or_create(&lexical_dir)?;
    let chunk_count = storage.list_chunks()?.len();
    let document_count = if chunk_count > 0 {
        chunk_count
    } else {
        storage.list_symbols()?.len()
    } as u64;
    if index.num_docs()? != document_count {
        build_lexical_index(db_path, storage)?;
        Ok(LexicalIndex::open_or_create(lexical_dir)?)
    } else {
        Ok(index)
    }
}

/// Performs a search over the codebase using lexical, semantic, or hybrid ranking.
pub fn search_codebase<P: AsRef<Path>>(
    db_dir: P,
    storage: &Storage,
    query: &str,
    mode: &str,
    reranker: Option<&dyn search::rerank::Reranker>,
    limit: usize,
) -> Result<Vec<(Symbol, f32)>, IngestionError> {
    let embedder = create_embedder(128);
    search_codebase_with_components(
        db_dir,
        storage,
        query,
        mode,
        reranker,
        embedder.as_ref(),
        limit,
    )
}

/// Search with preloaded model components. MCP, TUI, and desktop callers use
/// this entry point so repeated queries share their ONNX sessions.
pub fn search_codebase_with_components<P: AsRef<Path>>(
    db_dir: P,
    storage: &Storage,
    query: &str,
    mode: &str,
    reranker: Option<&dyn search::rerank::Reranker>,
    embedder: &dyn crate::search::vector::Embedder,
    limit: usize,
) -> Result<Vec<(Symbol, f32)>, IngestionError> {
    let db_path = db_dir.as_ref();
    let limit = limit.max(1);

    // If reranker is present, fetch more candidates (up to 50) for stage 2
    let stage1_limit = if reranker.is_some() {
        limit.max(rerank_candidate_limit())
    } else {
        limit
    };

    let ranked_ids = match mode {
        "lexical" => {
            let lexical_index = ensure_lexical_index(db_path, storage)?;
            lexical_index.search(query, stage1_limit)?
        }
        "semantic" => {
            let vector_index = ensure_vector_index(db_path, storage, embedder)?;
            vector_index.search(query, stage1_limit, embedder)?
        }
        _ => {
            // The two independent retrieval legs run concurrently. This keeps
            // semantic inference from serializing the Tantivy lookup.
            let (lex_results, vec_results) = rayon::join(
                || -> Result<_, IngestionError> {
                    let lexical_index = ensure_lexical_index(db_path, storage)?;
                    Ok(lexical_index.search(query, 50)?)
                },
                || -> Result<_, IngestionError> {
                    let vector_index = ensure_vector_index(db_path, storage, embedder)?;
                    Ok(vector_index.search(query, 50, embedder)?)
                },
            );
            let lex_results = lex_results?;
            let vec_results = vec_results?;

            let lex_ids: Vec<String> = lex_results.into_iter().map(|(id, _)| id).collect();
            let vec_ids: Vec<String> = vec_results.into_iter().map(|(id, _)| id).collect();

            rrf_merge(&lex_ids, &vec_ids, stage1_limit)
        }
    };

    // Resolve symbol/chunk IDs back to parent Symbol structures. Multiple
    // matching chunks from one parent collapse to its best-scoring range.
    let mut candidates_by_symbol: std::collections::HashMap<String, (Symbol, f32)> =
        std::collections::HashMap::new();
    for (id, score) in ranked_ids {
        if let Some(sym) = storage.get_symbol(&id)? {
            candidates_by_symbol
                .entry(sym.id.clone())
                .and_modify(|existing| existing.1 = existing.1.max(score))
                .or_insert((sym, score));
        } else if let Some(chunk) = storage.get_chunk(&id)? {
            if let Some(mut parent) = storage.get_symbol(&chunk.parent_symbol_id)? {
                parent.start_line = chunk.start_line;
                parent.end_line = chunk.end_line;
                candidates_by_symbol
                    .entry(parent.id.clone())
                    .and_modify(|existing| {
                        if score > existing.1 {
                            *existing = (parent.clone(), score);
                        }
                    })
                    .or_insert((parent, score));
            }
        }
    }
    let mut candidates: Vec<(Symbol, f32)> = candidates_by_symbol.into_values().collect();
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Stage 2: Rerank
    let results = if let Some(r) = reranker {
        r.rerank(query, &candidates)?
    } else {
        candidates
    };

    // Stage 3: expand the highest-ranked seeds with direct structural context.
    // Related results are discounted so retrieval hits remain dominant.
    let mut expanded_by_id: std::collections::HashMap<String, (Symbol, f32)> =
        std::collections::HashMap::new();
    for (symbol, score) in &results {
        expanded_by_id
            .entry(symbol.id.clone())
            .and_modify(|existing| existing.1 = existing.1.max(*score))
            .or_insert_with(|| (symbol.clone(), *score));
    }

    for (seed, score) in results.iter().take(5) {
        for related in crate::resolver::expand_context(&seed.id, 1, storage)? {
            if related.id == seed.id {
                continue;
            }
            let discounted = *score * 0.5;
            expanded_by_id
                .entry(related.id.clone())
                .and_modify(|existing| existing.1 = existing.1.max(discounted))
                .or_insert((related, discounted));
        }
    }

    let mut expanded: Vec<(Symbol, f32)> = expanded_by_id.into_values().collect();
    expanded.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(mmr_diversify(&expanded, limit, 0.75))
}

fn rerank_candidate_limit() -> usize {
    std::env::var("FINDEX_RERANK_CANDIDATES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| match crate::models::model_profile() {
            crate::models::ModelProfile::Fast => 24,
            crate::models::ModelProfile::Balanced => 16,
            crate::models::ModelProfile::Quality => 12,
        })
        .clamp(4, 100)
}

/// Computes PageRank and builds the elided code skeleton for the ingested repository.
pub fn get_codebase_skeleton(
    storage: &Storage,
    token_budget: usize,
) -> Result<String, IngestionError> {
    let symbols = storage.list_symbols()?;
    let edges = storage.list_edges()?;

    let pageranks = compute_pagerank(&symbols, &edges);
    let skeleton = generate_skeleton(&symbols, &pageranks, token_budget);

    Ok(skeleton)
}

/// Build a repository map biased toward symbols/files in the active task.
pub fn get_codebase_skeleton_with_personalization(
    storage: &Storage,
    token_budget: usize,
    personalization: &PersonalizationConfig,
) -> Result<String, IngestionError> {
    let symbols = storage.list_symbols()?;
    let edges = storage.list_edges()?;
    let pageranks = compute_personalized_pagerank(&symbols, &edges, personalization);
    Ok(generate_skeleton(&symbols, &pageranks, token_budget))
}

/// Build a token-budgeted skeleton for a single file in source order.
pub fn get_file_skeleton(
    storage: &Storage,
    path: &Path,
    token_budget: usize,
) -> Result<String, IngestionError> {
    let symbols = storage.get_symbols_by_file(path)?;
    let skeleton = crate::skeleton::get_file_skeleton(&symbols, token_budget);
    Ok(skeleton)
}

/// Compute a tree-structural diff between two files of the same registered
/// tree-sitter language.
pub fn semantic_diff_files<P: AsRef<Path>, Q: AsRef<Path>>(
    old_path: P,
    new_path: Q,
) -> Result<crate::semantic_diff::SemanticDiff, IngestionError> {
    let old_path = old_path.as_ref();
    let new_path = new_path.as_ref();
    let old_ext = old_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    let new_ext = new_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    if old_ext != new_ext {
        return Err(ParserError::Unsupported(format!(
            "semantic diff requires matching extensions ({} vs {})",
            old_path.display(),
            new_path.display()
        ))
        .into());
    }
    let config = crate::parser::registry::config_for_extension(old_ext).ok_or_else(|| {
        ParserError::Unsupported(format!(
            "semantic diff is not available for .{} files; use a tree-sitter-backed language",
            old_ext
        ))
    })?;
    let old_code = std::fs::read_to_string(old_path)?;
    let new_code = std::fs::read_to_string(new_path)?;
    Ok(crate::semantic_diff::diff_code(
        &old_code,
        &new_code,
        &config.language,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;
    use tempfile::tempdir;

    #[test]
    fn test_search_and_skeleton() {
        let codebase_dir = tempdir().unwrap();
        let db_dir = tempdir().unwrap();

        let storage = Storage::open(db_dir.path().join("findex_db")).unwrap();

        let file1 = codebase_dir.path().join("main.rs");
        write(
            &file1,
            b"
            pub struct Config {
                port: u16,
            }
            pub fn run_server(cfg: Config) {
                println!(\"running\");
            }
        ",
        )
        .unwrap();

        // Ingest codebase
        let stats = ingest_codebase(codebase_dir.path(), db_dir.path(), &storage).unwrap();
        assert_eq!(stats.parsed_files, 1);

        // 1. Verify Lexical Search
        let results =
            search_codebase(db_dir.path(), &storage, "server", "lexical", None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.name, "run_server");

        // 2. Verify Semantic/Vector Search
        let results2 = search_codebase(
            db_dir.path(),
            &storage,
            "Config struct",
            "semantic",
            None,
            10,
        )
        .unwrap();
        assert!(!results2.is_empty());

        // 3. Verify Hybrid Search
        let results3 = search_codebase(
            db_dir.path(),
            &storage,
            "run_server Config",
            "hybrid",
            None,
            10,
        )
        .unwrap();
        assert!(!results3.is_empty());

        // 3.1 Verify Reranking Search
        let reranker = search::rerank::MockReranker;
        let results3_reranked = search_codebase(
            db_dir.path(),
            &storage,
            "run_server",
            "hybrid",
            Some(&reranker),
            10,
        )
        .unwrap();
        assert_eq!(results3_reranked[0].0.name, "run_server");

        // 4. Verify Skeleton Generation
        let skeleton = get_codebase_skeleton(&storage, 1000).unwrap();
        assert!(skeleton.contains("struct Config"));
        assert!(skeleton.contains("fn run_server"));
    }

    #[cfg(feature = "stack-graphs")]
    #[test]
    fn stack_graphs_persist_exact_cross_file_reference_edges() {
        let codebase_dir = tempdir().unwrap();
        let db_dir = tempdir().unwrap();
        write(
            codebase_dir.path().join("provider.py"),
            b"def provide():\n    return 42\n",
        )
        .unwrap();
        write(
            codebase_dir.path().join("consumer.py"),
            b"from provider import provide\n\ndef consume():\n    return provide()\n",
        )
        .unwrap();
        let storage = Storage::open(db_dir.path().join("db")).unwrap();
        let stats = ingest_codebase_phase0(codebase_dir.path(), db_dir.path(), &storage).unwrap();
        assert_eq!(stats.parsed_files, 2);
        let exact: Vec<_> = storage
            .list_edges()
            .unwrap()
            .into_iter()
            .filter(|edge| edge.tags.iter().any(|tag| tag == "stack-graphs"))
            .collect();
        assert!(
            exact.iter().any(|edge| {
                storage
                    .get_symbol(&edge.dst)
                    .unwrap()
                    .is_some_and(|symbol| symbol.name == "provide")
            }),
            "expected an exact reference edge to provider.py#provide, got {exact:#?}"
        );
    }
}
