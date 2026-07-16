//! Persisted production settings shared by CLI, TUI, desktop, and MCP.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

pub const SETTINGS_FILE: &str = "settings.json";
pub const SETTINGS_VERSION: u32 = 2;

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("settings I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid settings JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid setting: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComputeDevice {
    #[default]
    Auto,
    Cpu,
    Cuda,
}

impl std::fmt::Display for ComputeDevice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
        })
    }
}

impl FromStr for ComputeDevice {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            "cuda" | "gpu" => Ok(Self::Cuda),
            _ => Err("expected auto, cpu, or cuda".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl std::fmt::Display for ThemePreference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        })
    }
}

impl FromStr for ThemePreference {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "system" | "auto" => Ok(Self::System),
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            _ => Err("expected system, light, or dark".to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct IndexingSettings {
    pub lexical_index: bool,
    pub semantic_index: bool,
    pub stack_graphs: bool,
    pub watcher: bool,
    pub vfs_shadowing: bool,
    pub execution_trace_pinning: bool,
}

impl Default for IndexingSettings {
    fn default() -> Self {
        Self {
            lexical_index: true,
            semantic_index: true,
            stack_graphs: true,
            watcher: true,
            vfs_shadowing: true,
            execution_trace_pinning: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RetrievalSettings {
    pub semantic_search: bool,
    pub reranking: bool,
    pub graph_expansion: bool,
    pub structural_prefetch: bool,
    pub graph_hops: u32,
    pub candidate_limit: usize,
    pub default_token_budget: usize,
    pub mmr_lambda: f32,
    pub predictive_query_cache: bool,
    pub query_cache_entries: usize,
    pub query_cache_ttl_seconds: u64,
}

impl Default for RetrievalSettings {
    fn default() -> Self {
        Self {
            semantic_search: true,
            reranking: true,
            graph_expansion: true,
            structural_prefetch: true,
            graph_hops: 1,
            candidate_limit: 32,
            default_token_budget: 2_048,
            mmr_lambda: 0.75,
            predictive_query_cache: true,
            query_cache_entries: 128,
            query_cache_ttl_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RuntimeSettings {
    pub compute_device: ComputeDevice,
    pub model_profile: String,
    pub memory_budget_mib: u64,
    pub gpu_memory_limit_mib: u64,
    pub model_idle_seconds: u64,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            compute_device: ComputeDevice::Auto,
            model_profile: "fast".to_string(),
            memory_budget_mib: 2_048,
            gpu_memory_limit_mib: 4_096,
            model_idle_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiSettings {
    pub theme: ThemePreference,
    pub motion: bool,
    pub graph_particles: bool,
    pub graph_labels: bool,
    /// Keep the desktop process available from the system tray when its window closes.
    pub minimize_to_tray: bool,
    /// Render the small pointer companion in graphical and terminal clients.
    pub cursor_companion: bool,
    /// Enable terminal mouse capture. Touch-capable terminals normally translate touch to mouse.
    pub terminal_pointer_input: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            motion: true,
            graph_particles: true,
            graph_labels: true,
            minimize_to_tray: true,
            cursor_companion: true,
            terminal_pointer_input: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct TelemetrySettings {
    /// Master consent gate. No event leaves the device while this is false.
    pub enabled: bool,
    pub crash_reports: bool,
    pub include_hardware: bool,
    pub include_project_metrics: bool,
    /// Reserved for an explicit, per-report diagnostic action. Findex never
    /// samples source code automatically, even when this gate is enabled.
    pub include_source_samples: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FindexSettings {
    pub version: u32,
    pub indexing: IndexingSettings,
    pub retrieval: RetrievalSettings,
    pub runtime: RuntimeSettings,
    pub ui: UiSettings,
    pub telemetry: TelemetrySettings,
}

impl Default for FindexSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            indexing: IndexingSettings::default(),
            retrieval: RetrievalSettings::default(),
            runtime: RuntimeSettings::default(),
            ui: UiSettings::default(),
            telemetry: TelemetrySettings::default(),
        }
    }
}

impl FindexSettings {
    pub fn validate_and_normalize(mut self) -> Result<Self, SettingsError> {
        self.version = SETTINGS_VERSION;
        self.retrieval.graph_hops = self.retrieval.graph_hops.clamp(0, 4);
        self.retrieval.candidate_limit = self.retrieval.candidate_limit.clamp(4, 200);
        self.retrieval.default_token_budget =
            self.retrieval.default_token_budget.clamp(128, 32_768);
        if !self.retrieval.mmr_lambda.is_finite() {
            return Err(SettingsError::Invalid(
                "retrieval.mmr_lambda must be finite".to_string(),
            ));
        }
        self.retrieval.mmr_lambda = self.retrieval.mmr_lambda.clamp(0.0, 1.0);
        self.retrieval.query_cache_entries = self.retrieval.query_cache_entries.clamp(1, 2_048);
        self.retrieval.query_cache_ttl_seconds =
            self.retrieval.query_cache_ttl_seconds.clamp(5, 86_400);
        self.runtime.memory_budget_mib = self.runtime.memory_budget_mib.clamp(256, 1_048_576);
        self.runtime.gpu_memory_limit_mib = self.runtime.gpu_memory_limit_mib.clamp(256, 1_048_576);
        self.runtime.model_idle_seconds = self.runtime.model_idle_seconds.clamp(30, 86_400);
        self.runtime.model_profile = self.runtime.model_profile.trim().to_ascii_lowercase();
        if !matches!(
            self.runtime.model_profile.as_str(),
            "fast" | "balanced" | "quality"
        ) {
            return Err(SettingsError::Invalid(
                "runtime.model_profile must be fast, balanced, or quality".to_string(),
            ));
        }
        if !self.telemetry.enabled {
            self.telemetry.crash_reports = false;
            self.telemetry.include_hardware = false;
            self.telemetry.include_project_metrics = false;
            self.telemetry.include_source_samples = false;
        }
        Ok(self)
    }
}

pub fn settings_path(db_dir: impl AsRef<Path>) -> PathBuf {
    db_dir.as_ref().join(SETTINGS_FILE)
}

pub fn load(db_dir: impl AsRef<Path>) -> Result<FindexSettings, SettingsError> {
    let path = settings_path(db_dir);
    if !path.exists() {
        return Ok(FindexSettings::default());
    }
    serde_json::from_slice::<FindexSettings>(&fs::read(path)?)?.validate_and_normalize()
}

pub fn load_or_default(db_dir: impl AsRef<Path>) -> FindexSettings {
    load(db_dir).unwrap_or_default()
}

pub fn save(
    db_dir: impl AsRef<Path>,
    settings: FindexSettings,
) -> Result<FindexSettings, SettingsError> {
    let settings = settings.validate_and_normalize()?;
    let path = settings_path(db_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let encoded = serde_json::to_vec_pretty(&settings)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&encoded)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(&path).map_err(|error| error.error)?;
    Ok(settings)
}

pub fn reset(db_dir: impl AsRef<Path>) -> Result<FindexSettings, SettingsError> {
    save(db_dir, FindexSettings::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_clamp_unsafe_values() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = FindexSettings::default();
        settings.retrieval.graph_hops = 99;
        settings.retrieval.candidate_limit = 1;
        settings.retrieval.mmr_lambda = 4.0;
        let saved = save(dir.path(), settings).unwrap();
        assert_eq!(saved.retrieval.graph_hops, 4);
        assert_eq!(saved.retrieval.candidate_limit, 4);
        assert_eq!(saved.retrieval.mmr_lambda, 1.0);
        assert_eq!(load(dir.path()).unwrap(), saved);
    }

    #[test]
    fn partial_json_inherits_production_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            settings_path(dir.path()),
            br#"{"retrieval":{"reranking":false}}"#,
        )
        .unwrap();
        let settings = load(dir.path()).unwrap();
        assert!(!settings.retrieval.reranking);
        assert!(settings.retrieval.graph_expansion);
        assert!(settings.indexing.stack_graphs);
    }
}
