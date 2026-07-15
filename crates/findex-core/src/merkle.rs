//! Deterministic Merkle snapshots for incremental repository indexing.
//!
//! File content hashes are the leaves. Directory hashes include every child
//! name, kind and hash, which makes renames visible and lets diffing stop at an
//! unchanged subtree instead of comparing the entire persisted file map.

use crate::discovery::DiscoveredFile;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MerkleNodeKind {
    Directory,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MerkleNode {
    pub kind: MerkleNodeKind,
    pub hash: [u8; 32],
    #[serde(default)]
    pub children: BTreeMap<String, MerkleNode>,
}

impl MerkleNode {
    fn directory() -> Self {
        Self {
            kind: MerkleNodeKind::Directory,
            hash: [0; 32],
            children: BTreeMap::new(),
        }
    }

    fn file(hash: [u8; 32]) -> Self {
        Self {
            kind: MerkleNodeKind::File,
            hash,
            children: BTreeMap::new(),
        }
    }

    fn recompute(&mut self) {
        if self.kind == MerkleNodeKind::File {
            return;
        }
        for child in self.children.values_mut() {
            child.recompute();
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"findex-merkle-directory-v1\0");
        for (name, child) in &self.children {
            hasher.update(name.as_bytes());
            hasher.update(&[0]);
            hasher.update(&[match child.kind {
                MerkleNodeKind::Directory => b'd',
                MerkleNodeKind::File => b'f',
            }]);
            hasher.update(&child.hash);
        }
        self.hash = *hasher.finalize().as_bytes();
    }

    fn collect_files(&self, prefix: &Path, output: &mut BTreeSet<PathBuf>) {
        match self.kind {
            MerkleNodeKind::File => {
                output.insert(prefix.to_path_buf());
            }
            MerkleNodeKind::Directory => {
                for (name, child) in &self.children {
                    child.collect_files(&prefix.join(name), output);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MerkleSnapshot {
    /// Canonicalized only when the platform permits it; serialized paths stay
    /// portable enough for the same checkout to be reopened.
    pub root_path: PathBuf,
    pub root: MerkleNode,
}

impl MerkleSnapshot {
    pub fn from_files(root_path: &Path, files: &[DiscoveredFile]) -> Self {
        let root_path = root_path
            .canonicalize()
            .unwrap_or_else(|_| root_path.to_path_buf());
        let mut root = MerkleNode::directory();

        for file in files {
            let relative = file
                .path
                .strip_prefix(&root_path)
                .or_else(|_| file.path.strip_prefix(root_path.as_path()))
                .unwrap_or(file.path.as_path());
            let components: Vec<String> = relative
                .components()
                .filter_map(|part| part.as_os_str().to_str().map(ToOwned::to_owned))
                .collect();
            if components.is_empty() {
                continue;
            }

            let mut cursor = &mut root;
            for component in &components[..components.len() - 1] {
                cursor = cursor
                    .children
                    .entry(component.clone())
                    .or_insert_with(MerkleNode::directory);
            }
            cursor.children.insert(
                components.last().expect("non-empty path").clone(),
                MerkleNode::file(file.hash),
            );
        }
        root.recompute();
        Self { root_path, root }
    }

    pub fn root_hash_hex(&self) -> String {
        blake3::Hash::from_bytes(self.root.hash)
            .to_hex()
            .to_string()
    }

    pub fn diff(&self, newer: &Self) -> MerkleDiff {
        let mut diff = MerkleDiff::default();
        diff_nodes(
            Some(&self.root),
            Some(&newer.root),
            Path::new(""),
            &mut diff,
        );
        diff
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MerkleDiff {
    /// Added and content-modified files, relative to the repository root.
    pub changed: BTreeSet<PathBuf>,
    /// Deleted files, relative to the repository root.
    pub deleted: BTreeSet<PathBuf>,
    /// Nodes inspected during comparison. Identical repositories inspect one.
    pub visited_nodes: usize,
}

fn diff_nodes(
    older: Option<&MerkleNode>,
    newer: Option<&MerkleNode>,
    path: &Path,
    diff: &mut MerkleDiff,
) {
    diff.visited_nodes += 1;
    match (older, newer) {
        (Some(a), Some(b)) if a.kind == b.kind && a.hash == b.hash => {}
        (None, Some(node)) => node.collect_files(path, &mut diff.changed),
        (Some(node), None) => node.collect_files(path, &mut diff.deleted),
        (Some(a), Some(b))
            if a.kind == MerkleNodeKind::Directory && b.kind == MerkleNodeKind::Directory =>
        {
            let names: BTreeSet<_> = a.children.keys().chain(b.children.keys()).collect();
            for name in names {
                diff_nodes(
                    a.children.get(name),
                    b.children.get(name),
                    &path.join(name),
                    diff,
                );
            }
        }
        (Some(a), Some(b)) => {
            a.collect_files(path, &mut diff.deleted);
            b.collect_files(path, &mut diff.changed);
        }
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: &[u8]) -> DiscoveredFile {
        DiscoveredFile {
            path: PathBuf::from("repo").join(path),
            hash: *blake3::hash(content).as_bytes(),
            size: content.len() as u64,
        }
    }

    #[test]
    fn identical_snapshots_stop_at_the_root() {
        let files = vec![file("src/a.rs", b"a"), file("tests/a.rs", b"test")];
        let a = MerkleSnapshot::from_files(Path::new("repo"), &files);
        let diff = a.diff(&a);
        assert!(diff.changed.is_empty());
        assert!(diff.deleted.is_empty());
        assert_eq!(diff.visited_nodes, 1);
    }

    #[test]
    fn reports_modified_added_and_deleted_leaves() {
        let old = MerkleSnapshot::from_files(
            Path::new("repo"),
            &[file("src/a.rs", b"a"), file("src/deleted.rs", b"old")],
        );
        let new = MerkleSnapshot::from_files(
            Path::new("repo"),
            &[file("src/a.rs", b"changed"), file("src/new.rs", b"new")],
        );
        let diff = old.diff(&new);
        assert!(diff.changed.contains(Path::new("src/a.rs")));
        assert!(diff.changed.contains(Path::new("src/new.rs")));
        assert!(diff.deleted.contains(Path::new("src/deleted.rs")));
    }
}
