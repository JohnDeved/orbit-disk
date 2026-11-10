//! Plans which indexed directories to rescan for a batch of filesystem change notifications.
//!
//! Ported from `IndexRefreshPlanner.swift` (`refreshDirectories`). For each changed path:
//! 1. Outside the mapped root: ignored.
//! 2. Not an indexed directory (new or removed item): the nearest indexed ancestor folder.
//! 3. Walking down from the root to that folder, the first ancestor that is no longer a plain
//!    directory on the same device (replaced by a symlink or file, now a mount point) stops the
//!    walk and its *parent* is reconciled recursively instead. A symlinked ancestor is therefore
//!    refreshed as a folder entry and never followed. An ancestor that is gone, or indexed as
//!    unreadable, also stops the walk, but only its parent's listing is repeated (it prunes a
//!    removed folder and retries an unreadable one). A change below a folder indexed as another
//!    volume's mount point is outside the map and ignored.
//!
//! Each path is either a listing (the folder's own entries changed) or a subtree (reconcile it
//! recursively); a replaced ancestor always makes its parent a subtree. The results are then
//! coalesced: a subtree absorbs any path strictly inside it (by whole components, so `/r/ab` is
//! not under `/r/a`) and a listing of itself; a listing absorbs only a duplicate listing.
//!
//! Everything the planner needs from the index and the filesystem comes through
//! [`RefreshFacts`], so it stays pure. Index lookups are cached per call with byte-exact keys
//! (NFC and NFD spellings are different paths); filesystem checks are not cached.

use crate::paths::standardize;
use crate::paths::{SEP, is_within};
use crate::{CoreError, Result};
use orbit_platform::Cancellation;
use std::collections::{HashMap, HashSet};

/// What the snapshot being refreshed says about a path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexedNode {
    pub is_dir: bool,
    pub accessible: bool,
    /// Indexed as the mount point of another volume (the map does not descend into it).
    pub mount_point: bool,
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

/// Directories to refresh when every path in `paths` asks for a recursive reconciliation: the
/// planned subtrees, then any listings, each in ascending (length, bytes) order. Tests only: real
/// callers need the kinds ([`plan_changes`]).
#[cfg(test)]
fn plan_refresh<'p>(
    paths: impl IntoIterator<Item = &'p [u8]>,
    root: &[u8],
    facts: &mut impl RefreshFacts,
    cancellation: &Cancellation,
) -> Result<Vec<Vec<u8>>> {
    let plan = plan_changes(std::iter::empty(), paths, root, facts, cancellation)?;
    Ok(plan.subtrees.into_iter().chain(plan.listings).collect())
}

/// What a refresh re-reads: folders whose listings are repeated and folders reconciled
/// recursively. Each list is in ascending (length, bytes) order. No subtree lies inside another,
/// and no listing repeats one or lies inside a subtree; a listing may contain other listings
/// and subtrees (re-listing a folder does not re-read its subfolders).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshPlan {
    pub listings: Vec<Vec<u8>>,
    pub subtrees: Vec<Vec<u8>>,
}

/// Plans a refresh of changed `listings` (folders whose own entries changed) and `subtrees`
/// (folders to reconcile recursively) under the mapped `root`. A path that is not an indexed
/// folder is replaced by its nearest indexed ancestor, keeping its kind. When an ancestor was
/// replaced, that ancestor's parent is reconciled recursively whatever was asked; when an
/// ancestor is indexed as unreadable, only that ancestor's parent is re-listed.
pub fn plan_changes<'p>(
    listings: impl IntoIterator<Item = &'p [u8]>,
    subtrees: impl IntoIterator<Item = &'p [u8]>,
    root: &[u8],
    facts: &mut impl RefreshFacts,
    cancellation: &Cancellation,
) -> Result<RefreshPlan> {
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

    let mut listed: HashSet<Vec<u8>> = HashSet::new();
    let mut reconciled: HashSet<Vec<u8>> = HashSet::new();
    let requests = listings.into_iter().map(|p| (p, false)).chain(subtrees.into_iter().map(|p| (p, true)));
    for (original, recursive) in requests {
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
        let mut recursive = recursive;
        let mut outside = false;
        for path in chain.iter().rev() {
            let node = planner.indexed(path)?;
            if node.is_some_and(|n| !n.accessible) {
                // The map could not read this folder, so nothing inside it can be refreshed (an
                // unreadable folder such as Mail, Safari or the Trash changes constantly). Its
                // parent's listing notices it being removed or replaced, and tries it again.
                candidate = planner.parent(path).to_vec();
                recursive = false;
                break;
            }
            let Some(stat) = planner.facts.lstat(path) else {
                // Removed (or renamed away): the parent's listing prunes it with everything
                // indexed below it.
                candidate = planner.parent(path).to_vec();
                recursive = false;
                break;
            };
            let replaced = if stat.kind != FileKind::Directory {
                true
            } else if stat.dev != root_stat.dev {
                if node.is_some_and(|n| n.mount_point) {
                    // Another volume the map already stops at: the change is not in this map.
                    outside = true;
                    break;
                }
                true
            } else {
                let suffix = if planner.root == b"/" { &path[..] } else { &path[planner.root.len()..] };
                let mut physical = if canonical_root == b"/" { Vec::new() } else { canonical_root.clone() };
                physical.extend_from_slice(suffix);
                planner.facts.mount_path(path)? == physical
            };
            if replaced {
                // The replaced folder and everything indexed below it must be reconciled.
                candidate = planner.parent(path).to_vec();
                recursive = true;
                break;
            }
        }
        if outside {
            continue;
        }
        if recursive {
            reconciled.insert(candidate)
        } else {
            listed.insert(candidate)
        };
    }

    let ordered = |set: HashSet<Vec<u8>>| {
        let mut sorted: Vec<Vec<u8>> = set.into_iter().collect();
        sorted.sort_unstable_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        sorted
    };
    // Subtrees first: a subtree absorbs every request inside it, a listing only its own.
    let mut selected: HashSet<Vec<u8>> = HashSet::new();
    let mut plan = RefreshPlan::default();
    for path in ordered(reconciled) {
        planner.check()?;
        if !planner.inside_any(&path, &selected, false) {
            selected.insert(path.clone());
            plan.subtrees.push(path);
        }
    }
    for path in ordered(listed) {
        planner.check()?;
        if !planner.inside_any(&path, &selected, true) {
            plan.listings.push(path);
        }
    }
    debug_assert!(plan.listings.iter().chain(&plan.subtrees).all(|p| p.first() == Some(&SEP)));
    Ok(plan)
}

impl<F: RefreshFacts> Planner<'_, F> {
    /// Whether a strict ancestor of `path` (or `path` itself, with `inclusive`) is in `set`.
    fn inside_any(&self, path: &[u8], set: &HashSet<Vec<u8>>, inclusive: bool) -> bool {
        if inclusive && set.contains(path) {
            return true;
        }
        let mut current = path;
        while current != self.root.as_slice() {
            current = self.parent(current);
            if set.contains(current) {
                return true;
            }
        }
        false
    }
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
            self.index.insert(path.into(), IndexedNode { is_dir: true, accessible: true, mount_point: false });
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
        unreadable.index.insert(b"/r/a".to_vec(), IndexedNode { is_dir: true, accessible: false, mount_point: false });
        assert_eq!(plan(&mut unreadable, &["/r/a/child"]).unwrap(), ["/r"]);
        assert_eq!(
            plan_mixed(&mut unreadable, &[], &["/r/a/child"]),
            (vec!["/r".into()], vec![]),
            "nothing below an unreadable folder can be read: only its parent is re-listed"
        );
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

    fn plan_mixed(fake: &mut Fake, listings: &[&str], subtrees: &[&str]) -> (Vec<String>, Vec<String>) {
        let strings = |v: Vec<Vec<u8>>| v.into_iter().map(|p| String::from_utf8(p).unwrap()).collect();
        let plan = plan_changes(
            listings.iter().map(|p| p.as_bytes()),
            subtrees.iter().map(|p| p.as_bytes()),
            b"/r",
            fake,
            &Cancellation::new(),
        )
        .unwrap();
        (strings(plan.listings), strings(plan.subtrees))
    }

    #[test]
    fn listings_coalesce_only_with_themselves_and_enclosing_subtrees() {
        // A listing of the root does not absorb listings or subtrees below it.
        assert_eq!(
            plan_mixed(&mut fixture(), &["/r", "/r/a/child", "/r/a/child"], &["/r/ab"]),
            (vec!["/r".into(), "/r/a/child".into()], vec!["/r/ab".into()])
        );
        // A subtree absorbs listings inside it and of itself.
        assert_eq!(
            plan_mixed(&mut fixture(), &["/r/a", "/r/a/child", "/r/ab"], &["/r/a"]),
            (vec!["/r/ab".into()], vec!["/r/a".into()])
        );
        // An unindexed path is listed as its nearest indexed folder: a new folder is found there.
        assert_eq!(plan_mixed(&mut fixture(), &["/r/a/child/new"], &[]), (vec!["/r/a/child".into()], vec![]));
        // Root-level changes stay a listing of the root.
        assert_eq!(plan_mixed(&mut fixture(), &["/r", "/r/gone"], &[]), (vec!["/r".into()], vec![]));
    }

    #[test]
    fn a_replaced_ancestor_makes_its_parent_a_subtree() {
        let mut fake = fixture();
        fake.disk.insert(b"/r/a/child".to_vec(), PathStat { kind: FileKind::Symlink, dev: 1 });
        assert_eq!(plan_mixed(&mut fake, &["/r/a/child"], &[]), (vec![], vec!["/r/a".into()]));
        let mut moved = fixture();
        moved.disk.insert(b"/r/a".to_vec(), PathStat { kind: FileKind::Directory, dev: 2 });
        assert_eq!(
            plan_mixed(&mut moved, &["/r/a/child", "/r/ab"], &[]),
            (vec![], vec!["/r".into()]),
            "a root subtree absorbs every listing"
        );
    }

    /// Changes inside folders the map could not read (Mail, Safari, Biome, the Trash) re-list
    /// only the folder holding them, never that folder's whole subtree.
    #[test]
    fn changes_below_an_unreadable_folder_list_its_parent() {
        let mut fake = fixture().dir("/r/a/child/locked");
        fake.index
            .insert(b"/r/a/child/locked".to_vec(), IndexedNode { is_dir: true, accessible: false, mount_point: false });
        assert_eq!(
            plan_mixed(&mut fake, &["/r/a/child/locked/deep/x", "/r/a/child/locked"], &[]),
            (vec!["/r/a/child".into()], vec![])
        );
    }

    /// A removed folder is pruned by its parent's listing; it never makes the parent recursive.
    #[test]
    fn a_removed_folder_lists_its_parent() {
        let mut fake = fixture();
        fake.disk.remove(b"/r/a".as_slice());
        // Files inside the removed folder (still indexed) report their own folder.
        assert_eq!(plan_mixed(&mut fake, &["/r/a/child"], &[]), (vec!["/r".into()], vec![]));
        assert_eq!(plan_mixed(&mut fake, &[], &["/r/a/child"]), (vec!["/r".into()], vec![]));
    }

    /// Writes on another volume mounted inside the map are not the map's changes.
    #[test]
    fn changes_below_an_indexed_mount_point_are_ignored() {
        let mut fake = fixture().dir("/r/volume");
        fake.index.insert(b"/r/volume".to_vec(), IndexedNode { is_dir: true, accessible: true, mount_point: true });
        fake.disk.insert(b"/r/volume".to_vec(), PathStat { kind: FileKind::Directory, dev: 7 });
        assert_eq!(plan_mixed(&mut fake, &["/r/volume/deep/file", "/r/volume"], &["/r/volume/x"]), (vec![], vec![]));
        // A folder that became a mount point since it was indexed is still reconciled.
        let mut mounted = fixture();
        mounted.disk.insert(b"/r/a".to_vec(), PathStat { kind: FileKind::Directory, dev: 7 });
        assert_eq!(plan_mixed(&mut mounted, &["/r/a/child"], &[]), (vec![], vec!["/r".into()]));
    }
}
