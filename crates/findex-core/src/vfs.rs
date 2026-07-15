use crate::parser::{parse_code, ParserError};
use crate::storage::{Edge, Symbol};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

const DEFAULT_VFS_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_VFS_FILES: usize = 512;

#[derive(Debug, Error)]
pub enum VfsError {
    #[error("shadow file is {actual} bytes, above the VFS limit of {limit} bytes")]
    FileTooLarge { actual: usize, limit: usize },
    #[error("VFS file capacity must be at least one")]
    InvalidCapacity,
}

#[derive(Debug, Clone)]
struct VfsFile {
    content: Arc<str>,
    hash: [u8; 32],
    version: u64,
    last_access: Cell<u64>,
}

/// Memory-bounded virtual filesystem for unsaved buffers and speculative edits.
/// Contents are held in `Arc<str>` so compilation and metadata inspection share
/// the same allocation. Least-recently-used entries are evicted before inserts.
#[derive(Debug, Clone)]
pub struct Vfs {
    files: HashMap<PathBuf, VfsFile>,
    total_bytes: usize,
    max_bytes: usize,
    max_files: usize,
    clock: Cell<u64>,
    next_version: u64,
}

impl Default for Vfs {
    fn default() -> Self {
        let max_bytes = std::env::var("FINDEX_VFS_MAX_MB")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .map(|mib| mib.saturating_mul(1024 * 1024))
            .unwrap_or(DEFAULT_VFS_BYTES)
            .clamp(1024 * 1024, 1024 * 1024 * 1024);
        let max_files = std::env::var("FINDEX_VFS_MAX_FILES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_VFS_FILES)
            .clamp(1, 16_384);
        Self::with_limits(max_bytes, max_files).expect("default VFS limits are valid")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VfsPutResult {
    pub path: PathBuf,
    pub version: u64,
    pub content_hash: String,
    pub bytes: usize,
    pub evicted: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VfsStats {
    pub files: usize,
    pub bytes: usize,
    pub max_files: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct VfsSnapshotReport {
    pub loaded: usize,
    pub skipped: usize,
    pub evicted: Vec<PathBuf>,
}

/// Explicitly serializable VFS state. It contains unsaved source text and must
/// only be persisted when the operator enables `FINDEX_VFS_PERSIST=1`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VfsSnapshot {
    pub files: Vec<VfsSnapshotFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsSnapshotFile {
    pub path: PathBuf,
    pub content: String,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limits(max_bytes: usize, max_files: usize) -> Result<Self, VfsError> {
        if max_files == 0 {
            return Err(VfsError::InvalidCapacity);
        }
        Ok(Self {
            files: HashMap::new(),
            total_bytes: 0,
            max_bytes: max_bytes.max(1),
            max_files,
            clock: Cell::new(0),
            next_version: 1,
        })
    }

    /// Insert or update a shadowed file and evict the least-recently-used
    /// entries needed to stay inside both memory and file-count limits.
    pub fn put<P: Into<PathBuf>>(
        &mut self,
        path: P,
        content: String,
    ) -> Result<VfsPutResult, VfsError> {
        let path = normalize_path(path.into());
        let bytes = content.len();
        if bytes > self.max_bytes {
            return Err(VfsError::FileTooLarge {
                actual: bytes,
                limit: self.max_bytes,
            });
        }
        if let Some(previous) = self.files.remove(&path) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.content.len());
        }

        let mut evicted = Vec::new();
        while !self.files.is_empty()
            && (self.files.len() >= self.max_files
                || self.total_bytes.saturating_add(bytes) > self.max_bytes)
        {
            let Some(oldest) = self
                .files
                .iter()
                .min_by_key(|(_, file)| file.last_access.get())
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            if let Some(removed) = self.files.remove(&oldest) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.content.len());
                evicted.push(oldest);
            }
        }

        let hash = *blake3::hash(content.as_bytes()).as_bytes();
        let version = self.next_version;
        self.next_version = self.next_version.saturating_add(1);
        let access = self.bump_clock();
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.files.insert(
            path.clone(),
            VfsFile {
                content: Arc::<str>::from(content),
                hash,
                version,
                last_access: Cell::new(access),
            },
        );
        Ok(VfsPutResult {
            path,
            version,
            content_hash: hash_hex(&hash),
            bytes,
            evicted,
        })
    }

    pub fn remove<P: AsRef<Path>>(&mut self, path: P) -> Option<String> {
        let path = normalize_path(path.as_ref().to_path_buf());
        self.files.remove(&path).map(|file| {
            self.total_bytes = self.total_bytes.saturating_sub(file.content.len());
            file.content.to_string()
        })
    }

    pub fn get<P: AsRef<Path>>(&self, path: P) -> Option<&str> {
        let path = normalize_path(path.as_ref().to_path_buf());
        let file = self.files.get(&path)?;
        file.last_access.set(self.bump_clock());
        Some(file.content.as_ref())
    }

    pub fn contains<P: AsRef<Path>>(&self, path: P) -> bool {
        self.files
            .contains_key(&normalize_path(path.as_ref().to_path_buf()))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = (&PathBuf, &str)> {
        self.files
            .iter()
            .map(|(path, file)| (path, file.content.as_ref()))
    }

    pub fn stats(&self) -> VfsStats {
        VfsStats {
            files: self.files.len(),
            bytes: self.total_bytes,
            max_files: self.max_files,
            max_bytes: self.max_bytes,
        }
    }

    pub fn snapshot_from_disk<P: AsRef<Path>>(&mut self, paths: &[P]) -> VfsSnapshotReport {
        let mut report = VfsSnapshotReport::default();
        for path in paths {
            match fs::read_to_string(path.as_ref()) {
                Ok(content) => match self.put(path.as_ref().to_path_buf(), content) {
                    Ok(result) => {
                        report.loaded += 1;
                        report.evicted.extend(result.evicted);
                    }
                    Err(_) => report.skipped += 1,
                },
                Err(_) => report.skipped += 1,
            }
        }
        report
    }

    pub fn export_snapshot(&self) -> VfsSnapshot {
        let mut files = self
            .files
            .iter()
            .map(|(path, file)| VfsSnapshotFile {
                path: path.clone(),
                content: file.content.to_string(),
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        VfsSnapshot { files }
    }

    /// Restore through `put` so current memory/file limits and LRU eviction
    /// remain authoritative even when an older snapshot was larger.
    pub fn restore_snapshot(&mut self, snapshot: VfsSnapshot) -> VfsSnapshotReport {
        let mut report = VfsSnapshotReport::default();
        for file in snapshot.files {
            match self.put(file.path, file.content) {
                Ok(result) => {
                    report.loaded += 1;
                    report.evicted.extend(result.evicted);
                }
                Err(_) => report.skipped += 1,
            }
        }
        report
    }

    pub fn apply_updates(
        &mut self,
        updates: Vec<VfsUpdate>,
    ) -> Result<Vec<VfsPutResult>, VfsError> {
        let mut results = Vec::new();
        for update in updates {
            match update {
                VfsUpdate::Write { path, content } => results.push(self.put(path, content)?),
                VfsUpdate::Delete { path } => {
                    self.remove(path);
                }
            }
        }
        Ok(results)
    }

    fn metadata(&self, path: &Path) -> Option<([u8; 32], u64)> {
        self.files
            .get(&normalize_path(path.to_path_buf()))
            .map(|file| (file.hash, file.version))
    }

    fn bump_clock(&self) -> u64 {
        let next = self.clock.get().saturating_add(1);
        self.clock.set(next);
        next
    }
}

#[derive(Debug, Clone)]
pub enum VfsUpdate {
    Write { path: PathBuf, content: String },
    Delete { path: PathBuf },
}

#[derive(Debug, Clone, Serialize)]
pub struct MicroCompileResult {
    pub path: PathBuf,
    pub version: u64,
    pub content_hash: String,
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
}

/// Parse a shadowed file without disk I/O. The result is isolated and does not
/// mutate the persisted index, making it safe for speculative agent edits.
pub fn micro_compile<P: AsRef<Path>>(
    path: P,
    vfs: &Vfs,
) -> Result<MicroCompileResult, ParserError> {
    let path = normalize_path(path.as_ref().to_path_buf());
    let content = vfs.get(&path).ok_or_else(|| {
        ParserError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} is not shadowed in the VFS", path.display()),
        ))
    })?;
    let (hash, version) = vfs.metadata(&path).expect("content and metadata coexist");
    let (symbols, edges) = parse_code(&path, content)?;
    Ok(MicroCompileResult {
        path,
        version,
        content_hash: hash_hex(&hash),
        symbols,
        edges,
    })
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if normalized.file_name().is_some() => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn hash_hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_remove_and_path_normalization() {
        let mut vfs = Vfs::new();
        vfs.put("./src/../src/main.rs", "fn main() {}".to_string())
            .unwrap();
        assert_eq!(vfs.get("src/main.rs"), Some("fn main() {}"));
        assert!(vfs.contains("./src/main.rs"));
        vfs.remove("src/main.rs");
        assert!(!vfs.contains("src/main.rs"));
    }

    #[test]
    fn micro_compile_reports_version_and_hash() {
        let mut vfs = Vfs::new();
        let inserted = vfs
            .put("widget.rs", "pub fn render() {}".to_string())
            .unwrap();
        let result = micro_compile(Path::new("widget.rs"), &vfs).unwrap();
        assert_eq!(result.version, inserted.version);
        assert_eq!(result.content_hash, inserted.content_hash);
        assert!(result.symbols.iter().any(|symbol| symbol.name == "render"));
    }

    #[test]
    fn micro_compile_missing_file() {
        let vfs = Vfs::new();
        assert!(micro_compile(Path::new("missing.rs"), &vfs).is_err());
    }

    #[test]
    fn evicts_lru_files_within_memory_and_count_limits() {
        let mut vfs = Vfs::with_limits(12, 2).unwrap();
        vfs.put("a.rs", "aaaa".into()).unwrap();
        vfs.put("b.rs", "bbbb".into()).unwrap();
        assert_eq!(vfs.get("a.rs"), Some("aaaa"));

        let result = vfs.put("c.rs", "cccccccc".into()).unwrap();

        assert!(result.evicted.contains(&PathBuf::from("b.rs")));
        assert!(vfs.contains("a.rs"));
        assert!(vfs.contains("c.rs"));
        assert!(vfs.stats().bytes <= 12);
        assert!(vfs.stats().files <= 2);
    }

    #[test]
    fn snapshot_roundtrip_obeys_limits() {
        let mut original = Vfs::with_limits(64, 4).unwrap();
        original.put("a.rs", "fn a() {}".into()).unwrap();
        original.put("b.rs", "fn b() {}".into()).unwrap();
        let mut restored = Vfs::with_limits(12, 1).unwrap();
        let report = restored.restore_snapshot(original.export_snapshot());
        assert_eq!(report.loaded, 2);
        assert_eq!(restored.len(), 1);
        assert!(restored.stats().bytes <= 12);
    }

    #[test]
    fn rejects_a_single_file_above_the_total_budget() {
        let mut vfs = Vfs::with_limits(4, 2).unwrap();
        let error = vfs.put("large.rs", "12345".into()).unwrap_err();
        assert!(matches!(error, VfsError::FileTooLarge { .. }));
        assert!(vfs.is_empty());
    }

    #[test]
    fn apply_updates_is_atomic_per_update_and_bounded() {
        let mut vfs = Vfs::with_limits(64, 4).unwrap();
        vfs.apply_updates(vec![
            VfsUpdate::Write {
                path: PathBuf::from("a.rs"),
                content: "fn a() {}".into(),
            },
            VfsUpdate::Write {
                path: PathBuf::from("b.rs"),
                content: "fn b() {}".into(),
            },
        ])
        .unwrap();
        assert_eq!(vfs.len(), 2);
        vfs.apply_updates(vec![VfsUpdate::Delete {
            path: PathBuf::from("a.rs"),
        }])
        .unwrap();
        assert_eq!(vfs.len(), 1);
    }
}
