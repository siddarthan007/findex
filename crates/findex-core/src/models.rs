//! Pinned production model acquisition backed by the Hugging Face cache.

use hf_hub::HFClientSync;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};

const MINILM_EMBEDDING_REVISION: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";
const MINILM_RERANKER_REVISION: &str = "c5ee24cb16019beea0893ab7796b1df96625c6b8";
const JINA_CODE_REVISION: &str = "516f4baf13dec4ddddda8631e019b5737c8bc250";
const JINA_RERANKER_REVISION: &str = "b8c14f4e723d9e0aab4732a7b7b93741eeeb77c2";
static RUNTIME_MODEL_PROFILE: AtomicU8 = AtomicU8::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Embedding,
    Reranker,
}

/// Explicit accuracy/latency trade-off. Profiles pin repository commits and
/// artifacts, so a production update cannot silently change retrieval output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProfile {
    /// 384-dimensional MiniLM embedding + 6-layer MS MARCO reranker.
    #[default]
    Fast,
    /// Quantized code-specialized embedding and reranker.
    Balanced,
    /// Full-precision code-specialized embedding and reranker.
    Quality,
}

impl ModelProfile {
    pub const ALL: [Self; 3] = [Self::Fast, Self::Balanced, Self::Quality];
}

impl fmt::Display for ModelProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Quality => "quality",
        })
    }
}

impl FromStr for ModelProfile {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fast" | "minilm" => Ok(Self::Fast),
            "balanced" | "code" | "quantized" => Ok(Self::Balanced),
            "quality" | "full" | "full-precision" => Ok(Self::Quality),
            _ => Err(format!(
                "unknown model profile '{value}'; expected fast, balanced, or quality"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPolicy {
    /// Download a pinned artifact when it is absent from the shared cache.
    Auto,
    /// Resolve only from the local cache; never touch the network.
    Offline,
    /// Do not resolve bundled models. Environment-provided model paths still work.
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedModel {
    pub kind: ModelKind,
    pub profile: ModelProfile,
    pub repository: String,
    pub revision: String,
    pub artifact: String,
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("Hugging Face model acquisition failed for {repository}: {source}")]
    Hub {
        repository: String,
        #[source]
        source: hf_hub::HFError,
    },
}

#[derive(Clone, Copy)]
struct ModelSpec {
    kind: ModelKind,
    owner: &'static str,
    name: &'static str,
    revision: &'static str,
    artifact: &'static str,
}

impl ModelSpec {
    fn repository(self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

fn spec(profile: ModelProfile, kind: ModelKind) -> ModelSpec {
    match (profile, kind) {
        (ModelProfile::Fast, ModelKind::Embedding) => ModelSpec {
            kind,
            owner: "sentence-transformers",
            name: "all-MiniLM-L6-v2",
            revision: MINILM_EMBEDDING_REVISION,
            artifact: "onnx/model.onnx",
        },
        (ModelProfile::Fast, ModelKind::Reranker) => ModelSpec {
            kind,
            owner: "cross-encoder",
            name: "ms-marco-MiniLM-L6-v2",
            revision: MINILM_RERANKER_REVISION,
            artifact: "onnx/model.onnx",
        },
        (ModelProfile::Balanced, ModelKind::Embedding) => ModelSpec {
            kind,
            owner: "jinaai",
            name: "jina-embeddings-v2-base-code",
            revision: JINA_CODE_REVISION,
            artifact: "onnx/model_quantized.onnx",
        },
        (ModelProfile::Balanced, ModelKind::Reranker) => ModelSpec {
            kind,
            owner: "jinaai",
            name: "jina-reranker-v1-turbo-en",
            revision: JINA_RERANKER_REVISION,
            artifact: "onnx/model_quantized.onnx",
        },
        (ModelProfile::Quality, ModelKind::Embedding) => ModelSpec {
            kind,
            owner: "jinaai",
            name: "jina-embeddings-v2-base-code",
            revision: JINA_CODE_REVISION,
            artifact: "onnx/model.onnx",
        },
        (ModelProfile::Quality, ModelKind::Reranker) => ModelSpec {
            kind,
            owner: "jinaai",
            name: "jina-reranker-v1-turbo-en",
            revision: JINA_RERANKER_REVISION,
            artifact: "onnx/model.onnx",
        },
    }
}

/// Production release binaries self-provision by default. Debug/test builds stay
/// network-silent unless `FINDEX_MODEL_POLICY=auto` is explicitly set.
pub fn model_policy() -> ModelPolicy {
    match std::env::var("FINDEX_MODEL_POLICY")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "auto" | "download" | "online" => ModelPolicy::Auto,
        "offline" | "cache" | "local" => ModelPolicy::Offline,
        "disabled" | "disable" | "manual" | "mock" => ModelPolicy::Disabled,
        _ if cfg!(debug_assertions) => ModelPolicy::Disabled,
        _ => ModelPolicy::Auto,
    }
}

pub fn model_profile() -> ModelProfile {
    std::env::var("FINDEX_MODEL_PROFILE")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| match RUNTIME_MODEL_PROFILE.load(Ordering::Relaxed) {
            2 => ModelProfile::Balanced,
            3 => ModelProfile::Quality,
            _ => ModelProfile::Fast,
        })
}

pub fn set_runtime_model_profile(profile: &str) {
    let value = match profile.parse::<ModelProfile>().unwrap_or_default() {
        ModelProfile::Fast => 1,
        ModelProfile::Balanced => 2,
        ModelProfile::Quality => 3,
    };
    RUNTIME_MODEL_PROFILE.store(value, Ordering::Relaxed);
}

/// Ensure one pinned model and tokenizer exist in the standard Hugging Face
/// cache. The Hub client provides atomic, concurrent-safe cache population.
pub fn ensure_model_for_profile(
    kind: ModelKind,
    profile: ModelProfile,
    local_files_only: bool,
) -> Result<ResolvedModel, ModelError> {
    let spec = spec(profile, kind);
    let repository = spec.repository();
    let client = HFClientSync::new().map_err(|source| ModelError::Hub {
        repository: repository.clone(),
        source,
    })?;
    let repo = client.model(spec.owner, spec.name);
    let download = |filename: &str| {
        repo.download_file()
            .filename(filename)
            .revision(spec.revision)
            .local_files_only(local_files_only)
            .send()
            .map_err(|source| ModelError::Hub {
                repository: repository.clone(),
                source,
            })
    };

    let model_path = download(spec.artifact)?;
    let tokenizer_path = download("tokenizer.json")?;
    Ok(ResolvedModel {
        kind: spec.kind,
        profile,
        repository,
        revision: spec.revision.to_string(),
        artifact: spec.artifact.to_string(),
        model_path,
        tokenizer_path,
    })
}

pub fn ensure_model(kind: ModelKind, local_files_only: bool) -> Result<ResolvedModel, ModelError> {
    ensure_model_for_profile(kind, model_profile(), local_files_only)
}

/// Resolve a bundled model under the current runtime policy.
pub fn resolve_runtime_model(kind: ModelKind) -> Result<Option<ResolvedModel>, ModelError> {
    match model_policy() {
        ModelPolicy::Auto => ensure_model(kind, false).map(Some),
        ModelPolicy::Offline => ensure_model(kind, true).map(Some),
        ModelPolicy::Disabled => Ok(None),
    }
}

/// Prewarm both artifacts on a background thread. This is suitable for desktop
/// startup because callers need not block the UI and the shared cache is reused
/// by later model construction.
pub fn prewarm_models_background(
    local_files_only: bool,
) -> std::io::Result<std::thread::JoinHandle<Vec<Result<ResolvedModel, ModelError>>>> {
    std::thread::Builder::new()
        .name("findex-model-prewarm".to_string())
        .spawn(move || {
            [ModelKind::Embedding, ModelKind::Reranker]
                .into_iter()
                .map(|kind| ensure_model(kind, local_files_only))
                .collect()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_uses_immutable_commits() {
        for profile in ModelProfile::ALL {
            for kind in [ModelKind::Embedding, ModelKind::Reranker] {
                let revision = spec(profile, kind).revision;
                assert_eq!(revision.len(), 40);
                assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
            }
        }
    }

    #[test]
    fn profile_parser_has_stable_names() {
        for profile in ModelProfile::ALL {
            assert_eq!(profile.to_string().parse::<ModelProfile>(), Ok(profile));
        }
        assert!("largest".parse::<ModelProfile>().is_err());
    }

    #[test]
    fn balanced_uses_quantized_code_models() {
        let embedding = spec(ModelProfile::Balanced, ModelKind::Embedding);
        let reranker = spec(ModelProfile::Balanced, ModelKind::Reranker);
        assert!(embedding.name.contains("code"));
        assert!(embedding.artifact.contains("quantized"));
        assert!(reranker.artifact.contains("quantized"));
    }
}
