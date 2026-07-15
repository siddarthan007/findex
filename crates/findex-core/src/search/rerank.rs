use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ndarray::Array2;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

use crate::storage::Symbol;
use crate::IngestionError;

type TokenizedPair = (Vec<i64>, Vec<i64>, Vec<i64>);

pub trait Reranker: Send + Sync {
    fn rerank(
        &self,
        query: &str,
        candidates: &[(Symbol, f32)],
    ) -> Result<Vec<(Symbol, f32)>, IngestionError>;
    fn release_idle_resources(&self, _idle_for: Duration) -> bool {
        false
    }
}

/// A lightweight, pure-Rust mock reranker that ranks candidates based on word overlap density.
pub struct MockReranker;

impl Reranker for MockReranker {
    fn rerank(
        &self,
        query: &str,
        candidates: &[(Symbol, f32)],
    ) -> Result<Vec<(Symbol, f32)>, IngestionError> {
        let query_words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        let mut scored = Vec::new();

        for (sym, base_score) in candidates {
            let mut matches = 0;
            let text = format!("{} {}", sym.signature, sym.name).to_lowercase();
            for word in &query_words {
                if text.contains(word) {
                    matches += 1;
                }
            }

            // Combine base search score with density overlap score
            let overlap_score = if !query_words.is_empty() {
                (matches as f32) / (query_words.len() as f32)
            } else {
                0.0
            };

            let final_score = base_score * 0.4 + overlap_score * 0.6;
            scored.push((sym.clone(), final_score));
        }

        // Sort descending by final score
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored)
    }
}

/// A local ONNX cross-encoder reranker.
///
/// Expects a directory containing:
///   - `model.onnx`     - cross-encoder ONNX model
///   - `tokenizer.json` - HuggingFace tokenizer config
///
/// Typical models: BGE-reranker-v2-m3, jina-reranker-v2-base-multilingual.
/// The model is run once per (query, candidate) pair, so this is intentionally
/// used only on the top-k candidates returned by the first-stage retriever.
pub struct CrossEncoderReranker {
    tokenizer: Tokenizer,
    model_path: PathBuf,
    session: Mutex<ManagedSession>,
    max_length: usize,
}

struct ManagedSession {
    session: Option<Session>,
    last_used: Instant,
}

impl CrossEncoderReranker {
    pub fn from_dir<P: AsRef<Path>>(dir: P) -> Result<Self, IngestionError> {
        let dir = dir.as_ref();
        Self::from_files(dir.join("model.onnx"), dir.join("tokenizer.json"))
    }

    pub fn from_files<P: AsRef<Path>, T: AsRef<Path>>(
        model_path: P,
        tokenizer_path: T,
    ) -> Result<Self, IngestionError> {
        let model_path = model_path.as_ref();
        let tokenizer_path = tokenizer_path.as_ref();

        if !model_path.exists() {
            return Err(IngestionError::Reranker(format!(
                "Model file not found: {}",
                model_path.display()
            )));
        }
        if !tokenizer_path.exists() {
            return Err(IngestionError::Reranker(format!(
                "Tokenizer file not found: {}",
                tokenizer_path.display()
            )));
        }

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| IngestionError::Reranker(format!("Failed to load tokenizer: {}", e)))?;

        let session = build_session(model_path)?;

        let max_length = std::env::var("FINDEX_RERANK_MAX_TOKENS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(384)
            .clamp(32, 8192);

        Ok(Self {
            tokenizer,
            model_path: model_path.to_path_buf(),
            session: Mutex::new(ManagedSession {
                session: Some(session),
                last_used: Instant::now(),
            }),
            max_length,
        })
    }

    fn tokenize_pair(&self, query: &str, text: &str) -> Result<TokenizedPair, IngestionError> {
        let encoding = self
            .tokenizer
            .encode((query.to_string(), text.to_string()), true)
            .map_err(|e| IngestionError::Reranker(format!("Tokenization failed: {}", e)))?;

        let mut ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let mut mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let mut type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();

        if ids.len() > self.max_length {
            ids.truncate(self.max_length);
            mask.truncate(self.max_length);
            type_ids.truncate(self.max_length);
        }
        Ok((ids, mask, type_ids))
    }

    fn sigmoid(x: f32) -> f32 {
        1.0 / (1.0 + (-x).exp())
    }

    fn score_pair(&self, query: &str, text: &str) -> Result<f32, IngestionError> {
        let (ids, mask, type_ids) = self.tokenize_pair(query, text)?;
        let sequence_length = ids.len();

        let input_ids = Array2::from_shape_vec((1, sequence_length), ids)
            .map_err(|e| IngestionError::Reranker(format!("Invalid input_ids shape: {}", e)))?;
        let attention_mask = Array2::from_shape_vec((1, sequence_length), mask).map_err(|e| {
            IngestionError::Reranker(format!("Invalid attention_mask shape: {}", e))
        })?;
        let token_type_ids =
            Array2::from_shape_vec((1, sequence_length), type_ids).map_err(|e| {
                IngestionError::Reranker(format!("Invalid token_type_ids shape: {}", e))
            })?;

        let input_ids_tensor = Tensor::from_array(input_ids).map_err(|e| {
            IngestionError::Reranker(format!("Failed to create input_ids tensor: {}", e))
        })?;
        let attention_mask_tensor = Tensor::from_array(attention_mask).map_err(|e| {
            IngestionError::Reranker(format!("Failed to create attention_mask tensor: {}", e))
        })?;
        let mut managed = self
            .session
            .lock()
            .map_err(|e| IngestionError::Reranker(format!("ONNX session lock poisoned: {}", e)))?;
        if managed.session.is_none() {
            managed.session = Some(build_session(&self.model_path)?);
        }
        managed.last_used = Instant::now();
        let session = managed.session.as_mut().expect("session was restored");

        let mut inputs = vec![
            (
                std::borrow::Cow::Borrowed("input_ids"),
                ort::session::SessionInputValue::from(input_ids_tensor),
            ),
            (
                std::borrow::Cow::Borrowed("attention_mask"),
                ort::session::SessionInputValue::from(attention_mask_tensor),
            ),
        ];
        if session
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids")
        {
            let token_type_ids_tensor = Tensor::from_array(token_type_ids).map_err(|e| {
                IngestionError::Reranker(format!("Failed to create token_type_ids tensor: {}", e))
            })?;
            inputs.push((
                std::borrow::Cow::Borrowed("token_type_ids"),
                ort::session::SessionInputValue::from(token_type_ids_tensor),
            ));
        }

        let outputs = session
            .run(inputs)
            .map_err(|e| IngestionError::Reranker(format!("ONNX inference failed: {}", e)))?;

        let output = outputs
            .iter()
            .next()
            .map(|(_, value)| value)
            .ok_or_else(|| {
                IngestionError::Reranker("ONNX model produced no outputs".to_string())
            })?;

        let array = output.try_extract_array::<f32>().map_err(|e| {
            IngestionError::Reranker(format!("Failed to extract ONNX output: {}", e))
        })?;

        let shape: Vec<usize> = array.shape().to_vec();
        let flat: Vec<f32> = array.iter().copied().collect();

        let logit = if shape.len() == 1 || (shape.len() == 2 && shape[1] == 1) {
            flat[0]
        } else if shape.len() == 2 && shape[1] == 2 {
            // Take the positive-class logit.
            flat[1]
        } else {
            return Err(IngestionError::Reranker(format!(
                "Unexpected cross-encoder output shape: {:?}",
                shape
            )));
        };

        Ok(Self::sigmoid(logit))
    }

    fn score_batch(&self, query: &str, texts: &[String]) -> Result<Vec<f32>, IngestionError> {
        let batch_size = texts.len();
        if batch_size == 0 {
            return Ok(Vec::new());
        }
        let mut encoded = Vec::with_capacity(batch_size);
        for text in texts {
            encoded.push(self.tokenize_pair(query, text)?);
        }
        let sequence_length = encoded
            .iter()
            .map(|(ids, _, _)| ids.len())
            .max()
            .unwrap_or(1);
        let mut ids = Vec::with_capacity(batch_size * sequence_length);
        let mut masks = Vec::with_capacity(batch_size * sequence_length);
        let mut type_ids = Vec::with_capacity(batch_size * sequence_length);
        for (mut item_ids, mut item_mask, mut item_type_ids) in encoded {
            item_ids.resize(sequence_length, 0);
            item_mask.resize(sequence_length, 0);
            item_type_ids.resize(sequence_length, 0);
            ids.extend(item_ids);
            masks.extend(item_mask);
            type_ids.extend(item_type_ids);
        }
        let input_ids = Array2::from_shape_vec((batch_size, sequence_length), ids)
            .map_err(|error| IngestionError::Reranker(error.to_string()))?;
        let attention_mask = Array2::from_shape_vec((batch_size, sequence_length), masks)
            .map_err(|error| IngestionError::Reranker(error.to_string()))?;
        let token_type_ids = Array2::from_shape_vec((batch_size, sequence_length), type_ids)
            .map_err(|error| IngestionError::Reranker(error.to_string()))?;

        let input_ids = Tensor::from_array(input_ids)
            .map_err(|error| IngestionError::Reranker(error.to_string()))?;
        let attention_mask = Tensor::from_array(attention_mask)
            .map_err(|error| IngestionError::Reranker(error.to_string()))?;
        let mut managed = self
            .session
            .lock()
            .map_err(|error| IngestionError::Reranker(error.to_string()))?;
        if managed.session.is_none() {
            managed.session = Some(build_session(&self.model_path)?);
        }
        managed.last_used = Instant::now();
        let session = managed.session.as_mut().expect("session was restored");
        let mut inputs = vec![
            (
                std::borrow::Cow::Borrowed("input_ids"),
                ort::session::SessionInputValue::from(input_ids),
            ),
            (
                std::borrow::Cow::Borrowed("attention_mask"),
                ort::session::SessionInputValue::from(attention_mask),
            ),
        ];
        if session
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids")
        {
            inputs.push((
                std::borrow::Cow::Borrowed("token_type_ids"),
                ort::session::SessionInputValue::from(
                    Tensor::from_array(token_type_ids)
                        .map_err(|error| IngestionError::Reranker(error.to_string()))?,
                ),
            ));
        }
        let outputs = session
            .run(inputs)
            .map_err(|error| IngestionError::Reranker(format!("ONNX batch failed: {error}")))?;
        let output = outputs
            .iter()
            .next()
            .map(|(_, value)| value)
            .ok_or_else(|| IngestionError::Reranker("ONNX model produced no outputs".into()))?;
        let array = output
            .try_extract_array::<f32>()
            .map_err(|error| IngestionError::Reranker(error.to_string()))?;
        let shape = array.shape();
        let flat: Vec<f32> = array.iter().copied().collect();
        let scores = match shape {
            [batch] if *batch == batch_size => flat.into_iter().map(Self::sigmoid).collect(),
            [batch, 1] if *batch == batch_size => flat.into_iter().map(Self::sigmoid).collect(),
            [batch, 2] if *batch == batch_size => flat
                .chunks_exact(2)
                .map(|logits| Self::sigmoid(logits[1]))
                .collect(),
            _ => {
                return Err(IngestionError::Reranker(format!(
                    "unexpected batched cross-encoder output shape: {shape:?}"
                )))
            }
        };
        Ok(scores)
    }

    fn score_pairs(&self, query: &str, texts: &[String]) -> Result<Vec<f32>, IngestionError> {
        let batch_size = std::env::var("FINDEX_RERANK_BATCH_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(16)
            .min(64);
        let mut scores = Vec::with_capacity(texts.len());
        for batch in texts.chunks(batch_size) {
            match self.score_batch(query, batch) {
                Ok(mut batch_scores) => scores.append(&mut batch_scores),
                Err(error) if batch.len() > 1 => {
                    eprintln!("Findex reranker batch fallback: {error}");
                    for text in batch {
                        scores.push(self.score_pair(query, text)?);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(scores)
    }
}

fn build_session(model_path: &Path) -> Result<Session, IngestionError> {
    #[allow(unused_mut)]
    let builder = Session::builder().map_err(|error| {
        IngestionError::Reranker(format!("Failed to create ONNX session: {error}"))
    })?;
    let builder = builder
        .with_intra_threads(crate::runtime::onnx_intra_threads())
        .map_err(|error| {
            IngestionError::Reranker(format!("Failed to set ONNX threads: {error}"))
        })?;
    let builder = builder.with_inter_threads(1).map_err(|error| {
        IngestionError::Reranker(format!("Failed to set ONNX inter-op threads: {error}"))
    })?;
    let mut builder = builder.with_memory_pattern(true).map_err(|error| {
        IngestionError::Reranker(format!("Failed to configure ONNX memory pattern: {error}"))
    })?;

    #[cfg(feature = "cuda")]
    let mut builder = {
        use ort::ep::ExecutionProvider;
        let device = std::env::var("FINDEX_ONNX_DEVICE").unwrap_or_else(|_| "auto".to_string());
        let cuda = crate::runtime::cuda_execution_provider();
        if !device.eq_ignore_ascii_case("cpu") && cuda.is_available().unwrap_or(false) {
            match builder.with_execution_providers([cuda.build()]) {
                Ok(cuda_builder) => {
                    eprintln!("Findex ONNX reranker: CUDA execution provider enabled");
                    cuda_builder
                }
                Err(error) => {
                    eprintln!(
                        "Findex ONNX reranker: CUDA setup failed ({}); using CPU",
                        error
                    );
                    let cpu_builder = Session::builder().map_err(|builder_error| {
                        IngestionError::Reranker(format!(
                            "Failed to recreate CPU session builder: {builder_error}"
                        ))
                    })?;
                    let cpu_builder = cpu_builder
                        .with_intra_threads(crate::runtime::onnx_intra_threads())
                        .map_err(|error| {
                            IngestionError::Reranker(format!(
                                "Failed to set CPU ONNX threads: {error}"
                            ))
                        })?;
                    let cpu_builder = cpu_builder.with_inter_threads(1).map_err(|error| {
                        IngestionError::Reranker(format!(
                            "Failed to set CPU inter-op threads: {error}"
                        ))
                    })?;
                    cpu_builder.with_memory_pattern(true).map_err(|error| {
                        IngestionError::Reranker(format!(
                            "Failed to set CPU memory pattern: {error}"
                        ))
                    })?
                }
            }
        } else {
            builder
        }
    };

    builder
        .commit_from_file(model_path)
        .map_err(|error| IngestionError::Reranker(format!("Failed to load ONNX model: {}", error)))
}

impl Reranker for CrossEncoderReranker {
    fn rerank(
        &self,
        query: &str,
        candidates: &[(Symbol, f32)],
    ) -> Result<Vec<(Symbol, f32)>, IngestionError> {
        let texts: Vec<String> = candidates
            .iter()
            .map(|(symbol, _)| {
                format!(
                    "{} {} {} {} {}",
                    symbol.kind,
                    symbol.name,
                    symbol.signature,
                    symbol.docstring.as_deref().unwrap_or_default(),
                    symbol.file_path
                )
            })
            .collect();
        let cross_scores = self.score_pairs(query, &texts)?;
        let mut scored = Vec::with_capacity(candidates.len());
        for ((sym, base_score), cross_score) in candidates.iter().zip(cross_scores) {
            let final_score = base_score * 0.35 + cross_score * 0.65;
            scored.push((sym.clone(), final_score));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored)
    }

    fn release_idle_resources(&self, idle_for: Duration) -> bool {
        let Ok(mut managed) = self.session.lock() else {
            return false;
        };
        if managed.session.is_some() && managed.last_used.elapsed() >= idle_for {
            managed.session.take();
            true
        } else {
            false
        }
    }
}

/// Create a reranker from the environment.
///
/// If `FINDEX_RERANKER_MODEL_DIR` is set and points to a valid local ONNX
/// cross-encoder model directory, a `CrossEncoderReranker` is returned.
/// Otherwise the zero-dependency `MockReranker` is used.
pub fn create_reranker() -> Arc<dyn Reranker> {
    if let Ok(model_dir) = std::env::var("FINDEX_RERANKER_MODEL_DIR") {
        let resolved_dir = expand_home(&model_dir);
        match CrossEncoderReranker::from_dir(&resolved_dir) {
            Ok(reranker) => {
                eprintln!("Loaded local cross-encoder reranker from {}", model_dir);
                return Arc::new(reranker);
            }
            Err(e) => {
                eprintln!(
                    "Failed to load local reranker from {}: {}. Falling back to mock reranker.",
                    model_dir, e
                );
            }
        }
    }
    match crate::models::resolve_runtime_model(crate::models::ModelKind::Reranker) {
        Ok(Some(model)) => {
            match CrossEncoderReranker::from_files(&model.model_path, &model.tokenizer_path) {
                Ok(reranker) => {
                    eprintln!(
                        "Loaded pinned reranker {}@{}",
                        model.repository, model.revision
                    );
                    return Arc::new(reranker);
                }
                Err(error) => eprintln!(
                "Failed to load cached reranker {}: {}. Falling back to lexical overlap reranking.",
                model.repository, error
            ),
            }
        }
        Ok(None) => {}
        Err(error) => eprintln!("Automatic reranker acquisition failed: {error}"),
    }
    Arc::new(MockReranker)
}

fn expand_home(path: &str) -> std::path::PathBuf {
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return std::path::PathBuf::from(home)
                .join(path.trim_start_matches('~').trim_start_matches(['/', '\\']));
        }
    }
    std::path::PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_reranker() {
        let sym_a = Symbol {
            id: "sym_a".to_string(),
            name: "calculate_sum".to_string(),
            kind: "Function".to_string(),
            signature: "fn calculate_sum(a: i32, b: i32) -> i32".to_string(),
            file_path: "main.rs".to_string(),
            start_line: 1,
            start_col: 1,
            end_line: 3,
            end_col: 1,
            docstring: None,
            ..Default::default()
        };

        let sym_b = Symbol {
            id: "sym_b".to_string(),
            name: "run_server".to_string(),
            kind: "Function".to_string(),
            signature: "fn run_server()".to_string(),
            file_path: "main.rs".to_string(),
            start_line: 5,
            start_col: 1,
            end_line: 7,
            end_col: 1,
            docstring: None,
            ..Default::default()
        };

        let reranker = MockReranker;
        let candidates = vec![(sym_a, 0.5), (sym_b, 0.8)];

        // Query "calculate sum" should rerank sym_a (score 0.5 originally) above sym_b (score 0.8 originally)
        let results = reranker.rerank("calculate sum", &candidates).unwrap();

        assert_eq!(results[0].0.id, "sym_a");
    }
}
