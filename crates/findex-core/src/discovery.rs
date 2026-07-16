use ignore::WalkBuilder;
use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::Mmap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DiscoveryError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ignore error: {0}")]
    Ignore(#[from] ignore::Error),
    #[error(transparent)]
    Cancelled(#[from] crate::cancellation::Cancelled),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub hash: [u8; 32], // Blake3 hash
    pub size: u64,
}

/// Discovers files in the target directory, respecting `.gitignore` and filtering by supported extensions.
/// Uses Rayon to parallelize reading and hashing the file contents.
pub fn discover_files<P: AsRef<Path>>(root_dir: P) -> Result<Vec<DiscoveredFile>, DiscoveryError> {
    let root = root_dir.as_ref();
    let mut walk = WalkBuilder::new(root);
    let include_generated = std::env::var("FINDEX_INCLUDE_GENERATED")
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    // Explicitly configure walker options
    walk.hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(move |entry| {
            include_generated
                || !entry.file_type().is_some_and(|kind| kind.is_dir())
                || !is_generated_directory(entry.path())
        });

    let walker = walk.build();
    let mut paths = Vec::new();

    for entry in walker {
        crate::cancellation::checkpoint()?;
        let entry = entry.map_err(DiscoveryError::Ignore)?;
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if is_supported_extension(ext) {
                    paths.push(path.to_path_buf());
                }
            }
        }
    }

    // Parallel processing with Rayon: read files (using memmap2) and compute Blake3 hashes
    let cancellation = crate::cancellation::inherited_token();
    let discovered_files: Vec<Result<DiscoveredFile, DiscoveryError>> = paths
        .into_par_iter()
        .map(|path| {
            crate::cancellation::checkpoint_token(cancellation.as_ref())?;
            let file = File::open(&path)?;
            let metadata = file.metadata()?;
            let size = metadata.len();

            let hash = if size == 0 {
                // memmap2 fails on empty files, so we return a default Blake3 hash for empty input
                *blake3::hash(&[]).as_bytes()
            } else {
                // SAFETY: memmap2 is safe for read-only maps as long as the file is not
                // truncated or modified concurrently by another process. We only read the
                // mapped bytes and compute a hash; the mapping is dropped before any writes.
                let mmap = unsafe { Mmap::map(&file)? };
                *blake3::hash(&mmap).as_bytes()
            };

            Ok(DiscoveredFile { path, hash, size })
        })
        .collect();

    // Collect results and propagate errors
    let mut results = Vec::new();
    for res in discovered_files {
        crate::cancellation::checkpoint()?;
        results.push(res?);
    }

    Ok(results)
}

/// Directories whose contents are generated, downloaded, or belong to Findex itself.
///
/// Git ignore files are not guaranteed to exist (for example, an unpacked source archive),
/// so these high-cost trees are excluded independently. Set `FINDEX_INCLUDE_GENERATED=1`
/// for the rare case where generated or vendored dependency sources are intentional input.
fn is_generated_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    matches!(
        name.to_ascii_lowercase().as_str(),
        "node_modules"
            | "target"
            | "dist"
            | "coverage"
            | ".next"
            | ".nuxt"
            | ".svelte-kit"
            | ".venv"
            | "venv"
            | "__pycache__"
            | ".findex"
            | ".findex_db"
    )
}

fn is_supported_extension(ext: &str) -> bool {
    // JavaScript/TypeScript extensions are handled by the oxc-based parser.
    // All other supported extensions are registered in the parser registry.
    matches!(
        ext,
        "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "jsx" | "tsx" | "vue"
    ) || crate::parser::registry::is_supported_extension(ext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;
    use tempfile::tempdir;

    #[test]
    fn test_file_discovery_and_hashing() {
        let dir = tempdir().unwrap();
        let file_path1 = dir.path().join("main.rs");
        let file_path2 = dir.path().join("index.js");
        let file_path3 = dir.path().join("ignored.txt"); // Not in supported extensions list

        write(&file_path1, b"fn main() {}").unwrap();
        write(&file_path2, b"console.log('hello');").unwrap();
        write(&file_path3, b"some text").unwrap();

        let files = discover_files(dir.path()).unwrap();

        assert_eq!(files.len(), 2);

        let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
        assert!(paths.contains(&file_path1));
        assert!(paths.contains(&file_path2));
        assert!(!paths.contains(&file_path3));

        // Check hash
        let main_rs_file = files.iter().find(|f| f.path == file_path1).unwrap();
        assert_eq!(main_rs_file.hash, *blake3::hash(b"fn main() {}").as_bytes());
        assert_eq!(main_rs_file.size, 12);
    }

    #[test]
    fn excludes_generated_dependency_and_index_trees_without_gitignore() {
        let dir = tempdir().unwrap();
        let source_dir = dir.path().join("src");
        let generated_dirs = [
            dir.path().join("node_modules/package"),
            dir.path().join("target/debug/build"),
            dir.path().join(".venv/site-packages"),
            dir.path().join(".findex_db/cache"),
        ];

        std::fs::create_dir_all(&source_dir).unwrap();
        write(source_dir.join("target.rs"), b"fn real_source() {}").unwrap();
        for generated in &generated_dirs {
            std::fs::create_dir_all(generated).unwrap();
            write(generated.join("generated.rs"), b"fn generated() {}").unwrap();
        }

        let files = discover_files(dir.path()).unwrap();
        let paths: Vec<_> = files.iter().map(|file| &file.path).collect();

        assert_eq!(files.len(), 1);
        assert!(paths.contains(&&source_dir.join("target.rs")));
    }
}
