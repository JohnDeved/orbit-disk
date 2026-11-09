//! Struct-of-arrays node storage for the live inventory.
//!
//! - Every discovered entry is one `u32` node. Per-node fields live in separate segmented
//!   arrays ([`Seg`]), so scans touch only the columns they need and growth never copies more
//!   than one 16 Ki-element chunk (no long pauses under the write lock).
//! - Names live in one byte arena ([`Names`]), addressed by `u32` offset + `u16` length.
//! - Children of a directory are a chain of *runs*: each appended batch is one contiguous node
//!   range, so a directory with `n` children has `ceil(n / batch)` runs and iteration is
//!   sequential. Removed nodes are tombstoned with [`DEAD`].
//! - Directory sizes are subtree aggregates stored in the same `size`/`alloc` columns files use
//!   for their own sizes (directories report zero for themselves), updated incrementally up the
//!   ancestor chain once per batch.
//! - Directories are indexed by `(parent node, name)` in a hash table of `(hash, node)` pairs;
//!   paths are never stored per node.
//! - Hard-linked files (`nlink > 1`) join a link group; only the group's owner (first path in
//!   [`crate::hardlinks`] order) is charged allocated bytes.

use crate::hardlinks;
use crate::mailbox::{DirComponent, ListingBatch};
use crate::records::{RecordBatch, RecordMeta};
use crate::scope::TraversalScope;
use crate::types::ScanState;
use hashbrown::HashTable;
use orbit_platform::{EntryKind, FileIdentity};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::sync::Arc;

pub type NodeId = u32;
pub const NONE: u32 = u32::MAX;
pub const ROOT: NodeId = 0;

// Node flag bits (the low two bits hold the EntryKind).
const KIND_MASK: u8 = 0b11;
pub const MOUNT: u8 = 1 << 2;
pub const LINKED: u8 = 1 << 3;
pub const DEAD: u8 = 1 << 4;

// Directory flag bits.
pub const DIR_DENIED: u8 = 1;
pub const DIR_OUT_OF_SCOPE: u8 = 1 << 1;
pub const DIR_FAILED: u8 = 1 << 2;

const SHIFT: u32 = 14;
const CHUNK: usize = 1 << SHIFT;
const MASK: u32 = CHUNK as u32 - 1;
const NAME_SHIFT: u32 = 20;
const NAME_CHUNK: usize = 1 << NAME_SHIFT;

type FastState = foldhash::fast::FixedState;

fn fast_state() -> FastState {
    FastState::with_seed(0x6f72_6269_745f_6c76)
}

/// A growable array stored in fixed-size chunks: indices stay valid and growth never moves
/// existing elements.
pub struct Seg<T> {
    chunks: Vec<Vec<T>>,
    len: u32,
}

impl<T: Copy> Seg<T> {
    pub fn new() -> Self {
        Self { chunks: Vec::new(), len: 0 }
    }

    #[inline]
    pub fn len(&self) -> u32 {
        self.len
    }

    #[inline]
    pub fn push(&mut self, value: T) -> u32 {
        let index = self.len;
        match self.chunks.last_mut() {
            Some(chunk) if chunk.len() < CHUNK => chunk.push(value),
            _ => {
                // The first chunk grows from small; later chunks are allocated whole.
                let capacity = if self.chunks.is_empty() { 64 } else { CHUNK };
                let mut chunk = Vec::with_capacity(capacity);
                chunk.push(value);
                self.chunks.push(chunk);
            }
        }
        self.len += 1;
        index
    }

    #[inline]
    pub fn get(&self, index: u32) -> T {
        self.chunks[(index >> SHIFT) as usize][(index & MASK) as usize]
    }

    #[inline]
    pub fn at(&mut self, index: u32) -> &mut T {
        &mut self.chunks[(index >> SHIFT) as usize][(index & MASK) as usize]
    }

    pub fn heap_bytes(&self) -> usize {
        self.chunks.iter().map(|c| c.capacity() * std::mem::size_of::<T>()).sum()
    }
}

/// All names in one arena of 1 MiB chunks; a name never spans chunks.
pub struct Names {
    chunks: Vec<Vec<u8>>,
}

impl Names {
    fn push(&mut self, name: &[u8]) -> u32 {
        let fits = self.chunks.last().is_some_and(|c| c.len() + name.len() <= NAME_CHUNK);
        if !fits {
            let capacity = if self.chunks.is_empty() { 4096 } else { NAME_CHUNK };
            self.chunks.push(Vec::with_capacity(capacity.max(name.len())));
        }
        let chunk_index = self.chunks.len() - 1;
        let chunk = &mut self.chunks[chunk_index];
        let offset = chunk.len();
        chunk.extend_from_slice(name);
        assert!(chunk_index < (1 << (32 - NAME_SHIFT)), "live inventory name arena exceeds 4 GiB");
        ((chunk_index as u32) << NAME_SHIFT) | offset as u32
    }

    #[inline]
    fn get(&self, offset: u32, len: u16) -> &[u8] {
        let chunk = &self.chunks[(offset >> NAME_SHIFT) as usize];
        let start = (offset & (NAME_CHUNK as u32 - 1)) as usize;
        &chunk[start..start + len as usize]
    }

    fn heap_bytes(&self) -> usize {
        self.chunks.iter().map(Vec::capacity).sum()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DirInfo {
    pub first_run: u32,
    pub last_run: u32,
    /// Children that are not tombstoned.
    pub children: u32,
    /// Directories in this subtree (self included) whose listing is still expected.
    pub open: u32,
    /// Files in this subtree.
    pub files: u64,
    pub state: ScanState,
    pub flags: u8,
}

impl DirInfo {
    fn new(state: ScanState, flags: u8) -> Self {
        let mut info = Self { first_run: NONE, last_run: NONE, children: 0, open: 0, files: 0, state, flags };
        info.open = u32::from(info.counts_open());
        info
    }

    /// The directory's own listing is still outstanding.
    #[inline]
    pub fn counts_open(&self) -> bool {
        self.state != ScanState::Complete && self.flags & (DIR_DENIED | DIR_OUT_OF_SCOPE | DIR_FAILED) == 0
    }

    #[inline]
    pub fn accessible(&self) -> bool {
        self.flags & (DIR_DENIED | DIR_OUT_OF_SCOPE) == 0
    }
}

#[derive(Clone, Copy, Debug)]
struct Run {
    start: u32,
    len: u32,
    next: u32,
}

struct LinkGroup {
    owner: NodeId,
    members: SmallVec<[NodeId; 2]>,
}

/// The result of walking towards a preferred path.
pub enum Toward {
    /// This directory is the next one to list on the way to the preferred path.
    Claimable(NodeId),
    /// The way is blocked by a directory that is being listed right now.
    Waiting,
    /// The preferred path is listed, being listed, or unreachable: stop prioritizing.
    Done,
}

pub struct Arena {
    parent: Seg<u32>,
    name_off: Seg<u32>,
    name_len: Seg<u16>,
    flags: Seg<u8>,
    dev: Seg<u16>,
    ino: Seg<u64>,
    /// Files: logical bytes. Directories: subtree logical bytes.
    size: Seg<u64>,
    /// Files: raw allocated bytes. Directories: subtree allocated bytes after hard-link dedup.
    alloc: Seg<u64>,
    /// Directories: index into `dirs`. Others: `NONE`.
    aux: Seg<u32>,
    names: Names,
    devs: Vec<u64>,
    dirs: Seg<DirInfo>,
    runs: Seg<Run>,
    index: HashTable<(u64, NodeId)>,
    links: HashMap<FileIdentity, LinkGroup, FastState>,
    hasher: FastState,
    pub revision: u64,
    pub dead: u64,
    scratch: Vec<u8>,
}

impl Arena {
    pub fn new(root_name: &[u8], root_identity: FileIdentity) -> Self {
        let mut arena = Self {
            parent: Seg::new(),
            name_off: Seg::new(),
            name_len: Seg::new(),
            flags: Seg::new(),
            dev: Seg::new(),
            ino: Seg::new(),
            size: Seg::new(),
            alloc: Seg::new(),
            aux: Seg::new(),
            names: Names { chunks: Vec::new() },
            devs: Vec::new(),
            dirs: Seg::new(),
            runs: Seg::new(),
            index: HashTable::new(),
            links: HashMap::with_hasher(fast_state()),
            hasher: fast_state(),
            revision: 0,
            dead: 0,
            scratch: Vec::new(),
        };
        let root = arena.push_node(NONE, root_name, EntryKind::Directory as u8, root_identity, 0, 0);
        debug_assert_eq!(root, ROOT);
        arena
    }

    // --- Accessors ------------------------------------------------------------------------

    #[inline]
    pub fn len(&self) -> u32 {
        self.parent.len()
    }

    #[inline]
    pub fn parent(&self, n: NodeId) -> NodeId {
        self.parent.get(n)
    }

    #[inline]
    pub fn name(&self, n: NodeId) -> &[u8] {
        self.names.get(self.name_off.get(n), self.name_len.get(n))
    }

    #[inline]
    pub fn flags(&self, n: NodeId) -> u8 {
        self.flags.get(n)
    }

    #[inline]
    pub fn kind(&self, n: NodeId) -> EntryKind {
        match self.flags.get(n) & KIND_MASK {
            0 => EntryKind::File,
            1 => EntryKind::Directory,
            2 => EntryKind::Symlink,
            _ => EntryKind::Other,
        }
    }

    #[inline]
    pub fn is_dir(&self, n: NodeId) -> bool {
        self.aux.get(n) != NONE
    }

    #[inline]
    pub fn is_alive(&self, n: NodeId) -> bool {
        self.flags.get(n) & DEAD == 0
    }

    #[inline]
    pub fn identity(&self, n: NodeId) -> FileIdentity {
        FileIdentity { dev: self.devs[self.dev.get(n) as usize], ino: self.ino.get(n) }
    }

    #[inline]
    pub fn size(&self, n: NodeId) -> u64 {
        self.size.get(n)
    }

    #[inline]
    pub fn raw_alloc(&self, n: NodeId) -> u64 {
        if self.is_dir(n) { 0 } else { self.alloc.get(n) }
    }

    /// Allocated bytes after hard-link dedup (subtree total for directories).
    pub fn effective_alloc(&self, n: NodeId) -> u64 {
        let raw = self.alloc.get(n);
        if self.flags.get(n) & LINKED == 0 {
            return raw;
        }
        match self.links.get(&self.identity(n)) {
            Some(group) if group.owner == n => raw,
            _ => 0,
        }
    }

    #[inline]
    pub fn dir(&self, n: NodeId) -> &DirInfo {
        let aux = self.aux.get(n);
        self.dirs_ref(aux)
    }

    #[inline]
    fn dirs_ref(&self, aux: u32) -> &DirInfo {
        let chunk = &self.dirs.chunks[(aux >> SHIFT) as usize];
        &chunk[(aux & MASK) as usize]
    }

    #[inline]
    fn dir_mut(&mut self, n: NodeId) -> &mut DirInfo {
        let aux = self.aux.get(n);
        self.dirs.at(aux)
    }

    /// Calls `f` for every live child of `dir`, in discovery order.
    #[inline]
    pub fn for_each_child(&self, dir: NodeId, mut f: impl FnMut(NodeId)) {
        let mut run = self.dir(dir).first_run;
        while run != NONE {
            let r = self.runs.get(run);
            for n in r.start..r.start + r.len {
                if self.flags.get(n) & DEAD == 0 {
                    f(n);
                }
            }
            run = r.next;
        }
    }

    pub fn heap_bytes(&self) -> usize {
        self.parent.heap_bytes()
            + self.name_off.heap_bytes()
            + self.name_len.heap_bytes()
            + self.flags.heap_bytes()
            + self.dev.heap_bytes()
            + self.ino.heap_bytes()
            + self.size.heap_bytes()
            + self.alloc.heap_bytes()
            + self.aux.heap_bytes()
            + self.names.heap_bytes()
            + self.dirs.heap_bytes()
            + self.runs.heap_bytes()
            + self.index.capacity() * 16
            + self.links.capacity() * 48
    }

    // --- Lookup ---------------------------------------------------------------------------

    #[inline]
    fn key_hash(&self, parent: NodeId, name: &[u8]) -> u64 {
        use std::hash::Hasher;
        let mut h = self.hasher.build_hasher();
        h.write_u32(parent);
        h.write(name);
        h.finish()
    }

    /// The live directory named `name` inside `parent`.
    pub fn child_dir(&self, parent: NodeId, name: &[u8]) -> Option<NodeId> {
        let hash = self.key_hash(parent, name);
        self.index
            .find(hash, |&(h, n)| h == hash && self.parent.get(n) == parent && self.name(n) == name)
            .map(|&(_, n)| n)
    }

    /// Any live child named `name` (directories via the index, others by scanning).
    pub fn child(&self, parent: NodeId, name: &[u8]) -> Option<NodeId> {
        if let Some(n) = self.child_dir(parent, name) {
            return Some(n);
        }
        let mut found = None;
        self.for_each_child(parent, |n| {
            if self.name(n) == name {
                found = Some(n);
            }
        });
        found
    }

    /// The node at `path` (`root_path` maps to the root).
    pub fn lookup(&self, root_path: &[u8], path: &[u8]) -> Option<NodeId> {
        let components = crate::paths::relative_components(path, root_path)?;
        let mut current = ROOT;
        for name in components {
            if !self.is_dir(current) {
                return None;
            }
            current = self.child(current, name)?;
        }
        Some(current)
    }

    /// Follows the queued components of a batch, requiring each directory to still have the
    /// identity it was queued with. `None` when a directory on the way was replaced or dropped.
    pub fn resolve(&self, components: &[DirComponent], identity: FileIdentity) -> Option<NodeId> {
        let mut current = ROOT;
        for component in components {
            current = self.child_dir(current, &component.name)?;
            if self.identity(current) != component.identity {
                return None;
            }
        }
        (self.identity(current) == identity).then_some(current)
    }

    /// Nodes from the root's child down to `n` (empty for the root).
    pub fn chain(&self, mut n: NodeId, out: &mut Vec<NodeId>) {
        out.clear();
        while n != ROOT && n != NONE {
            out.push(n);
            n = self.parent.get(n);
        }
        out.reverse();
    }

    /// Full path of `n` appended to `out` (which must hold `root_path` already, or be empty to
    /// get it written).
    pub fn path_into(&self, root_path: &[u8], n: NodeId, chain: &mut Vec<NodeId>, out: &mut Vec<u8>) {
        out.clear();
        out.extend_from_slice(root_path);
        self.chain(n, chain);
        for &c in chain.iter() {
            if out.last() != Some(&b'/') {
                out.push(b'/');
            }
            out.extend_from_slice(self.name(c));
        }
    }

    fn precedes(&self, a: NodeId, b: NodeId) -> bool {
        let (mut ca, mut cb) = (Vec::new(), Vec::new());
        self.chain(a, &mut ca);
        self.chain(b, &mut cb);
        hardlinks::compare_components(ca.iter().map(|&n| self.name(n)), cb.iter().map(|&n| self.name(n)))
            == std::cmp::Ordering::Less
    }

    // --- Mutation -------------------------------------------------------------------------

    fn dev_index(&mut self, dev: u64) -> u16 {
        if let Some(i) = self.devs.iter().position(|d| *d == dev) {
            return i as u16;
        }
        assert!(self.devs.len() < u16::MAX as usize, "too many devices in one mapping");
        self.devs.push(dev);
        (self.devs.len() - 1) as u16
    }

    fn push_node(&mut self, parent: NodeId, name: &[u8], flags: u8, id: FileIdentity, size: u64, alloc: u64) -> NodeId {
        // Names are at most a few hundred bytes on every supported filesystem.
        let name = &name[..name.len().min(u16::MAX as usize)];
        let dev = if self.devs.last() == Some(&id.dev) { (self.devs.len() - 1) as u16 } else { self.dev_index(id.dev) };
        let off = self.names.push(name);
        let n = self.parent.push(parent);
        self.name_off.push(off);
        self.name_len.push(name.len() as u16);
        self.flags.push(flags);
        self.dev.push(dev);
        self.ino.push(id.ino);
        self.size.push(size);
        self.alloc.push(alloc);
        let aux = if flags & KIND_MASK == EntryKind::Directory as u8 {
            self.dirs.push(DirInfo::new(ScanState::Pending, 0))
        } else {
            NONE
        };
        self.aux.push(aux);
        n
    }

    fn append_run(&mut self, dir: NodeId, start: u32, len: u32) {
        let run = self.runs.push(Run { start, len, next: NONE });
        let info = self.dir_mut(dir);
        let last = info.last_run;
        info.last_run = run;
        if last == NONE {
            info.first_run = run;
        } else {
            self.runs.at(last).next = run;
        }
    }

    /// Adds deltas (two's complement for negative values) to `dir` and all its ancestors.
    fn propagate(&mut self, dir: NodeId, logical: u64, alloc: u64, files: u64, open: i64) {
        if logical == 0 && alloc == 0 && files == 0 && open == 0 {
            return;
        }
        let mut current = dir;
        while current != NONE {
            let size = self.size.at(current);
            *size = size.wrapping_add(logical);
            let a = self.alloc.at(current);
            *a = a.wrapping_add(alloc);
            let info = self.dir_mut(current);
            info.files = info.files.wrapping_add(files);
            info.open = (i64::from(info.open) + open) as u32;
            current = self.parent.get(current);
        }
    }

    /// Changes a directory's state/flags and keeps the `open` counts consistent.
    fn set_dir_state(&mut self, dir: NodeId, state: ScanState, set: u8, clear: u8) {
        let info = self.dir_mut(dir);
        let before = info.counts_open();
        info.state = state;
        info.flags = (info.flags | set) & !clear;
        let after = info.counts_open();
        if before != after {
            self.propagate(dir, 0, 0, 0, if after { 1 } else { -1 });
        }
    }

    fn insert_index(&mut self, n: NodeId) {
        let parent = self.parent.get(n);
        let hash = self.key_hash(parent, self.name(n));
        // A later record with the same name supersedes an earlier one in lookups.
        let (names, offs, lens, parents) = (&self.names, &self.name_off, &self.name_len, &self.parent);
        let name = names.get(offs.get(n), lens.get(n));
        let existing = self.index.find_mut(hash, |&(h, other)| {
            h == hash && parents.get(other) == parent && names.get(offs.get(other), lens.get(other)) == name
        });
        match existing {
            Some(slot) => slot.1 = n,
            None => {
                self.index.insert_unique(hash, (hash, n), |&(h, _)| h);
            }
        }
    }

    fn remove_index(&mut self, n: NodeId) {
        let hash = self.key_hash(self.parent.get(n), self.name(n));
        if let Ok(entry) = self.index.find_entry(hash, |&(h, other)| h == hash && other == n) {
            entry.remove();
        }
    }

    /// Joins a hard-linked file to its group and returns the allocation it is charged.
    fn link_add(&mut self, n: NodeId, id: FileIdentity, raw: u64) -> u64 {
        let owner = match self.links.get_mut(&id) {
            None => {
                self.links.insert(id, LinkGroup { owner: n, members: SmallVec::from_slice(&[n]) });
                return raw;
            }
            Some(group) => {
                group.members.push(n);
                if group.owner == NONE {
                    group.owner = n;
                    return raw;
                }
                group.owner
            }
        };
        if !self.precedes(n, owner) {
            return 0;
        }
        let previous = self.alloc.get(owner);
        self.propagate(self.parent.get(owner), 0, previous.wrapping_neg(), 0, 0);
        if let Some(group) = self.links.get_mut(&id) {
            group.owner = n;
        }
        raw
    }

    /// Applies one listing batch. Returns false when the batch no longer matches the tree.
    pub fn append(&mut self, batch: &ListingBatch, scope: &TraversalScope) -> bool {
        let Some(dir) = self.resolve(&batch.components, batch.parent_identity) else { return false };
        if !self.is_alive(dir) {
            return false;
        }
        self.append_to(dir, batch, scope)
    }

    fn append_to(&mut self, dir: NodeId, batch: &ListingBatch, scope: &TraversalScope) -> bool {
        self.revision += 1;
        if batch.failure.is_some() {
            // Partial results stay visible ("so far"); the listing is no longer expected.
            let state = self.dir(dir).state;
            self.set_dir_state(
                dir,
                if state == ScanState::Pending { ScanState::Scanning } else { state },
                DIR_FAILED,
                0,
            );
            return true;
        }
        if self.dir(dir).state == ScanState::Pending {
            self.set_dir_state(dir, ScanState::Scanning, 0, 0);
        }
        let records = &batch.records;
        if !records.is_empty() {
            let start = self.len();
            let (mut logical, mut alloc, mut files, mut open) = (0u64, 0u64, 0u64, 0i64);
            let mut scratch = std::mem::take(&mut self.scratch);
            for (i, meta) in records.metas().iter().enumerate() {
                let name = records.name(i);
                let n = self.push_record(dir, name, meta);
                if meta.kind == EntryKind::Directory {
                    let allowed =
                        scope.allows_child(&batch.parent_path, name, meta.id.dev, meta.is_mount_point, &mut scratch);
                    if allowed {
                        open += 1;
                    } else {
                        let info = self.dir_mut(n);
                        info.flags |= DIR_OUT_OF_SCOPE;
                        info.state = ScanState::Complete;
                        info.open = 0;
                    }
                    self.insert_index(n);
                } else {
                    logical += meta.size;
                    files += 1;
                    alloc += if meta.nlink > 1 {
                        *self.flags.at(n) |= LINKED;
                        self.link_add(n, meta.id, meta.allocated)
                    } else {
                        meta.allocated
                    };
                }
            }
            self.scratch = scratch;
            let len = self.len() - start;
            self.dir_mut(dir).children += len;
            self.append_run(dir, start, len);
            self.propagate(dir, logical, alloc, files, open);
        }
        if batch.completion.is_some() {
            self.set_dir_state(dir, ScanState::Complete, 0, DIR_FAILED);
        }
        true
    }

    #[inline]
    fn push_record(&mut self, dir: NodeId, name: &[u8], meta: &RecordMeta) -> NodeId {
        let mut flags = meta.kind as u8;
        if meta.is_mount_point {
            flags |= MOUNT;
        }
        let (size, alloc) = if meta.kind == EntryKind::Directory { (0, 0) } else { (meta.size, meta.allocated) };
        self.push_node(dir, name, flags, meta.id, size, alloc)
    }

    /// Claims a pending directory for listing. Fails when it was claimed already, replaced,
    /// dropped or is outside the traversal scope.
    pub fn claim(&mut self, n: NodeId, identity: FileIdentity) -> bool {
        if !self.is_alive(n) || !self.is_dir(n) || self.identity(n) != identity {
            return false;
        }
        let info = self.dir(n);
        if info.state != ScanState::Pending || !info.accessible() {
            return false;
        }
        self.set_dir_state(n, ScanState::Scanning, 0, 0);
        self.revision += 1;
        true
    }

    /// Starts (or restarts) the listing of `dir`. A restart drops everything listed before.
    pub fn begin(&mut self, dir: NodeId) {
        if self.dir(dir).state != ScanState::Pending || self.dir(dir).children > 0 {
            self.drop_children(dir);
        }
        self.set_dir_state(dir, ScanState::Scanning, 0, DIR_FAILED | DIR_DENIED);
        self.revision += 1;
    }

    /// Replaces the listing of `dir` with the complete `records` of a newer listing, keeping
    /// the subtree of every child directory that is still the same directory (name, identity
    /// and mount flag). Files and changed children are replaced. The folder ends complete.
    pub fn relist(&mut self, dir: NodeId, parent_path: &[u8], records: &RecordBatch, scope: &TraversalScope) {
        let mut existing: HashMap<Vec<u8>, NodeId> = HashMap::new();
        let mut children = Vec::new();
        self.for_each_child(dir, |n| children.push(n));
        for &n in &children {
            if self.is_dir(n) {
                existing.insert(self.name(n).to_vec(), n);
            }
        }
        let mut kept = std::collections::HashSet::new();
        let mut fresh = RecordBatch::with_capacity(records.len(), 0);
        for (i, meta) in records.metas().iter().enumerate() {
            let name = records.name(i);
            let same = meta.kind == EntryKind::Directory
                && existing.get(name).is_some_and(|&n| {
                    self.identity(n) == meta.id && (self.flags(n) & MOUNT != 0) == meta.is_mount_point
                });
            if same {
                kept.insert(existing[name]);
            } else {
                fresh.push(&records.get(i));
            }
        }
        for n in children {
            if !kept.contains(&n) {
                self.remove(n);
            }
        }
        let state = self.dir(dir).state;
        if state == ScanState::Pending {
            self.set_dir_state(dir, ScanState::Scanning, 0, 0);
        }
        let batch = ListingBatch::new(
            Arc::from(parent_path),
            self.identity(dir),
            Arc::from(Vec::new()),
            fresh,
            Some(Default::default()),
            None,
        );
        self.append_to(dir, &batch, scope);
        self.set_dir_state(dir, ScanState::Complete, 0, DIR_FAILED | DIR_DENIED);
        self.revision += 1;
    }

    /// The directory could not be read: keep it as an inaccessible, complete, empty folder.
    pub fn deny(&mut self, dir: NodeId) {
        self.drop_children(dir);
        self.set_dir_state(dir, ScanState::Complete, DIR_DENIED, DIR_FAILED);
        self.revision += 1;
    }

    /// The entry no longer exists (or was replaced): remove it and its subtree.
    pub fn remove(&mut self, n: NodeId) {
        if n == ROOT || !self.is_alive(n) {
            return;
        }
        let parent = self.parent.get(n);
        let (logical, alloc, files, open) = if self.is_dir(n) {
            let info = self.dir(n);
            (self.size.get(n), self.alloc.get(n), info.files, i64::from(info.open))
        } else {
            (self.size.get(n), self.effective_alloc(n), 1, 0)
        };
        self.propagate(parent, logical.wrapping_neg(), alloc.wrapping_neg(), files.wrapping_neg(), -open);
        self.dir_mut(parent).children -= 1;
        let mut linked = Vec::new();
        self.kill(n, &mut linked);
        self.settle_links(&linked);
        self.revision += 1;
    }

    fn drop_children(&mut self, dir: NodeId) {
        let mut children = Vec::new();
        self.for_each_child(dir, |n| children.push(n));
        let mut linked = Vec::new();
        for &child in &children {
            self.kill(child, &mut linked);
        }
        let info = *self.dir(dir);
        let own_open = i64::from(info.counts_open());
        let (logical, alloc) = (self.size.get(dir), self.alloc.get(dir));
        self.propagate(
            dir,
            logical.wrapping_neg(),
            alloc.wrapping_neg(),
            info.files.wrapping_neg(),
            own_open - i64::from(info.open),
        );
        let info = self.dir_mut(dir);
        info.first_run = NONE;
        info.last_run = NONE;
        info.children = 0;
        self.settle_links(&linked);
    }

    /// Tombstones `n` and its subtree and unindexes its directories. Aggregates of ancestors
    /// must be adjusted by the caller; hard-linked files are collected for `settle_links`.
    fn kill(&mut self, n: NodeId, linked: &mut Vec<NodeId>) {
        let mut stack = vec![n];
        while let Some(node) = stack.pop() {
            if !self.is_alive(node) {
                continue;
            }
            if self.is_dir(node) {
                self.for_each_child(node, |c| stack.push(c));
                self.remove_index(node);
            } else if self.flags.get(node) & LINKED != 0 {
                linked.push(node);
            }
            *self.flags.at(node) |= DEAD;
            self.dead += 1;
        }
    }

    /// Removes tombstoned members from their link groups and charges a new owner where the
    /// old one was removed.
    fn settle_links(&mut self, linked: &[NodeId]) {
        for &n in linked {
            let id = self.identity(n);
            let Some(group) = self.links.get_mut(&id) else { continue };
            group.members.retain(|m| *m != n);
            if group.owner != n {
                continue;
            }
            group.owner = NONE;
            let members = group.members.clone();
            let mut best = NONE;
            for m in members {
                if best == NONE || self.precedes(m, best) {
                    best = m;
                }
            }
            if best == NONE {
                self.links.remove(&id);
                continue;
            }
            if let Some(group) = self.links.get_mut(&id) {
                group.owner = best;
            }
            let raw = self.alloc.get(best);
            self.propagate(self.parent.get(best), 0, raw, 0, 0);
        }
    }

    /// Walks from the root towards `path`, returning the next directory to list on the way.
    pub fn toward(&self, root_path: &[u8], path: &[u8]) -> Toward {
        let Some(components) = crate::paths::relative_components(path, root_path) else { return Toward::Done };
        let mut current = ROOT;
        for name in components {
            let info = self.dir(current);
            match info.state {
                ScanState::Pending => {
                    return if info.accessible() { Toward::Claimable(current) } else { Toward::Done };
                }
                ScanState::Scanning => {
                    return if info.flags & DIR_FAILED != 0 { Toward::Done } else { Toward::Waiting };
                }
                ScanState::Complete => {}
            }
            match self.child_dir(current, name) {
                Some(next) => current = next,
                None => return Toward::Done,
            }
        }
        let info = self.dir(current);
        if info.state == ScanState::Pending && info.accessible() { Toward::Claimable(current) } else { Toward::Done }
    }

    /// Components (names and identities) from the root to `n`.
    pub fn components(&self, n: NodeId) -> Vec<DirComponent> {
        let mut chain = Vec::new();
        self.chain(n, &mut chain);
        chain.iter().map(|&c| DirComponent { name: self.name(c).into(), identity: self.identity(c) }).collect()
    }
}
