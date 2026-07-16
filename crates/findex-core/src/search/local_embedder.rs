use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use ndarray::Array2;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

use crate::search::vector::{Embedder, VectorError};

/// A local ONNX embedding model.
///
/// Expects a directory containing:
///   - `model.onnx`        - the ONNX inference model
///   - `tokenizer.json`    - the HuggingFace tokenizer config
///
/// The model may expose either:
///   - a `sentence_embedding` output of shape `[batch, hidden_size]`, or
///   - a `last_hidden_state` / `token_embeddings` output of shape `[batch, seq_len, hidden_size]`
///     which will be mean-pooled using the attention mask and L2-normalized.
pub struct LocalEmbedder {
    tokenizer: Tokenizer,
    model_path: PathBuf,
    session: Mutex<ManagedSession>,
    max_length: usize,
    dimension: usize,
}

struct ManagedSession {
    session: Option<Session>,
    last_used: Instant,
}

impl LocalEmbedder {
    /// Load a local model from the given directory.
    pub fn from_dir<P: AsRef<Path>>(dir: P) -> Result<Self, VectorError> {
        let dir = dir.as_ref();
        Self::from_files(dir.join("model.onnx"), dir.join("tokenizer.json"))
    }

    /// Load model and tokenizer paths independently. Hub cache snapshots keep
    /// their repository layout, so the ONNX artifact may live under `onnx/`.
    pub fn from_files<P: AsRef<Path>, T: AsRef<Path>>(
        model_path: P,
        tokenizer_path: T,
    ) -> Result<Self, VectorError> {
        let model_path = model_path.as_ref();
        let tokenizer_path = tokenizer_path.as_ref();

        if !model_path.exists() {
            return Err(VectorError::Embedder(format!(
                "Model file not found: {}",
                model_path.display()
            )));
        }
        if !tokenizer_path.exists() {
            return Err(VectorError::Embedder(format!(
                "Tokenizer file not found: {}",
                tokenizer_path.display()
            )));
        }

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| VectorError::Embedder(format!("Failed to load tokenizer: {}", e)))?;

        let session = build_session(model_path)?;

        // all-MiniLM-L6-v2 is configured for 256 tokens. Larger code models
        // can opt into a wider window, but never exceed a limit explicitly
        // declared by the tokenizer configuration.
        let profile_default = match crate::models::model_profile() {
            crate::models::ModelProfile::Fast => 256,
            crate::models::ModelProfile::Balanced | crate::models::ModelProfile::Quality => 512,
        };
        let requested_max_length = std::env::var("FINDEX_EMBEDDING_MAX_TOKENS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(profile_default);
        let model_limit = tokenizer
            .get_truncation()
            .map(|params| params.max_length)
            .unwrap_or(requested_max_length);
        let max_length = requested_max_length.min(model_limit).max(1);
        let mut embedder = Self {
            tokenizer,
            model_path: model_path.to_path_buf(),
            session: Mutex::new(ManagedSession {
                session: Some(session),
                last_used: Instant::now(),
            }),
            max_length,
            dimension: 0,
        };

        // Derive the embedding dimension by running a dummy forward pass.
        let dummy = embedder.embed_impl("")?;
        embedder.dimension = dummy.len();

        Ok(embedder)
    }

    fn tokenize(&self, text: &str) -> Result<(Vec<i64>, Vec<i64>), VectorError> {
        let encoding = self
            .tokenizer
            .encode(text.to_string(), true)
            .map_err(|e| VectorError::Embedder(format!("Tokenization failed: {}", e)))?;

        let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();

        // Pad or truncate to a fixed sequence length.
        let mut ids = ids;
        let mut mask = mask;
        if ids.len() > self.max_length {
            ids.truncate(self.max_length);
            mask.truncate(self.max_length);
        }
        while ids.len() < self.max_length {
            ids.push(0);
            mask.push(0);
        }

        Ok((ids, mask))
    }

    fn mean_pool(&self, hidden: &Array2<f32>, mask: &Array2<i64>) -> Vec<f32> {
        // hidden: [seq_len, hidden_size]
        // mask: [1, seq_len]
        let mask = mask.index_axis(ndarray::Axis(0), 0);
        let seq_len = hidden.shape()[0];
        let hidden_size = hidden.shape()[1];
        let mut pooled = vec![0.0f32; hidden_size];
        let mut weight = 0.0f32;

        for i in 0..seq_len {
            let w = if mask[i] > 0 { 1.0f32 } else { 0.0f32 };
            if w == 0.0 {
                continue;
            }
            weight += w;
            for d in 0..hidden_size {
                pooled[d] += hidden[[i, d]] * w;
            }
        }

        if weight > 0.0 {
            for v in pooled.iter_mut() {
                *v /= weight;
            }
        }
        pooled
    }

    fn l2_normalize(vec: &mut [f32]) {
        let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in vec.iter_mut() {
                *v /= norm;
            }
        }
    }
}

fn build_session(model_path: &Path) -> Result<Session, VectorError> {
    #[allow(unused_mut)]
    let builder = Session::builder().map_err(|error| {
        VectorError::Embedder(format!("Failed to create ONNX session: {}", error))
    })?;
    let builder = builder
        .with_intra_threads(crate::runtime::onnx_intra_threads())
        .map_err(|error| VectorError::Embedder(format!("Failed to set ONNX threads: {error}")))?;
    let builder = builder.with_inter_threads(1).map_err(|error| {
        VectorError::Embedder(format!("Failed to set ONNX inter-op threads: {error}"))
    })?;
    let mut builder = builder.with_memory_pattern(true).map_err(|error| {
        VectorError::Embedder(format!("Failed to configure ONNX memory pattern: {error}"))
    })?;

    #[cfg(feature = "cuda")]
    let mut builder = {
        use ort::ep::ExecutionProvider;
        let device = crate::runtime::onnx_device();
        let cuda = crate::runtime::cuda_execution_provider();
        if !device.eq_ignore_ascii_case("cpu") && cuda.is_available().unwrap_or(false) {
            match builder.with_execution_providers([cuda.build()]) {
                Ok(cuda_builder) => {
                    eprintln!("Findex ONNX embedder: CUDA execution provider enabled");
                    cuda_builder
                }
                Err(error) => {
                    eprintln!(
                        "Findex ONNX embedder: CUDA setup failed ({}); using CPU",
                        error
                    );
                    let cpu_builder = Session::builder().map_err(|builder_error| {
                        VectorError::Embedder(format!(
                            "Failed to recreate CPU session builder: {builder_error}"
                        ))
                    })?;
                    let cpu_builder = cpu_builder
                        .with_intra_threads(crate::runtime::onnx_intra_threads())
                        .map_err(|error| {
                            VectorError::Embedder(format!(
                                "Failed to set CPU ONNX threads: {error}"
                            ))
                        })?;
                    let cpu_builder = cpu_builder.with_inter_threads(1).map_err(|error| {
                        VectorError::Embedder(format!(
                            "Failed to set CPU inter-op threads: {error}"
                        ))
                    })?;
                    cpu_builder.with_memory_pattern(true).map_err(|error| {
                        VectorError::Embedder(format!("Failed to set CPU memory pattern: {error}"))
                    })?
                }
            }
        } else {
            builder
        }
    };

    builder
        .commit_from_file(model_path)
        .map_err(|error| VectorError::Embedder(format!("Failed to load ONNX model: {}", error)))
}

impl Embedder for LocalEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        self.embed_impl(text).unwrap_or_else(|e| {
            // Preserve useful ranking if ONNX fails after startup. A zero vector
            // would collapse every candidate to the same semantic score.
            eprintln!("LocalEmbedder error: {}", e);
            crate::search::vector::MockEmbedder::new(self.dimension.max(1)).embed(text)
        })
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn fingerprint(&self) -> String {
        format!(
            "onnx:{}:{}:{}",
            self.model_path.display(),
            self.max_length,
            self.dimension
        )
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

    fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        if texts.is_empty() {
            return Vec::new();
        }
        self.embed_batch_impl(texts).unwrap_or_else(|error| {
            eprintln!("LocalEmbedder batch error: {}", error);
            let fallback = crate::search::vector::MockEmbedder::new(self.dimension.max(1));
            texts.iter().map(|text| fallback.embed(text)).collect()
        })
    }
}

impl LocalEmbedder {
    fn embed_impl(&self, text: &str) -> Result<Vec<f32>, VectorError> {
        let (ids, mask) = self.tokenize(text)?;

        let input_ids = Array2::from_shape_vec((1, self.max_length), ids)
            .map_err(|e| VectorError::Embedder(format!("Invalid input_ids shape: {}", e)))?;
        let attention_mask = Array2::from_shape_vec((1, self.max_length), mask)
            .map_err(|e| VectorError::Embedder(format!("Invalid attention_mask shape: {}", e)))?;

        let input_ids_tensor = Tensor::from_array(input_ids).map_err(|e| {
            VectorError::Embedder(format!("Failed to create input_ids tensor: {}", e))
        })?;
        let attention_mask_tensor = Tensor::from_array(attention_mask.clone()).map_err(|e| {
            VectorError::Embedder(format!("Failed to create attention_mask tensor: {}", e))
        })?;

        let mut managed = self
            .session
            .lock()
            .map_err(|e| VectorError::Embedder(format!("ONNX session lock poisoned: {}", e)))?;
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

        let has_token_type_ids = session
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");
        if has_token_type_ids {
            let token_type_ids = vec![0i64; self.max_length];
            let token_type_ids_arr = Array2::from_shape_vec((1, self.max_length), token_type_ids)
                .map_err(|e| {
                VectorError::Embedder(format!("Invalid token_type_ids shape: {}", e))
            })?;
            let token_type_ids_tensor = Tensor::from_array(token_type_ids_arr).map_err(|e| {
                VectorError::Embedder(format!("Failed to create token_type_ids tensor: {}", e))
            })?;
            inputs.push((
                std::borrow::Cow::Borrowed("token_type_ids"),
                ort::session::SessionInputValue::from(token_type_ids_tensor),
            ));
        }

        let outputs = session
            .run(inputs)
            .map_err(|e| VectorError::Embedder(format!("ONNX inference failed: {}", e)))?;

        // Prefer a sentence-level embedding if the model provides one.
        let output = outputs
            .iter()
            .find(|(name, _)| *name == "sentence_embedding")
            .map(|(_, value)| value)
            .or_else(|| outputs.iter().next().map(|(_, value)| value))
            .ok_or_else(|| VectorError::Embedder("ONNX model produced no outputs".to_string()))?;

        let array = output
            .try_extract_array::<f32>()
            .map_err(|e| VectorError::Embedder(format!("Failed to extract ONNX output: {}", e)))?;

        let shape: Vec<usize> = array.shape().to_vec();
        let flat: Vec<f32> = array.iter().copied().collect();

        let mut embedding = if shape.len() == 2 {
            // [batch, hidden_size]
            flat
        } else if shape.len() == 3 {
            // [batch, seq_len, hidden_size] -> mean pool
            let _batch = shape[0];
            let seq_len = shape[1];
            let hidden = shape[2];
            let hidden_array = ndarray::Array2::from_shape_vec((seq_len, hidden), flat)
                .map_err(|e| VectorError::Embedder(format!("Invalid hidden state shape: {}", e)))?;
            let mask_array = attention_mask;
            self.mean_pool(&hidden_array, &mask_array)
        } else {
            return Err(VectorError::Embedder(format!(
                "Unexpected ONNX output shape: {:?}",
                shape
            )));
        };

        Self::l2_normalize(&mut embedding);
        Ok(embedding)
    }

    fn embed_batch_impl(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
        let batch_size = texts.len();
        let mut ids = Vec::with_capacity(batch_size * self.max_length);
        let mut masks = Vec::with_capacity(batch_size * self.max_length);
        for text in texts {
            let (item_ids, item_mask) = self.tokenize(text)?;
            ids.extend(item_ids);
            masks.extend(item_mask);
        }

        let input_ids =
            Array2::from_shape_vec((batch_size, self.max_length), ids).map_err(|error| {
                VectorError::Embedder(format!("Invalid batched input_ids shape: {}", error))
            })?;
        let attention_mask =
            Array2::from_shape_vec((batch_size, self.max_length), masks).map_err(|error| {
                VectorError::Embedder(format!("Invalid batched attention_mask shape: {}", error))
            })?;
        let input_ids_tensor = Tensor::from_array(input_ids).map_err(|error| {
            VectorError::Embedder(format!("Failed to create batched input tensor: {}", error))
        })?;
        let attention_mask_tensor =
            Tensor::from_array(attention_mask.clone()).map_err(|error| {
                VectorError::Embedder(format!("Failed to create batched mask tensor: {}", error))
            })?;

        let mut managed = self.session.lock().map_err(|error| {
            VectorError::Embedder(format!("ONNX session lock poisoned: {}", error))
        })?;
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
            let type_ids = Array2::from_shape_vec(
                (batch_size, self.max_length),
                vec![0i64; batch_size * self.max_length],
            )
            .map_err(|error| {
                VectorError::Embedder(format!("Invalid batched token_type_ids shape: {}", error))
            })?;
            inputs.push((
                std::borrow::Cow::Borrowed("token_type_ids"),
                ort::session::SessionInputValue::from(Tensor::from_array(type_ids).map_err(
                    |error| {
                        VectorError::Embedder(format!(
                            "Failed to create batched type tensor: {}",
                            error
                        ))
                    },
                )?),
            ));
        }

        let outputs = session.run(inputs).map_err(|error| {
            VectorError::Embedder(format!("Batched ONNX inference failed: {}", error))
        })?;
        let output = outputs
            .iter()
            .find(|(name, _)| *name == "sentence_embedding")
            .map(|(_, value)| value)
            .or_else(|| outputs.iter().next().map(|(_, value)| value))
            .ok_or_else(|| VectorError::Embedder("ONNX model produced no outputs".to_string()))?;
        let array = output.try_extract_array::<f32>().map_err(|error| {
            VectorError::Embedder(format!("Failed to extract batched output: {}", error))
        })?;
        let shape = array.shape().to_vec();
        let flat: Vec<f32> = array.iter().copied().collect();

        let mut embeddings = match shape.as_slice() {
            [batch, hidden] if *batch == batch_size => flat
                .chunks_exact(*hidden)
                .map(|chunk| chunk.to_vec())
                .collect::<Vec<_>>(),
            [batch, sequence, hidden] if *batch == batch_size => {
                let mut pooled = Vec::with_capacity(batch_size);
                for batch_index in 0..batch_size {
                    let mut embedding = vec![0.0f32; *hidden];
                    let mut weight = 0.0f32;
                    for token_index in 0..*sequence {
                        if attention_mask[[batch_index, token_index]] == 0 {
                            continue;
                        }
                        weight += 1.0;
                        let offset = (batch_index * *sequence + token_index) * *hidden;
                        for dimension in 0..*hidden {
                            embedding[dimension] += flat[offset + dimension];
                        }
                    }
                    if weight > 0.0 {
                        for value in &mut embedding {
                            *value /= weight;
                        }
                    }
                    pooled.push(embedding);
                }
                pooled
            }
            _ => {
                return Err(VectorError::Embedder(format!(
                    "Unexpected batched ONNX output shape: {:?}",
                    shape
                )))
            }
        };

        for embedding in &mut embeddings {
            Self::l2_normalize(embedding);
        }
        Ok(embeddings)
    }
}

struct DeferredEmbedder {
    current: Arc<RwLock<Arc<dyn Embedder>>>,
}

impl Embedder for DeferredEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        self.current
            .read()
            .expect("deferred embedder lock was poisoned")
            .embed(text)
    }

    fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        self.current
            .read()
            .expect("deferred embedder lock was poisoned")
            .embed_batch(texts)
    }

    fn dimension(&self) -> usize {
        self.current
            .read()
            .expect("deferred embedder lock was poisoned")
            .dimension()
    }

    fn fingerprint(&self) -> String {
        self.current
            .read()
            .expect("deferred embedder lock was poisoned")
            .fingerprint()
    }

    fn release_idle_resources(&self, idle_for: Duration) -> bool {
        self.current
            .read()
            .expect("deferred embedder lock was poisoned")
            .release_idle_resources(idle_for)
    }
}

fn load_resolved_embedder(
    model: &crate::models::ResolvedModel,
) -> Result<LocalEmbedder, VectorError> {
    LocalEmbedder::from_files(&model.model_path, &model.tokenizer_path)
}

/// Create an embedder from the environment.
///
/// If `FINDEX_EMBEDDING_MODEL_DIR` is set and points to a valid local ONNX
/// model directory, a `LocalEmbedder` is returned. Otherwise a deterministic
/// `MockEmbedder` with the given dimension is used.
pub fn create_embedder(dimension: usize) -> Arc<dyn Embedder> {
    if let Ok(model_dir) = std::env::var("FINDEX_EMBEDDING_MODEL_DIR") {
        let resolved_dir = expand_home(&model_dir);
        match LocalEmbedder::from_dir(&resolved_dir) {
            Ok(embedder) => {
                eprintln!(
                    "Loaded local embedding model from {} (dimension={})",
                    model_dir, embedder.dimension
                );
                return Arc::new(embedder);
            }
            Err(e) => {
                eprintln!(
                    "Failed to load local embedding model from {}: {}. Falling back to mock embedder.",
                    model_dir, e
                );
            }
        }
    }
    let kind = crate::models::ModelKind::Embedding;
    let profile = crate::models::model_profile();
    if crate::models::model_policy() == crate::models::ModelPolicy::Disabled {
        return Arc::new(crate::search::vector::MockEmbedder::new(dimension));
    }

    match crate::models::ensure_model_for_profile(kind, profile, true) {
        Ok(model) => match load_resolved_embedder(&model) {
            Ok(embedder) => {
                eprintln!(
                    "Loaded pinned embedding model {}@{} (dimension={})",
                    model.repository, model.revision, embedder.dimension
                );
                return Arc::new(embedder);
            }
            Err(error) => eprintln!(
                "Failed to load cached embedding model {}: {}. Falling back to deterministic embeddings.",
                model.repository, error
            ),
        },
        Err(error) if crate::models::model_policy() == crate::models::ModelPolicy::Offline => {
            eprintln!("Offline embedding model is not cached: {error}");
        }
        Err(_) => {
            let expected_dimension = match profile {
                crate::models::ModelProfile::Fast => 384,
                crate::models::ModelProfile::Balanced | crate::models::ModelProfile::Quality => 768,
            };
            let current: Arc<RwLock<Arc<dyn Embedder>>> = Arc::new(RwLock::new(Arc::new(
                crate::search::vector::MockEmbedder::new(expected_dimension),
            )));
            let background = Arc::clone(&current);
            let _ = std::thread::Builder::new()
                .name("findex-embedding-download".to_string())
                .spawn(move || match crate::models::ensure_model_for_profile(kind, profile, false) {
                    Ok(model) => match load_resolved_embedder(&model) {
                        Ok(embedder) => {
                            *background
                                .write()
                                .expect("deferred embedder lock was poisoned") = Arc::new(embedder);
                            eprintln!(
                                "Pinned embedding model {}@{} is ready",
                                model.repository, model.revision
                            );
                        }
                        Err(error) => eprintln!(
                            "Downloaded embedding model could not be loaded: {error}"
                        ),
                    },
                    Err(error) => eprintln!("Background embedding acquisition failed: {error}"),
                });
            return Arc::new(DeferredEmbedder { current });
        }
    }
    Arc::new(crate::search::vector::MockEmbedder::new(dimension))
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
