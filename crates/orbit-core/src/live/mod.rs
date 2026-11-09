//! The in-memory inventory of a running mapping (Swift `LiveMappingInventory` +
//! `MemoryInventoryView`).
//!
//! Discovery feeds every batch here (through [`LiveInventory::hook`], installed as the
//! mailbox's `on_batch`), before the batch is queued for the writer. UI reads ([`view`],
//! [`search`], counts) are served from memory and never wait for SQLite.
//!
//! # Design
//! All state lives in one [`arena::Arena`] behind a `parking_lot::RwLock`:
//! - writers (discovery workers) hold the lock for one batch at a time: O(records) pushes into
//!   struct-of-arrays columns plus one walk up the ancestor chain for the aggregates;
//! - readers copy what they need under a read lock and sort/format after releasing it. Search
//!   releases and re-takes the lock every [`SEARCH_STRIDE`] nodes so a long scan cannot hold
//!   discovery back.
//!
//! Directories whose identity no longer matches (replaced between listing the parent and the
//! child) are rejected when their batches arrive, and restarting or failing a directory drops
//! its previous subtree, so a replaced directory never contributes stale descendants.
//!
//! [`view`]: LiveInventory::view
//! [`search`]: LiveInventory::search

pub(crate) mod arena;
mod read;

use crate::mailbox::ListingBatch;
use crate::scope::TraversalScope;
use crate::types::{ScanRemoval, ScanState};
use crate::view::{FolderTree, NodeMeta};
use arena::{Arena, NodeId, ROOT, Toward};
use orbit_platform::FileIdentity;
use parking_lot::RwLock;
use std::sync::Arc;

pub use read::SearchHit;

/// Nodes visited per read-lock hold during search.
pub const SEARCH_STRIDE: u32 = 32 * 1024;
/// Upper bound for `limit` in [`LiveInventory::search`] (Swift parity).
pub const SEARCH_LIMIT_MAX: usize = 10_000;

/// Aggregate totals of the whole inventory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveTotals {
    pub logical: u64,
    /// After hard-link deduplication.
    pub allocated: u64,
    pub files: u64,
    /// Nodes ever added (including tombstoned ones).
    pub nodes: u64,
    /// Directories still expected to be listed.
    pub open_directories: u64,
    pub heap_bytes: u64,
}

/// A directory handed to discovery after a successful claim.
#[derive(Clone, Debug)]
pub(crate) struct ClaimedDir {
    pub node: NodeId,
    pub path: Vec<u8>,
    pub identity: FileIdentity,
    pub components: Vec<crate::mailbox::DirComponent>,
}

pub(crate) enum Preferred {
    Claimed(ClaimedDir),
    Waiting,
    Done,
}

pub struct LiveInventory {
    root_path: Arc<[u8]>,
    root_identity: FileIdentity,
    scope: TraversalScope,
    arena: RwLock<Arena>,
}

impl std::fmt::Debug for LiveInventory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveInventory").field("root", &String::from_utf8_lossy(&self.root_path)).finish_non_exhaustive()
    }
}

impl LiveInventory {
    /// An inventory whose root (`scope.root_path()`) is pending.
    pub fn new(scope: TraversalScope) -> Self {
        let root_path: Arc<[u8]> = Arc::from(scope.root_path());
        let root_identity = scope.root_identity();
        let name = if &*root_path == b"/" { &b"/"[..] } else { crate::paths::file_name(&root_path) };
        Self { arena: RwLock::new(Arena::new(name, root_identity)), root_path, root_identity, scope }
    }

    pub fn root_path(&self) -> &Arc<[u8]> {
        &self.root_path
    }

    pub fn root_identity(&self) -> FileIdentity {
        self.root_identity
    }

    pub fn scope(&self) -> &TraversalScope {
        &self.scope
    }

    /// A closure for [`crate::mailbox::Mailbox::new`]'s `on_batch`.
    pub fn hook(self: &Arc<Self>) -> impl Fn(&ListingBatch) + Send + Sync + 'static {
        let live = Arc::clone(self);
        move |batch| {
            live.append(batch);
        }
    }

    /// Adds a listing batch. Batches for a directory that was replaced, dropped or never
    /// discovered are ignored (returns false). A failure batch ends the directory's listing
    /// and keeps what was seen so far.
    pub fn append(&self, batch: &ListingBatch) -> bool {
        self.arena.write().append(batch, &self.scope)
    }

    /// Starts (or restarts) listing `path` with `identity`; a restart drops its old contents.
    pub fn begin(&self, path: &[u8], identity: FileIdentity) -> bool {
        let mut arena = self.arena.write();
        match arena.lookup(&self.root_path, path) {
            Some(n) if arena.is_dir(n) && arena.identity(n) == identity => {
                arena.begin(n);
                true
            }
            _ => false,
        }
    }

    /// A newer complete listing of the directory at `path` (the writer's local pass re-listed
    /// it). Child directories that are still the same keep their subtrees; everything else is
    /// replaced. Returns false when `path` is not a known directory with `identity`.
    pub fn relist(&self, path: &[u8], identity: FileIdentity, records: &crate::records::RecordBatch) -> bool {
        let mut arena = self.arena.write();
        match arena.lookup(&self.root_path, path) {
            Some(n) if arena.is_dir(n) && arena.identity(n) == identity => {
                arena.relist(n, path, records, &self.scope);
                true
            }
            _ => false,
        }
    }

    /// The directory at `path` could not be listed. `missing` removes it (it vanished or was
    /// replaced); otherwise it stays as an inaccessible, empty folder.
    pub fn unavailable(&self, path: &[u8], identity: FileIdentity, missing: bool) {
        let mut arena = self.arena.write();
        let Some(n) = arena.lookup(&self.root_path, path) else { return };
        if arena.identity(n) != identity || !arena.is_dir(n) {
            return;
        }
        if missing && n != ROOT {
            arena.remove(n);
        } else {
            arena.deny(n);
        }
    }

    pub(crate) fn claim_root(&self) -> bool {
        self.arena.write().claim(ROOT, self.root_identity)
    }

    /// Claims the child directory `name` of `parent` for listing.
    pub(crate) fn claim_child(&self, parent: NodeId, name: &[u8], identity: FileIdentity) -> Option<NodeId> {
        let mut arena = self.arena.write();
        let n = arena.child_dir(parent, name)?;
        arena.claim(n, identity).then_some(n)
    }

    pub(crate) fn unavailable_node(&self, n: NodeId, identity: FileIdentity, missing: bool) {
        let mut arena = self.arena.write();
        if !arena.is_alive(n) || arena.identity(n) != identity {
            return;
        }
        if missing && n != ROOT {
            arena.remove(n);
        } else {
            arena.deny(n);
        }
    }

    /// Finds and claims the next directory on the way to `path`.
    pub(crate) fn claim_toward(&self, path: &[u8]) -> Preferred {
        let candidate = match self.arena.read().toward(&self.root_path, path) {
            Toward::Claimable(n) => n,
            Toward::Waiting => return Preferred::Waiting,
            Toward::Done => return Preferred::Done,
        };
        let mut arena = self.arena.write();
        let identity = arena.identity(candidate);
        if !arena.claim(candidate, identity) {
            // Someone else claimed it in between; look again next time.
            return Preferred::Waiting;
        }
        let mut chain = Vec::new();
        let mut path = Vec::new();
        arena.path_into(&self.root_path, candidate, &mut chain, &mut path);
        let components = arena.components(candidate);
        Preferred::Claimed(ClaimedDir { node: candidate, path, identity, components })
    }

    /// Mutation counter; a view with a higher revision supersedes a lower one.
    pub fn revision(&self) -> u64 {
        self.arena.read().revision
    }

    /// Files discovered so far (after drops of replaced or missing directories).
    pub fn discovered_file_count(&self) -> u64 {
        self.arena.read().dir(ROOT).files
    }

    pub fn totals(&self) -> LiveTotals {
        let arena = self.arena.read();
        LiveTotals {
            logical: arena.size(ROOT),
            allocated: arena.effective_alloc(ROOT),
            files: arena.dir(ROOT).files,
            nodes: u64::from(arena.len()),
            open_directories: u64::from(arena.dir(ROOT).open),
            heap_bytes: arena.heap_bytes() as u64,
        }
    }

    /// Metadata of the item at `path`, if discovered.
    pub fn node(&self, path: &[u8]) -> Option<NodeMeta> {
        let arena = self.arena.read();
        let n = arena.lookup(&self.root_path, path)?;
        Some(read::meta(&arena, n))
    }

    /// Scan state of the item at `path`; `None` when it is not known (yet).
    pub fn state(&self, path: &[u8]) -> Option<ScanState> {
        self.node(path).map(|m| m.state)
    }

    /// The folder at `path` with all direct children and up to
    /// [`crate::view::VIEW_DESCENDANT_BUDGET`] descendants over [`crate::view::VIEW_DEPTH`]
    /// levels, with `removals` hidden. `None` when `path` is not a discovered folder or is
    /// hidden by a removal.
    pub fn view(&self, path: &[u8], removals: &[ScanRemoval]) -> Option<FolderTree> {
        read::view(self, path, removals)
    }

    /// Up to `limit` (≤ [`SEARCH_LIMIT_MAX`]) matches for an already folded query
    /// ([`crate::fold::fold_query`]), largest first then by path bytes, plus the total number of
    /// visible matches.
    pub fn search(&self, folded_query: &str, limit: usize, removals: &[ScanRemoval]) -> (Vec<SearchHit>, usize) {
        read::search(self, folded_query, limit, removals)
    }

    /// Files visible after `removals`; `None` when the root itself is removed.
    pub fn file_count(&self, removals: &[ScanRemoval]) -> Option<u64> {
        read::file_count(self, removals)
    }

    /// The inventory proves `path` is not a folder: it is a file, or its nearest discovered
    /// ancestor is a file or a completely listed folder.
    pub fn confirms_missing_directory(&self, path: &[u8]) -> bool {
        let arena = self.arena.read();
        if let Some(n) = arena.lookup(&self.root_path, path) {
            return !arena.is_dir(n);
        }
        let mut current = path;
        while let Some(parent) = crate::paths::parent(current) {
            if !crate::paths::is_within(parent, &self.root_path) {
                return false;
            }
            if let Some(n) = arena.lookup(&self.root_path, parent) {
                return !arena.is_dir(n) || arena.dir(n).state == ScanState::Complete;
            }
            current = parent;
        }
        false
    }

    pub(crate) fn arena(&self) -> &RwLock<Arena> {
        &self.arena
    }
}

#[cfg(test)]
mod tests;
