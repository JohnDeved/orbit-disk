//! Removal projection: hides items the user removed (Trash or delete) from views and search
//! results until the index catches up, and subtracts their totals from the folders above them.
//!
//! Ported from `DiskRemovalProjection.swift`. Rules:
//! - A removal hides a path only while the indexed item there still has the removal's identity;
//!   a replacement at the same path stays visible. Items without an identity are never hidden.
//! - Hiding a folder hides everything below it. A removal below another hidden removal adds no
//!   second subtraction.
//! - Paths are compared as bytes: canonically equivalent but byte-distinct spellings (NFC/NFD)
//!   are different items.
//! - A node whose totals are smaller than what is subtracted is inconsistent with the removals and
//!   is dropped rather than shown with wrapped-around totals.
//!
//! Construction is O(R · depth) hash operations for R removals; projecting a tree or a hit list is
//! O(n · depth) plus the tree's child sort, so 10,000 removals over a 100,000-child folder stay
//! cheap (see `orbit-bench`, `projection/*`).

use crate::view::{FolderTree, FolderTreeBuilder, NodeIx, NodeMeta};
use crate::{CoreError, Result, ScanRemoval};
use orbit_platform::{Cancellation, FileIdentity};
use std::collections::{HashMap, HashSet};

const CHECK_INTERVAL: usize = 256;

/// Size totals a removal subtracts from its ancestors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub size: u64,
    pub allocated: u64,
    pub file_count: u64,
}

impl Totals {
    pub fn of(meta: &NodeMeta) -> Self {
        Self { size: meta.size, allocated: meta.allocated, file_count: meta.file_count }
    }

    /// `self - other`, or `None` if any field would go negative.
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self {
            size: self.size.checked_sub(other.size)?,
            allocated: self.allocated.checked_sub(other.allocated)?,
            file_count: self.file_count.checked_sub(other.file_count)?,
        })
    }

    pub fn saturating_sub(self, other: Self) -> Self {
        Self {
            size: self.size.saturating_sub(other.size),
            allocated: self.allocated.saturating_sub(other.allocated),
            file_count: self.file_count.saturating_sub(other.file_count),
        }
    }

    fn add(&mut self, other: Self) {
        self.size = self.size.saturating_add(other.size);
        self.allocated = self.allocated.saturating_add(other.allocated);
        self.file_count = self.file_count.saturating_add(other.file_count);
    }

    fn apply_to(self, meta: &mut NodeMeta) {
        meta.size = self.size;
        meta.allocated = self.allocated;
        meta.file_count = self.file_count;
    }
}

/// The indexed item currently at a removal's path.
#[derive(Clone, Copy, Debug)]
pub struct RemovalSource<'a> {
    pub path: &'a [u8],
    pub identity: Option<FileIdentity>,
    pub totals: Totals,
}

/// A search hit that can be projected.
pub trait ProjectedHit {
    fn path(&self) -> &[u8];
    fn totals(&self) -> Totals;
    fn set_totals(&mut self, totals: Totals);
}

#[derive(Clone, Debug, Default)]
pub struct RemovalProjection {
    hidden: HashSet<Box<[u8]>>,
    /// Per uncovered ancestor path: what its hidden descendants add up to.
    deltas: HashMap<Box<[u8]>, Totals>,
}

fn check(cancellation: &Cancellation, i: usize) -> Result<()> {
    if i.is_multiple_of(CHECK_INTERVAL) && cancellation.is_cancelled() { Err(CoreError::Cancelled) } else { Ok(()) }
}

fn ancestors(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::successors(crate::paths::parent(path), |p| crate::paths::parent(p))
}

impl RemovalProjection {
    /// `sources` are the indexed items at (some of) the removal paths; removals without a source
    /// are ignored.
    pub fn new<'a>(
        removals: &[ScanRemoval],
        sources: impl IntoIterator<Item = RemovalSource<'a>>,
        cancellation: &Cancellation,
    ) -> Result<Self> {
        let mut nodes: HashMap<&[u8], RemovalSource<'a>> = HashMap::new();
        for (i, source) in sources.into_iter().enumerate() {
            check(cancellation, i)?;
            nodes.insert(source.path, source);
        }
        let mut hidden: HashSet<Box<[u8]>> = HashSet::with_capacity(removals.len());
        for (i, removal) in removals.iter().enumerate() {
            check(cancellation, i)?;
            if nodes.get(removal.path.as_slice()).is_some_and(|n| n.identity == Some(removal.identity)) {
                hidden.insert(removal.path.as_slice().into());
            }
        }
        let mut deltas: HashMap<Box<[u8]>, Totals> = HashMap::new();
        for (i, path) in hidden.iter().enumerate() {
            check(cancellation, i)?;
            if ancestors(path).any(|a| hidden.contains(a)) {
                continue;
            }
            let totals = nodes[&path[..]].totals;
            for ancestor in ancestors(path) {
                deltas.entry(ancestor.into()).or_default().add(totals);
            }
        }
        if cancellation.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        Ok(Self { hidden, deltas })
    }

    pub fn is_empty(&self) -> bool {
        self.hidden.is_empty()
    }

    /// Paths hidden by a matching removal (descendants are hidden implicitly).
    pub fn hidden_paths(&self) -> impl Iterator<Item = &[u8]> {
        self.hidden.iter().map(|p| &p[..])
    }

    /// Every ancestor path whose totals shrink, with the amount (for SQL-side adjustment).
    pub fn deltas(&self) -> impl Iterator<Item = (&[u8], Totals)> {
        self.deltas.iter().map(|(p, t)| (&p[..], *t))
    }

    /// `path` or one of its ancestors is hidden.
    pub fn is_hidden(&self, path: &[u8]) -> bool {
        !self.hidden.is_empty() && (self.hidden.contains(path) || ancestors(path).any(|a| self.hidden.contains(a)))
    }

    /// What is subtracted from the item at `path` (zero if nothing).
    pub fn delta(&self, path: &[u8]) -> Totals {
        self.deltas.get(path).copied().unwrap_or_default()
    }

    /// Projected totals of the item at `path`, or `None` if it is hidden or its totals are
    /// inconsistent with the removals.
    pub fn project(&self, path: &[u8], totals: Totals) -> Option<Totals> {
        if self.is_hidden(path) {
            return None;
        }
        totals.checked_sub(self.delta(path))
    }

    /// Drops hidden or inconsistent hits and adjusts the totals of the rest. Order is preserved;
    /// callers that rank by size re-sort afterwards.
    pub fn retain_hits<H: ProjectedHit>(&self, hits: &mut Vec<H>, cancellation: &Cancellation) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let mut i = 0;
        let mut cancelled = false;
        hits.retain_mut(|hit| {
            cancelled = cancelled || check(cancellation, i).is_err();
            i += 1;
            if cancelled {
                return true;
            }
            match self.project(hit.path(), hit.totals()) {
                Some(totals) => {
                    hit.set_totals(totals);
                    true
                }
                None => false,
            }
        });
        if cancelled { Err(CoreError::Cancelled) } else { Ok(()) }
    }

    /// The tree as it looks without the removed items, or `None` if the folder itself is hidden
    /// or inconsistent. Hidden or inconsistent descendants are dropped with their subtrees;
    /// remaining children are re-sorted (largest first, then name). Ancestor (breadcrumb) totals
    /// saturate at zero instead of failing.
    pub fn project_tree(&self, tree: &FolderTree, cancellation: &Cancellation) -> Result<Option<FolderTree>> {
        if cancellation.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        if self.is_empty() || tree.is_empty() {
            return Ok(Some(tree.clone()));
        }
        let folder_path = tree.folder_path.as_slice();
        let folder = tree.node(FolderTree::FOLDER);
        let Some(totals) = self.project(folder_path, Totals::of(&folder.meta)) else { return Ok(None) };
        let mut meta = folder.meta;
        totals.apply_to(&mut meta);
        let mut builder =
            FolderTreeBuilder::new(tree.root_path.clone(), folder_path.to_vec(), tree.name(FolderTree::FOLDER), meta);
        for &ancestor in &tree.ancestors {
            let mut meta = tree.node(ancestor).meta;
            Totals::of(&meta).saturating_sub(self.delta(&tree.path(ancestor))).apply_to(&mut meta);
            builder.push_ancestor(tree.name(ancestor), meta);
        }
        let mut path = folder_path.to_vec();
        let mut visited = 0;
        self.project_children(
            tree,
            FolderTree::FOLDER,
            FolderTree::FOLDER,
            &mut path,
            &mut builder,
            &mut visited,
            cancellation,
        )?;
        Ok(Some(builder.finish(tree.revision, tree.is_listing, tree.source)))
    }

    #[allow(clippy::too_many_arguments)]
    fn project_children(
        &self,
        tree: &FolderTree,
        old: NodeIx,
        new: NodeIx,
        path: &mut Vec<u8>,
        builder: &mut FolderTreeBuilder,
        visited: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<()> {
        let mut removed = 0u32;
        for &child in tree.children(old) {
            check(cancellation, *visited)?;
            *visited += 1;
            let len = path.len();
            if path.last() != Some(&crate::paths::SEP) {
                path.push(crate::paths::SEP);
            }
            path.extend_from_slice(tree.name(child));
            let node = tree.node(child);
            let projected = if self.hidden.contains(path.as_slice()) {
                None
            } else {
                Totals::of(&node.meta).checked_sub(self.delta(path))
            };
            if let Some(totals) = projected {
                let mut meta = node.meta;
                totals.apply_to(&mut meta);
                let ix = builder.push_child(new, tree.name(child), meta);
                builder.set_total_children(ix, node.total_children);
                self.project_children(tree, child, ix, path, builder, visited, cancellation)?;
            } else {
                removed += 1;
            }
            path.truncate(len);
        }
        builder.set_total_children(new, tree.node(old).total_children.saturating_sub(removed));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::ViewSource;
    use std::sync::Arc;

    fn id(n: u64) -> FileIdentity {
        FileIdentity { dev: 1, ino: n }
    }

    /// A node of a test tree: (name, identity, own size for files / children for folders).
    enum Fixture {
        File(&'static str, Option<u64>, u64),
        Dir(&'static str, u64, Vec<Fixture>),
    }

    fn totals(f: &Fixture) -> Totals {
        match f {
            Fixture::File(_, _, size) => Totals { size: *size, allocated: size * 2, file_count: 1 },
            Fixture::Dir(_, _, children) => {
                let mut sum = Totals::default();
                children.iter().for_each(|c| sum.add(totals(c)));
                sum
            }
        }
    }

    fn meta(f: &Fixture) -> NodeMeta {
        let (is_dir, identity) = match f {
            Fixture::File(_, identity, _) => (false, identity.map(id)),
            Fixture::Dir(_, identity, _) => (true, Some(id(*identity))),
        };
        let mut meta = NodeMeta { identity, ..NodeMeta::default() };
        meta.flags.is_dir = is_dir;
        totals(f).apply_to(&mut meta);
        meta
    }

    fn add(b: &mut FolderTreeBuilder, parent: NodeIx, f: &Fixture) {
        let name = match f {
            Fixture::File(n, ..) | Fixture::Dir(n, ..) => n.as_bytes(),
        };
        let ix = b.push_child(parent, name, meta(f));
        if let Fixture::Dir(_, _, children) = f {
            children.iter().for_each(|c| add(b, ix, c));
        }
    }

    /// Tree for folder `/r` (children of `root`), plus sources for every node.
    type Source = (Vec<u8>, Option<FileIdentity>, Totals);

    fn build(root: &Fixture) -> (FolderTree, Vec<Source>) {
        let mut b = FolderTreeBuilder::new(Arc::from(&b"/r"[..]), b"/r".to_vec(), b"r", meta(root));
        if let Fixture::Dir(_, _, children) = root {
            children.iter().for_each(|c| add(&mut b, FolderTree::FOLDER, c));
        }
        let tree = b.finish(7, true, ViewSource::Stage);
        let sources = (0..tree.len() as NodeIx)
            .map(|ix| (tree.path(ix), tree.node(ix).meta.identity, Totals::of(&tree.node(ix).meta)))
            .collect();
        (tree, sources)
    }

    fn projection(removals: &[ScanRemoval], sources: &[Source]) -> RemovalProjection {
        let sources =
            sources.iter().map(|(path, identity, totals)| RemovalSource { path, identity: *identity, totals: *totals });
        RemovalProjection::new(removals, sources, &Cancellation::new()).unwrap()
    }

    fn removal(path: &str, identity: u64) -> ScanRemoval {
        ScanRemoval { path: path.as_bytes().to_vec(), identity: id(identity) }
    }

    fn find(tree: &FolderTree, path: &str) -> Option<NodeIx> {
        (0..tree.len() as NodeIx).find(|ix| !tree.ancestors.contains(ix) && tree.path(*ix) == path.as_bytes())
    }

    fn fixture() -> Fixture {
        Fixture::Dir(
            "r",
            1,
            vec![
                Fixture::Dir(
                    "group",
                    2,
                    vec![Fixture::File("victim", Some(3), 10), Fixture::File("retained", Some(4), 20)],
                ),
                Fixture::File("group-old", Some(5), 5),
                Fixture::File("unknown", None, 7),
            ],
        )
    }

    #[test]
    fn identity_must_match_and_ancestors_are_subtracted_once() {
        let (tree, sources) = build(&fixture());
        let p = projection(
            &[
                removal("/r/group/victim", 3),
                removal("/r/group/victim", 3),
                removal("/r/group/retained", 99), // replaced since removal
                removal("/r/missing", 9),
                removal("/r/unknown", 0), // no indexed identity: never hidden
            ],
            &sources,
        );
        let projected = p.project_tree(&tree, &Cancellation::new()).unwrap().unwrap();
        let root = projected.node(FolderTree::FOLDER);
        assert_eq!(Totals::of(&root.meta), Totals { size: 32, allocated: 64, file_count: 3 });
        assert_eq!((projected.revision, projected.is_listing, projected.source), (7, true, ViewSource::Stage));
        let group = find(&projected, "/r/group").unwrap();
        assert_eq!(projected.node(group).meta.size, 20);
        assert_eq!(projected.children(group).iter().map(|c| projected.name(*c)).collect::<Vec<_>>(), [b"retained"]);
        assert_eq!(projected.node(group).total_children, 1);
        assert!(find(&projected, "/r/group-old").is_some(), "sibling-prefix path was hidden");
        assert!(find(&projected, "/r/unknown").is_some());

        // Parent and child removed together: subtracted once.
        let both = projection(&[removal("/r/group", 2), removal("/r/group/victim", 3)], &sources);
        let projected = both.project_tree(&tree, &Cancellation::new()).unwrap().unwrap();
        assert_eq!(
            Totals::of(&projected.node(FolderTree::FOLDER).meta),
            Totals { size: 12, allocated: 24, file_count: 2 }
        );
        assert_eq!(projected.children(FolderTree::FOLDER).len(), 2);
        assert!(both.is_hidden(b"/r/group/retained") && !both.is_hidden(b"/r/group-old"));

        // Removing the folder itself hides the whole view.
        let whole = projection(&[removal("/r", 1)], &sources);
        assert!(whole.project_tree(&tree, &Cancellation::new()).unwrap().is_none());
    }

    #[test]
    fn unicode_spellings_are_distinct_and_inconsistent_totals_are_rejected() {
        let nfc = "/p/\u{e9}/child";
        let nfd = "/p/e\u{301}/child";
        let t = |size| Totals { size, allocated: size, file_count: 1 };
        let sources = [(nfc.as_bytes().to_vec(), Some(id(1)), t(13)), (nfd.as_bytes().to_vec(), Some(id(2)), t(29))];
        let p = projection(&[removal(nfc, 1)], &sources);
        assert_eq!(p.project("/p/\u{e9}".as_bytes(), t(13)), Some(Totals { size: 0, allocated: 0, file_count: 0 }));
        assert_eq!(p.project("/p/e\u{301}".as_bytes(), t(29)), Some(t(29)));
        assert_eq!(p.project(nfd.as_bytes(), t(29)), Some(t(29)));
        assert_eq!(p.project(nfc.as_bytes(), t(13)), None);
        for inconsistent in [
            Totals { size: 12, allocated: 13, file_count: 1 },
            Totals { size: 13, allocated: 12, file_count: 1 },
            Totals { size: 13, allocated: 13, file_count: 0 },
        ] {
            assert_eq!(p.project(b"/p", inconsistent), None, "projection allowed a negative total");
        }
        let root = projection(&[removal("/p", 3)], &[(b"/p".to_vec(), Some(id(3)), t(42))]);
        assert!(root.is_hidden(nfd.as_bytes()), "root removal did not cover descendants");
    }

    struct Hit(Vec<u8>, Totals);
    impl ProjectedHit for Hit {
        fn path(&self) -> &[u8] {
            &self.0
        }
        fn totals(&self) -> Totals {
            self.1
        }
        fn set_totals(&mut self, totals: Totals) {
            self.1 = totals;
        }
    }

    /// Random trees and removals (a fifth with a stale identity): the projected tree equals the
    /// tree rebuilt from surviving files, no total wraps, and mismatched identities never hide.
    #[test]
    fn projected_totals_equal_surviving_files() {
        let mut seed = 0x1234_5678_u64;
        let mut rand = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..40 {
            let mut next_id = 10;
            let mut make = |depth: u32, rand: &mut dyn FnMut(u64) -> u64| -> Vec<Fixture> {
                fn level(depth: u32, next: &mut u64, rand: &mut dyn FnMut(u64) -> u64) -> Vec<Fixture> {
                    const NAMES: [&str; 6] = ["a", "b", "ab", "a-b", "\u{e9}", "e\u{301}"];
                    (0..rand(5) as usize)
                        .map(|i| {
                            *next += 1;
                            if depth > 0 && rand(2) == 0 {
                                Fixture::Dir(NAMES[i], *next, level(depth - 1, next, rand))
                            } else {
                                Fixture::File(NAMES[i], Some(*next), 1 + rand(100))
                            }
                        })
                        .collect()
                }
                level(depth, &mut next_id, rand)
            };
            let root = Fixture::Dir("r", 1, make(3, &mut rand));
            let (tree, sources) = build(&root);
            let mut removals = Vec::new();
            let mut stale = HashSet::new();
            for (path, identity, _) in &sources {
                if rand(4) == 0 && path.as_slice() != b"/r" {
                    let identity = identity.unwrap();
                    if rand(5) == 0 {
                        stale.insert(path.clone());
                        removals.push(ScanRemoval {
                            path: path.clone(),
                            identity: FileIdentity { ino: identity.ino + 1000, ..identity },
                        });
                    } else {
                        removals.push(ScanRemoval { path: path.clone(), identity });
                    }
                }
            }
            let p = projection(&removals, &sources);
            let projected = p.project_tree(&tree, &Cancellation::new()).unwrap().unwrap();

            // Expected: every file not at or below a matching removal survives.
            let removed: Vec<&[u8]> =
                removals.iter().filter(|r| !stale.contains(&r.path)).map(|r| r.path.as_slice()).collect();
            let survives = |path: &[u8]| !removed.iter().any(|r| crate::paths::is_within(path, r));
            let mut expected: HashMap<Vec<u8>, Totals> = HashMap::new();
            for (path, _, totals) in sources.iter().filter(|(p, ..)| survives(p)) {
                expected.entry(path.clone()).or_default();
                if find(&tree, std::str::from_utf8(path).unwrap()).is_some_and(|ix| !tree.node(ix).meta.flags.is_dir) {
                    for a in
                        std::iter::once(&path[..]).chain(ancestors(path)).filter(|a| crate::paths::is_within(a, b"/r"))
                    {
                        expected.entry(a.to_vec()).or_default().add(*totals);
                    }
                }
            }
            let got: HashMap<Vec<u8>, Totals> = (0..projected.len() as NodeIx)
                .map(|ix| (projected.path(ix), Totals::of(&projected.node(ix).meta)))
                .collect();
            assert_eq!(got, expected);
            for path in &stale {
                assert_eq!(got.contains_key(path), survives(path), "a stale identity changed visibility");
            }

            let mut hits: Vec<Hit> = sources.iter().map(|(p, _, t)| Hit(p.clone(), *t)).collect();
            p.retain_hits(&mut hits, &Cancellation::new()).unwrap();
            assert_eq!(hits.iter().map(|h| (h.0.clone(), h.1)).collect::<HashMap<_, _>>(), expected);
        }
    }

    #[test]
    fn projection_honours_cancellation() {
        let (tree, sources) = build(&fixture());
        let cancelled = Cancellation::new();
        cancelled.cancel();
        let removals = [removal("/r/group/victim", 3)];
        let sources_iter =
            sources.iter().map(|(path, identity, totals)| RemovalSource { path, identity: *identity, totals: *totals });
        assert!(RemovalProjection::new(&removals, sources_iter, &cancelled).unwrap_err().is_cancelled());
        let p = projection(&removals, &sources);
        assert!(p.project_tree(&tree, &cancelled).unwrap_err().is_cancelled());
        let mut hits = vec![Hit(b"/r/group/victim".to_vec(), Totals::default())];
        assert!(p.retain_hits(&mut hits, &cancelled).unwrap_err().is_cancelled());
    }
}
