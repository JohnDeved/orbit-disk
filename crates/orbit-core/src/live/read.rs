//! Read paths of the live inventory: folder views, removal projection, search and counts.

use super::arena::{Arena, DEAD, LINKED, MOUNT, NONE, NodeId, ROOT};
use super::{LiveInventory, SEARCH_LIMIT_MAX, SEARCH_STRIDE};
use crate::types::{ScanRemoval, ScanState};
use crate::view::{FolderTree, FolderTreeBuilder, NodeFlags, NodeMeta, VIEW_DEPTH, VIEW_DESCENDANT_BUDGET, ViewSource};
use orbit_platform::EntryKind;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// Children loaded per descendant folder (Swift `min(96, budget)`).
const HYDRATE_PER_FOLDER: usize = 96;

pub(crate) fn meta(arena: &Arena, n: NodeId) -> NodeMeta {
    let flags = arena.flags(n);
    let kind = arena.kind(n);
    let identity = Some(arena.identity(n));
    if arena.is_dir(n) {
        let info = arena.dir(n);
        let accessible = info.accessible();
        NodeMeta {
            flags: NodeFlags { is_dir: true, is_symlink: false, is_mount_point: flags & MOUNT != 0, accessible },
            size: arena.size(n),
            allocated: arena.effective_alloc(n),
            raw_allocated: 0,
            file_count: info.files,
            identity,
            state: if accessible { info.state } else { ScanState::Complete },
        }
    } else {
        let parent = arena.parent(n);
        let state = if parent != NONE && arena.dir(parent).state == ScanState::Complete {
            ScanState::Complete
        } else {
            ScanState::Pending
        };
        NodeMeta {
            flags: NodeFlags {
                is_dir: false,
                is_symlink: kind == EntryKind::Symlink,
                is_mount_point: false,
                accessible: true,
            },
            size: arena.size(n),
            allocated: if flags & LINKED != 0 { arena.effective_alloc(n) } else { arena.raw_alloc(n) },
            raw_allocated: arena.raw_alloc(n),
            file_count: 1,
            identity,
            state,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Delta {
    size: u64,
    allocated: u64,
    files: u64,
}

/// Removed items hidden from a read, with the totals they take from their ancestors.
/// An item is hidden only while its identity still matches the removal.
#[derive(Default)]
struct Projection {
    hidden: HashSet<NodeId>,
    deltas: HashMap<NodeId, Delta>,
    hidden_children: HashMap<NodeId, u32>,
}

impl Projection {
    fn new(arena: &Arena, root_path: &[u8], removals: &[ScanRemoval]) -> Self {
        let mut projection = Self::default();
        for removal in removals {
            if let Some(n) = arena.lookup(root_path, &removal.path)
                && arena.identity(n) == removal.identity
            {
                projection.hidden.insert(n);
            }
        }
        let hidden: Vec<NodeId> = projection.hidden.iter().copied().collect();
        for n in hidden {
            let mut ancestors = Vec::new();
            let mut current = arena.parent(n);
            let mut covered = false;
            while current != NONE {
                if projection.hidden.contains(&current) {
                    covered = true;
                    break;
                }
                ancestors.push(current);
                current = arena.parent(current);
            }
            if covered {
                continue;
            }
            let m = meta(arena, n);
            if let Some(&parent) = ancestors.first() {
                *projection.hidden_children.entry(parent).or_default() += 1;
            }
            for a in ancestors {
                let d = projection.deltas.entry(a).or_default();
                d.size += m.size;
                d.allocated += m.allocated;
                d.files += m.file_count;
            }
        }
        projection
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.hidden.is_empty()
    }

    /// `n` itself is hidden (callers walking down from a visible folder need only this).
    #[inline]
    fn hides(&self, n: NodeId) -> bool {
        !self.hidden.is_empty() && self.hidden.contains(&n)
    }

    /// `n` or one of its ancestors is hidden.
    fn covers(&self, arena: &Arena, mut n: NodeId) -> bool {
        if self.hidden.is_empty() {
            return false;
        }
        while n != NONE {
            if self.hidden.contains(&n) {
                return true;
            }
            n = arena.parent(n);
        }
        false
    }

    fn adjust(&self, n: NodeId, meta: &mut NodeMeta) {
        if let Some(d) = self.deltas.get(&n) {
            meta.size = meta.size.saturating_sub(d.size);
            meta.allocated = meta.allocated.saturating_sub(d.allocated);
            meta.file_count = meta.file_count.saturating_sub(d.files);
        }
    }

    fn size(&self, arena: &Arena, n: NodeId) -> u64 {
        let size = arena.size(n);
        if self.deltas.is_empty() {
            return size;
        }
        self.deltas.get(&n).map_or(size, |d| size.saturating_sub(d.size))
    }

    fn visible_children(&self, arena: &Arena, n: NodeId) -> u32 {
        arena.dir(n).children - self.hidden_children.get(&n).copied().unwrap_or(0)
    }
}

pub(super) fn view(live: &LiveInventory, path: &[u8], removals: &[ScanRemoval]) -> Option<FolderTree> {
    let arena = live.arena().read();
    let root_path = live.root_path();
    let folder = arena.lookup(root_path, path)?;
    if !arena.is_dir(folder) {
        return None;
    }
    let projection = Projection::new(&arena, root_path, removals);
    if projection.covers(&arena, folder) {
        return None;
    }
    let node_meta = |n: NodeId| {
        let mut m = meta(&arena, n);
        projection.adjust(n, &mut m);
        m
    };
    let folder_name = arena.name(folder).to_vec();
    let mut builder = FolderTreeBuilder::new(root_path.clone(), path.to_vec(), &folder_name, node_meta(folder));
    builder.set_total_children(FolderTree::FOLDER, projection.visible_children(&arena, folder));
    let mut chain = Vec::new();
    arena.chain(folder, &mut chain);
    // Ancestors: the root, then every directory down to the folder's parent.
    if folder != ROOT {
        builder.push_ancestor(arena.name(ROOT), node_meta(ROOT));
        for &a in &chain[..chain.len() - 1] {
            builder.push_ancestor(arena.name(a), node_meta(a));
        }
    }
    // All direct children; directories are hydrated largest first, like the final order.
    let mut directories: Vec<(u64, NodeId, u32)> = Vec::new();
    arena.for_each_child(folder, |c| {
        if projection.hides(c) {
            return;
        }
        let m = node_meta(c);
        let ix = builder.push_child(FolderTree::FOLDER, arena.name(c), m);
        if m.flags.is_dir {
            builder.set_total_children(ix, projection.visible_children(&arena, c));
            directories.push((m.size, c, ix));
        }
    });
    directories.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| arena.name(a.1).cmp(arena.name(b.1))));
    let mut budget = VIEW_DESCENDANT_BUDGET;
    let mut scratch = Vec::new();
    for (_, child, ix) in directories {
        hydrate(&arena, &projection, &mut builder, child, ix, 1, &mut budget, &mut scratch);
    }
    let is_listing = arena.dir(folder).open > 0;
    let revision = arena.revision;
    drop(arena);
    Some(builder.finish(revision, is_listing, ViewSource::Live))
}

#[allow(clippy::too_many_arguments)]
fn hydrate(
    arena: &Arena,
    projection: &Projection,
    builder: &mut FolderTreeBuilder,
    dir: NodeId,
    ix: u32,
    depth: u32,
    budget: &mut usize,
    scratch: &mut Vec<(u64, NodeId)>,
) {
    if depth >= VIEW_DEPTH || *budget == 0 {
        return;
    }
    let limit = HYDRATE_PER_FOLDER.min(*budget);
    scratch.clear();
    arena.for_each_child(dir, |c| {
        if !projection.hides(c) {
            scratch.push((projection.size(arena, c), c));
        }
    });
    let order = |a: &(u64, NodeId), b: &(u64, NodeId)| b.0.cmp(&a.0).then_with(|| arena.name(a.1).cmp(arena.name(b.1)));
    if scratch.len() > limit {
        scratch.select_nth_unstable_by(limit - 1, order);
        scratch.truncate(limit);
    }
    scratch.sort_unstable_by(order);
    *budget -= scratch.len();
    let items: Vec<NodeId> = scratch.iter().map(|&(_, c)| c).collect();
    for c in items {
        let mut m = meta(arena, c);
        projection.adjust(c, &mut m);
        let child_ix = builder.push_child(ix, arena.name(c), m);
        if m.flags.is_dir {
            builder.set_total_children(child_ix, projection.visible_children(arena, c));
            hydrate(arena, projection, builder, c, child_ix, depth + 1, budget, scratch);
        }
    }
}

/// One search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub path: Vec<u8>,
    pub meta: NodeMeta,
}

/// Heap entry ordered so the *worst* candidate is on top: smaller size, then larger path.
struct Candidate {
    size: u64,
    path: Vec<u8>,
    node: NodeId,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.size.cmp(&self.size).then_with(|| self.path.cmp(&other.path))
    }
}

pub(super) fn search(
    live: &LiveInventory,
    folded_query: &str,
    limit: usize,
    removals: &[ScanRemoval],
) -> (Vec<SearchHit>, usize) {
    if folded_query.is_empty() {
        return (Vec::new(), 0);
    }
    let limit = limit.min(SEARCH_LIMIT_MAX);
    let finder = memchr::memmem::Finder::new(folded_query.as_bytes());
    let root_path = live.root_path();
    let projection = {
        let arena = live.arena().read();
        Projection::new(&arena, root_path, removals)
    };
    let mut heap: BinaryHeap<Candidate> = BinaryHeap::with_capacity(limit + 1);
    let mut count = 0usize;
    let mut folded = String::new();
    let mut chain = Vec::new();
    let mut parent_path: Vec<u8> = Vec::new();
    let mut cached_parent = NONE;
    let mut path: Vec<u8> = Vec::new();
    let mut start: NodeId = 1;
    loop {
        let arena = live.arena().read();
        let end = arena.len().min(start.saturating_add(SEARCH_STRIDE));
        for n in start..end {
            if arena.flags(n) & DEAD != 0 {
                continue;
            }
            let name = arena.name(n);
            folded.clear();
            crate::fold::fold_into(name, &mut folded);
            if finder.find(folded.as_bytes()).is_none() {
                continue;
            }
            if !projection.is_empty() && projection.covers(&arena, n) {
                continue;
            }
            count += 1;
            if limit == 0 {
                continue;
            }
            let size = projection.size(&arena, n);
            if heap.len() == limit && heap.peek().is_some_and(|worst| size < worst.size) {
                continue;
            }
            let parent = arena.parent(n);
            if parent != cached_parent {
                arena.path_into(root_path, parent, &mut chain, &mut parent_path);
                cached_parent = parent;
            }
            path.clear();
            path.extend_from_slice(&parent_path);
            if path.last() != Some(&b'/') {
                path.push(b'/');
            }
            path.extend_from_slice(name);
            if heap.len() == limit {
                let worst = heap.peek().expect("non-empty heap");
                if size == worst.size && path >= worst.path {
                    continue;
                }
                heap.pop();
            }
            heap.push(Candidate { size, path: path.clone(), node: n });
        }
        start = end;
        if start >= arena.len() {
            // Build results under this final lock so metadata matches the scanned state.
            let mut hits: Vec<Candidate> = heap.into_vec();
            hits.sort_unstable();
            let hits = hits
                .into_iter()
                .filter(|c| arena.flags(c.node) & DEAD == 0)
                .map(|c| {
                    let mut m = meta(&arena, c.node);
                    projection.adjust(c.node, &mut m);
                    SearchHit { path: c.path, meta: m }
                })
                .collect();
            return (hits, count);
        }
        drop(arena);
    }
}

pub(super) fn file_count(live: &LiveInventory, removals: &[ScanRemoval]) -> Option<u64> {
    let arena = live.arena().read();
    if removals.is_empty() {
        return Some(arena.dir(ROOT).files);
    }
    let projection = Projection::new(&arena, live.root_path(), removals);
    if projection.hides(ROOT) {
        return None;
    }
    let mut m = meta(&arena, ROOT);
    projection.adjust(ROOT, &mut m);
    Some(m.file_count)
}
