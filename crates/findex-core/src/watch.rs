use crate::parser::is_supported_path;
use crate::storage::Storage;
use crate::{ingest_codebase, IngestionError, IngestionStats};
use notify::{EventKind, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebouncedEvent, Debouncer};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("Notify error: {0}")]
    Notify(#[from] notify::Error),
    #[error("Ingestion error: {0}")]
    Ingestion(#[from] IngestionError),
    #[error("Channel receiver error: {0}")]
    Channel(#[from] std::sync::mpsc::RecvError),
}

/// Watches a codebase directory and re-indexes incrementally when source files change.
/// Debounces rapid-fire events and only triggers one ingestion pass per quiet period.
pub fn watch_codebase<P: AsRef<Path>, D: AsRef<Path>>(
    root_dir: P,
    db_dir: D,
    storage: Arc<Storage>,
    debounce_ms: u64,
    mut on_reindex: impl FnMut(&IngestionStats),
) -> Result<(), WatchError> {
    let root = root_dir.as_ref().to_path_buf();
    let db_path = db_dir.as_ref().to_path_buf();

    let (tx, rx) = std::sync::mpsc::channel::<Vec<std::path::PathBuf>>();

    // Build a debounced watcher. notify-debouncer-mini bundles events that arrive
    // within the debounce window and emits a single Vec<DebouncedEvent>.
    let mut debouncer: Debouncer<notify::RecommendedWatcher> = new_debouncer(
        Duration::from_millis(debounce_ms),
        move |res: Result<Vec<DebouncedEvent>, notify::Error>| match res {
            Ok(events) if !events.is_empty() => {
                let changed = events
                    .into_iter()
                    .map(|event| event.path)
                    .filter(|path| is_watched_path(path))
                    .collect::<Vec<_>>();
                if !changed.is_empty() {
                    let _ = tx.send(changed);
                }
            }
            _ => {}
        },
    )?;

    debouncer.watcher().watch(&root, RecursiveMode::Recursive)?;

    println!(
        "Watching {} for changes (debounce: {} ms). Press Ctrl+C to stop.",
        root.display(),
        debounce_ms
    );

    // Perform an initial indexing pass so the watched state is up to date.
    let initial_stats = ingest_codebase(&root, &db_path, &storage)?;
    println!(
        "Initial index: {} files, {} symbols parsed, {} ms",
        initial_stats.total_files, initial_stats.parsed_files, initial_stats.duration_ms
    );

    loop {
        let mut changed_paths = rx.recv()?;

        // Drain any additional debounced signals that arrived while we were indexing.
        while let Ok(paths) = rx.try_recv() {
            changed_paths.extend(paths);
        }
        changed_paths.sort_unstable();
        changed_paths.dedup();

        println!(
            "Detected {} source path change(s), updating the Merkle index...",
            changed_paths.len()
        );
        match ingest_codebase(&root, &db_path, &storage) {
            Ok(stats) => {
                println!(
                    "Re-index complete: {} files, {} changed, {} deleted, {} ms",
                    stats.total_files, stats.parsed_files, stats.deleted_files, stats.duration_ms
                );
                on_reindex(&stats);
            }
            Err(e) => {
                eprintln!("Re-index failed: {}", e);
            }
        }
    }
}

/// Returns true if the notify event kind represents a meaningful file content change.
pub fn is_content_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// Filters a watched path so that only supported source files trigger re-indexing.
pub fn is_watched_path(path: &Path) -> bool {
    is_supported_path(path)
}
