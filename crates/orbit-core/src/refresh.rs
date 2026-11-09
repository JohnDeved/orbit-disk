//! Plans which indexed directories to rescan for a batch of filesystem change notifications.
//!
//! Ported from `IndexRefreshPlanner.swift` (`refreshDirectories`). For each changed path:
//! 1. Outside the mapped root: ignored.
//! 2. Not an indexed directory (new or removed item): the nearest indexed ancestor folder.
//! 3. Walking down from the root to that folder, the first ancestor that is no longer a plain
//!    directory on the same device (replaced by a symlink or file, now a mount point, or indexed
//!    as unreadable) stops the walk and its *parent* is refreshed instead. A symlinked ancestor is
//!    therefore refreshed as a folder entry and never followed.
//!
//! The results are then coalesced: a path is dropped only when a strict ancestor (by whole
//! components, so `/r/ab` is not under `/r/a`) is also refreshed.
//!
//! Everything the planner needs from the index and the filesystem comes through
//! [`RefreshFacts`], so it stays pure. Index lookups are cached per call with byte-exact keys
//! (NFC and NFD spellings are different paths); filesystem checks are not cached.

use crate::paths::{SEP, is_within};
use crate::policy::standardize;
use crate::{CoreError, Result};
use orbit_platform::Cancellation;
use std::collections::{HashMap, HashSet};

/// What the snapshot being refreshed says about a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexedNode {
    pub is_dir: bool,
    pub accessible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    Symlink,
    Other,
}

/// `lstat` result: never follows a final symlink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathStat {
    pub kind: FileKind,
    pub dev: u64,
}

pub trait RefreshFacts {
    /// The indexed item at `path` in the snapshot being refreshed. Must fail with
    /// [`CoreError::SnapshotChanged`] once that snapshot has been superseded.
    fn indexed(&mut self, path: &[u8]) -> Result<Option<IndexedNode>>;
    /// `lstat(path)`, or `None` if it does not exist or cannot be read.
    fn lstat(&mut self, path: &[u8]) -> Option<PathStat>;
    /// Mount point of the filesystem holding the directory `path` (`statfs` `f_mntonname`).
    fn mount_path(&mut self, path: &[u8]) -> Result<Vec<u8>>;
    /// `path` with every symlink resolved (the input itself if that fails).
    fn canonicalize(&mut self, path: &[u8]) -> Vec<u8>;
}

fn io_error(kind: std::io::ErrorKind, path: &[u8], what: &str) -> CoreError {
    CoreError::Io(std::io::Error::new(kind, format!("{}: {what}", String::from_utf8_lossy(path))))
}

struct Planner<'a, F> {
    facts: &'a mut F,
    cancellation: &'a Cancellation,
    root: Vec<u8>,
    indexed: HashMap<Vec<u8>, Option<IndexedNode>>,
}

impl<F: RefreshFacts> Planner<'_, F> {
    fn check(&self) -> Result<()> {
        if self.cancellation.is_cancelled() { Err(CoreError::Cancelled) } else { Ok(()) }
    }

    fn indexed(&mut self, path: &[u8]) -> Result<Option<IndexedNode>> {
        self.check()?;
        if let Some(cached) = self.indexed.get(path) {
            return Ok(*cached);
        }
        let value = self.facts.indexed(path)?;
        self.indexed.insert(path.to_vec(), value);
        Ok(value)
    }

    /// The parent directory, clamped to the root.
    fn parent<'p>(&'p self, path: &'p [u8]) -> &'p [u8] {
        match crate::paths::parent(path) {
            Some(parent) if is_within(parent, &self.root) => parent,
            _ => &self.root,
        }
    }
}

/// Directories to refresh for `paths` under the mapped `root`, in ascending (length, bytes)
/// order with no entry inside another.
pub fn plan_refresh<'p>(
    paths: impl IntoIterator<Item = &'p [u8]>,
    root: &[u8],
    facts: &mut impl RefreshFacts,
    cancellation: &Cancellation,
) -> Result<Vec<Vec<u8>>> {
    if cancellation.is_cancelled() {
        return Err(CoreError::Cancelled);
    }
    let root = standardize(root).ok_or_else(|| CoreError::Invalid("The mapped folder path is not absolute.".into()))?;
    let canonical_root = facts.canonicalize(&root);
    let root_stat = facts.lstat(&root).ok_or_else(|| io_error(std::io::ErrorKind::NotFound, &root, "not found"))?;
    if root_stat.kind != FileKind::Directory {
        return Err(io_error(std::io::ErrorKind::NotADirectory, &root, "not a directory"));
    }
    let mut planner = Planner { facts, cancellation, root, indexed: HashMap::new() };

    let mut candidates: HashSet<Vec<u8>> = HashSet::new();
    for original in paths {
        planner.check()?;
        let Some(original) = standardize(original) else { continue };
        if !is_within(&original, &planner.root) {
            continue;
        }
        let mut candidate = original;
        while planner.indexed(&candidate)?.is_none_or(|n| !n.is_dir) {
            if candidate == planner.root {
                return Err(io_error(std::io::ErrorKind::NotFound, &candidate, "the mapped folder is not indexed"));
            }
            candidate = planner.parent(&candidate).to_vec();
        }

        // Walk down from the root's child to the candidate; stop at the first ancestor that is
        // no longer a plain, readable directory of the mapped filesystem.
        let mut chain = Vec::new();
        let mut current = candidate.as_slice();
        while current != planner.root.as_slice() {
            chain.push(current.to_vec());
            current = planner.parent(current);
        }
        for path in chain.iter().rev() {
            let replaced = if planner.indexed(path)?.is_some_and(|n| !n.accessible) {
                true
            } else {
                match planner.facts.lstat(path) {
                    Some(stat) if stat.kind == FileKind::Directory && stat.dev == root_stat.dev => {
                        let suffix = if planner.root == b"/" { &path[..] } else { &path[planner.root.len()..] };
                        let mut physical = if canonical_root == b"/" { Vec::new() } else { canonical_root.clone() };
                        physical.extend_from_slice(suffix);
                        planner.facts.mount_path(path)? == physical
                    }
                    _ => true,
                }
            };
            if replaced {
                candidate = planner.parent(path).to_vec();
                break;
            }
        }
        candidates.insert(candidate);
    }

    let mut sorted: Vec<Vec<u8>> = candidates.into_iter().collect();
    sorted.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let mut selected: HashSet<&[u8]> = HashSet::new();
    let mut minimal = Vec::new();
    for path in &sorted {
        planner.check()?;
        let mut current = path.as_slice();
        let mut covered = false;
        while current != planner.root.as_slice() {
            current = planner.parent(current);
            if selected.contains(current) {
                covered = true;
                break;
            }
        }
        if !covered {
            selected.insert(path);
            minimal.push(path.clone());
        }
    }
    debug_assert!(minimal.iter().all(|p| p.first() == Some(&SEP)));
    Ok(minimal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        /// path -> (is_dir, accessible)
        index: HashMap<Vec<u8>, IndexedNode>,
        /// path -> lstat; anything absent is missing.
        disk: HashMap<Vec<u8>, PathStat>,
        mounts: HashMap<Vec<u8>, Vec<u8>>,
        stale: bool,
        index_reads: Vec<Vec<u8>>,
        disk_reads: Vec<Vec<u8>>,
    }

    impl Fake {
        fn dir(mut self, path: &str) -> Self {
            self.index.insert(path.into(), IndexedNode { is_dir: true, accessible: true });
            self.disk.insert(path.into(), PathStat { kind: FileKind::Directory, dev: 1 });
            self
        }
    }

    impl RefreshFacts for Fake {
        fn indexed(&mut self, path: &[u8]) -> Result<Option<IndexedNode>> {
            if self.stale {
                return Err(CoreError::SnapshotChanged);
            }
            self.index_reads.push(path.to_vec());
            Ok(self.index.get(path).copied())
        }
        fn lstat(&mut self, path: &[u8]) -> Option<PathStat> {
            self.disk_reads.push(path.to_vec());
            self.disk.get(path).copied()
        }
        fn mount_path(&mut self, path: &[u8]) -> Result<Vec<u8>> {
            self.disk_reads.push(path.to_vec());
            Ok(self.mounts.get(path).cloned().unwrap_or_else(|| b"/".to_vec()))
        }
        fn canonicalize(&mut self, path: &[u8]) -> Vec<u8> {
            path.to_vec()
        }
    }

    fn plan(fake: &mut Fake, paths: &[&str]) -> Result<Vec<String>> {
        plan_refresh(paths.iter().map(|p| p.as_bytes()), b"/r", fake, &Cancellation::new())
            .map(|v| v.into_iter().map(|p| String::from_utf8(p).unwrap()).collect())
    }

    fn fixture() -> Fake {
        Fake::default().dir("/r").dir("/r/a").dir("/r/a/child").dir("/r/ab")
    }

    #[test]
    fn coalesces_true_descendants_and_refreshes_nearest_indexed_folder() {
        let mut fake = fixture();
        assert_eq!(
            plan(&mut fake, &["/r/a/child", "/r/a", "/r/ab", "/outside", "/r/a/../ab/"]).unwrap(),
            ["/r/a", "/r/ab"]
        );
        assert_eq!(plan(&mut fixture(), &["/r/a/child/new/file"]).unwrap(), ["/r/a/child"]);
        assert_eq!(plan(&mut fixture(), &["/r/gone", "/r/a"]).unwrap(), ["/r"], "the root covers everything");
    }

    #[test]
    fn replaced_ancestor_is_refreshed_as_a_folder_and_never_followed() {
        let mut fake = fixture();
        fake.disk.insert(b"/r/a/child".to_vec(), PathStat { kind: FileKind::Symlink, dev: 1 });
        assert_eq!(plan(&mut fake, &["/r/a/child/deeper/file"]).unwrap(), ["/r/a"]);
        assert!(
            fake.disk_reads.iter().all(|p| !is_within(p, b"/r/a/child") || p == b"/r/a/child"),
            "the planner looked through the replaced folder: {:?}",
            fake.disk_reads
        );

        // Same for a folder on another device, a mount point, or one indexed as unreadable.
        let mut moved = fixture();
        moved.disk.insert(b"/r/a".to_vec(), PathStat { kind: FileKind::Directory, dev: 2 });
        assert_eq!(plan(&mut moved, &["/r/a/child"]).unwrap(), ["/r"]);
        let mut mounted = fixture();
        mounted.mounts.insert(b"/r/a/child".to_vec(), b"/r/a/child".to_vec());
        assert_eq!(plan(&mut mounted, &["/r/a/child"]).unwrap(), ["/r/a"]);
        let mut unreadable = fixture();
        unreadable.index.insert(b"/r/a".to_vec(), IndexedNode { is_dir: true, accessible: false });
        assert_eq!(plan(&mut unreadable, &["/r/a/child"]).unwrap(), ["/r"]);
    }

    #[test]
    fn rejects_cancellation_stale_snapshots_and_bad_roots() {
        let cancelled = Cancellation::new();
        cancelled.cancel();
        let result = plan_refresh([&b"/r/a"[..]], b"/r", &mut fixture(), &cancelled);
        assert!(result.unwrap_err().is_cancelled());
        let mut stale = fixture();
        stale.stale = true;
        assert!(matches!(plan(&mut stale, &["/r/a"]), Err(CoreError::SnapshotChanged)));
        let mut linked = fixture();
        linked.disk.insert(b"/r".to_vec(), PathStat { kind: FileKind::Symlink, dev: 1 });
        assert!(plan(&mut linked, &["/r/a"]).is_err(), "a symlinked root was followed");
    }

    #[test]
    fn index_lookups_are_cached_with_byte_exact_keys() {
        let nfc = "/r/\u{e9}";
        let nfd = "/r/e\u{301}";
        let mut fake = fixture().dir(nfd);
        assert_eq!(plan(&mut fake, &[nfc, nfd, nfc, nfd]).unwrap(), ["/r"]);
        let reads = |p: &str| fake.index_reads.iter().filter(|r| r.as_slice() == p.as_bytes()).count();
        assert_eq!((reads(nfc), reads(nfd)), (1, 1), "lookups were repeated or keys merged");
    }
}
