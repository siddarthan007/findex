//! Explicit-consent telemetry with a bounded local queue.
//!
//! Events contain operational counters, never queries, paths, symbol names, or
//! source text. Payloads are zstd-compressed before upload, credentials stay in
//! the OS vault, and a disabled consent gate prevents both queueing and network
//! access.

use crate::auth::{self, AuthError};
use crate::settings::FindexSettings;
use crate::storage::Storage;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::sync::{Mutex, OnceLock};

const FIREBASE_PROJECT: &str = "findexcodeintelligence";
const MAX_QUEUE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EVENTS: usize = 1_000;
const MAX_PAYLOAD_BYTES: usize = 128 * 1024;
const FLUSH_BATCH: usize = 20;

static QUEUE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static PANIC_HOOK: OnceLock<()> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryStatus {
    pub enabled: bool,
    pub queued_events: usize,
    pub queued_bytes: u64,
    pub queue_limit_bytes: u64,
    pub source_collection_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    id: String,
    schema_version: u32,
    event_type: String,
    created_at: String,
    platform: String,
    app_version: String,
    encoding: String,
    payload: String,
    contains_source: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("telemetry is disabled")]
    Disabled,
    #[error("telemetry payload is too large")]
    PayloadTooLarge,
    #[error("telemetry queue I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("telemetry serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("authentication failed: {0}")]
    Auth(#[from] AuthError),
    #[error("telemetry upload failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Firebase telemetry backend returned {0}")]
    Backend(String),
}

/// Install one process-wide, source-free panic counter. The previous hook is
/// preserved, and the consent settings are evaluated again at panic time.
pub fn install_panic_hook(db_path: PathBuf, settings: Arc<RwLock<FindexSettings>>) {
    if PANIC_HOOK.set(()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let current = settings
            .read()
            .map(|value| value.clone())
            .unwrap_or_default();
        if current.telemetry.enabled && current.telemetry.crash_reports {
            let _ = record_event(
                &db_path,
                &current,
                None,
                "rust_panic",
                json!({
                    "surface": "rust",
                    "location_available": info.location().is_some()
                }),
            );
        }
        previous(info);
    }));
}

/// Queue one privacy-filtered event. Callers supply only categorical values or
/// counters; Findex augments them with coarse platform data according to the
/// persisted consent gates.
pub fn record_event(
    db_path: &Path,
    settings: &FindexSettings,
    storage: Option<&Storage>,
    event_type: &str,
    details: Value,
) -> Result<(), TelemetryError> {
    if !settings.telemetry.enabled {
        return Err(TelemetryError::Disabled);
    }
    let mut payload = match details {
        Value::Object(values) => values,
        _ => Map::new(),
    };
    payload.insert(
        "locale".into(),
        Value::String(
            std::env::var("LC_ALL")
                .or_else(|_| std::env::var("LANG"))
                .unwrap_or_else(|_| "unknown".to_string())
                .chars()
                .take(32)
                .collect(),
        ),
    );
    if settings.telemetry.include_hardware {
        let runtime = crate::runtime::profile(false);
        payload.insert(
            "hardware".into(),
            json!({
                "logical_cpus": runtime.logical_cpus,
                "memory_gib_bucket": (runtime.total_memory_bytes / 1_073_741_824).clamp(1, 1024),
                "compute_device": runtime.compute_device,
                "cuda_compiled": runtime.cuda_compiled
            }),
        );
    }
    if settings.telemetry.include_project_metrics {
        if let Some(storage) = storage {
            payload.insert(
                "project".into(),
                json!({
                    "files": storage.list_files().map(|values| values.len()).unwrap_or(0),
                    "symbols": storage.list_symbols().map(|values| values.len()).unwrap_or(0),
                    "edges": storage.list_edges().map(|values| values.len()).unwrap_or(0)
                }),
            );
        }
    }
    let encoded = serde_json::to_vec(&payload)?;
    if encoded.len() > MAX_PAYLOAD_BYTES {
        return Err(TelemetryError::PayloadTooLarge);
    }
    let compressed = zstd::stream::encode_all(Cursor::new(encoded), 3)?;
    let envelope = Envelope {
        id: uuid::Uuid::new_v4().to_string(),
        schema_version: 1,
        event_type: sanitize_event_type(event_type),
        created_at: now_rfc3339(),
        platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        encoding: "zstd+base64".to_string(),
        payload: STANDARD_NO_PAD.encode(compressed),
        contains_source: false,
    };
    append_envelope(db_path, &envelope)
}

pub fn status(
    db_path: &Path,
    settings: &FindexSettings,
) -> Result<TelemetryStatus, TelemetryError> {
    let queue_path = queue_path(db_path);
    let queued_bytes = queue_path
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let queued_events = if queue_path.exists() {
        std::fs::read_to_string(&queue_path)?.lines().count()
    } else {
        0
    };
    Ok(TelemetryStatus {
        enabled: settings.telemetry.enabled,
        queued_events,
        queued_bytes,
        queue_limit_bytes: MAX_QUEUE_BYTES,
        // Source collection requires a separate explicit diagnostic action;
        // no automatic event path currently sets contains_source=true.
        source_collection_active: false,
    })
}

/// Upload a bounded batch to the authenticated user's Firestore namespace.
/// Successfully uploaded prefixes are removed atomically; transient failures
/// leave the remaining queue untouched for a later retry.
pub async fn flush(db_path: &Path, settings: &FindexSettings) -> Result<usize, TelemetryError> {
    if !settings.telemetry.enabled {
        return Err(TelemetryError::Disabled);
    }
    let Some(credential) = auth::credential().await? else {
        return Ok(0);
    };
    let batch = {
        let _guard = queue_lock().lock().expect("telemetry queue lock poisoned");
        read_queue(db_path)?
            .into_iter()
            .take(FLUSH_BATCH)
            .collect::<Vec<_>>()
    };
    if batch.is_empty() {
        return Ok(0);
    }
    let mut uploaded_ids = Vec::with_capacity(batch.len());
    let client = reqwest::Client::new();
    let mut failure = None;
    for envelope in &batch {
        let endpoint = format!(
            "https://firestore.googleapis.com/v1/projects/{FIREBASE_PROJECT}/databases/(default)/documents/users/{}/events?documentId={}",
            credential.user.uid, envelope.id
        );
        let response = client
            .post(endpoint)
            .bearer_auth(&credential.id_token)
            .json(&firestore_document(envelope))
            .send()
            .await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::CONFLICT {
            uploaded_ids.push(envelope.id.clone());
            continue;
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        failure = Some(TelemetryError::Backend(format!(
            "{}: {}",
            status,
            body.chars().take(512).collect::<String>()
        )));
        break;
    }
    if !uploaded_ids.is_empty() {
        let uploaded = uploaded_ids.iter().collect::<HashSet<_>>();
        let _guard = queue_lock().lock().expect("telemetry queue lock poisoned");
        let mut current = read_queue(db_path)?;
        current.retain(|envelope| !uploaded.contains(&envelope.id));
        rewrite_queue(db_path, &current)?;
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(uploaded_ids.len())
}

fn firestore_document(envelope: &Envelope) -> Value {
    json!({
        "fields": {
            "schema_version": { "integerValue": envelope.schema_version.to_string() },
            "event_type": { "stringValue": envelope.event_type },
            "created_at": { "stringValue": envelope.created_at },
            "platform": { "stringValue": envelope.platform },
            "app_version": { "stringValue": envelope.app_version },
            "encoding": { "stringValue": envelope.encoding },
            "payload": { "stringValue": envelope.payload },
            "contains_source": { "booleanValue": envelope.contains_source }
        }
    })
}

fn append_envelope(db_path: &Path, envelope: &Envelope) -> Result<(), TelemetryError> {
    let _guard = queue_lock().lock().expect("telemetry queue lock poisoned");
    let path = queue_path(db_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    serde_json::to_writer(&mut file, envelope)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    let queued_bytes = file.metadata()?.len();
    drop(file);
    let exceeds_event_limit = if queued_bytes > MAX_QUEUE_BYTES {
        true
    } else {
        std::fs::read(&path)?
            .iter()
            .filter(|byte| **byte == b'\n')
            .take(MAX_EVENTS + 1)
            .count()
            > MAX_EVENTS
    };
    if queued_bytes > MAX_QUEUE_BYTES || exceeds_event_limit {
        let queue = bounded_queue(read_queue(db_path)?)?;
        rewrite_queue(db_path, &queue)?;
    }
    Ok(())
}

fn read_queue(db_path: &Path) -> Result<Vec<Envelope>, TelemetryError> {
    let path = queue_path(db_path);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

fn rewrite_queue(db_path: &Path, queue: &[Envelope]) -> Result<(), TelemetryError> {
    let path = queue_path(db_path);
    let parent = path.parent().expect("queue path has parent");
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    for envelope in queue {
        serde_json::to_writer(&mut file, envelope)?;
        file.write_all(b"\n")?;
    }
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn bounded_queue(queue: Vec<Envelope>) -> Result<Vec<Envelope>, serde_json::Error> {
    let mut newest = Vec::with_capacity(queue.len().min(MAX_EVENTS));
    let mut bytes = 0_u64;
    for envelope in queue.into_iter().rev() {
        if newest.len() == MAX_EVENTS {
            break;
        }
        let line_bytes = serde_json::to_vec(&envelope)?.len() as u64 + 1;
        if bytes.saturating_add(line_bytes) > MAX_QUEUE_BYTES {
            continue;
        }
        bytes += line_bytes;
        newest.push(envelope);
    }
    newest.reverse();
    Ok(newest)
}

fn queue_path(db_path: &Path) -> PathBuf {
    db_path.join("telemetry").join("queue.jsonl")
}

fn queue_lock() -> &'static Mutex<()> {
    QUEUE_LOCK.get_or_init(|| Mutex::new(()))
}

fn sanitize_event_type(value: &str) -> String {
    let sanitized = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .take(48)
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_collection_never_creates_a_queue() {
        let directory = tempfile::tempdir().unwrap();
        let settings = FindexSettings::default();
        assert!(matches!(
            record_event(directory.path(), &settings, None, "started", json!({})),
            Err(TelemetryError::Disabled)
        ));
        assert!(!queue_path(directory.path()).exists());
    }

    #[test]
    fn queued_payload_is_compressed_bounded_and_source_free() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings = FindexSettings::default();
        settings.telemetry.enabled = true;
        record_event(
            directory.path(),
            &settings,
            None,
            "desktop_started",
            json!({ "surface": "desktop" }),
        )
        .unwrap();
        let queue = read_queue(directory.path()).unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].encoding, "zstd+base64");
        assert!(!queue[0].contains_source);
        assert!(queue_path(directory.path()).metadata().unwrap().len() < 4_096);
    }

    #[test]
    fn event_type_drops_untrusted_punctuation() {
        assert_eq!(
            sanitize_event_type("search.completed/<script>"),
            "searchcompletedscript"
        );
    }

    #[test]
    fn queue_bounds_keep_the_newest_events() {
        let events = (0..MAX_EVENTS + 5)
            .map(|index| Envelope {
                id: index.to_string(),
                schema_version: 1,
                event_type: "bounded".to_string(),
                created_at: "2026-07-16T00:00:00Z".to_string(),
                platform: "test/test".to_string(),
                app_version: "3.1.0".to_string(),
                encoding: "zstd+base64".to_string(),
                payload: "eA".to_string(),
                contains_source: false,
            })
            .collect();
        let bounded = bounded_queue(events).unwrap();
        assert_eq!(bounded.len(), MAX_EVENTS);
        assert_eq!(bounded.first().unwrap().id, "5");
        assert_eq!(bounded.last().unwrap().id, (MAX_EVENTS + 4).to_string());
    }
}
