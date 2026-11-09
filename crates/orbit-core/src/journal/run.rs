//! The engine half of "Move to Trash" / "Delete Permanently" (Swift `DiskModel.removeCollection`).
//!
//! Order per run: commit the intent to the journal, then for every item re-check, in this order,
//! the mapped root's identity (a replaced root stops the whole run), the item's identity, its
//! parent's identity and canonical spelling, and the removal policy; only then call the executor.
//! The operation is completed after the last attempted mutation, even if some failed.

use super::{Journal, RemovalCursor, current_identity, delete};
use crate::paths;
use crate::types::ScanRemoval;
use orbit_platform::posix::path_from_bytes;
use orbit_platform::{Cancellation, FileIdentity, PlatformError};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// Most distinct changed parents a run reports before asking for a full refresh.
pub const PARENT_LIMIT: usize = 10_000;
/// Most removed items a run returns for the view overlay.
pub const OVERLAY_LIMIT: usize = 10_000;

/// Performs the actual filesystem mutation for one item that passed every check.
pub trait RemovalExecutor {
    /// `parent` is the parent identity recorded with the intent (always resolved by the time this
    /// is called).
    fn remove(&mut self, item: &ScanRemoval, parent: FileIdentity) -> Result<(), PlatformError>;
}

/// Moves items to the Trash through an injected function (production: `Platform::trash`).
pub struct TrashExecutor<F>(pub F);

impl<F: FnMut(&Path) -> Result<Option<PathBuf>, PlatformError>> RemovalExecutor for TrashExecutor<F> {
    fn remove(&mut self, item: &ScanRemoval, _parent: FileIdentity) -> Result<(), PlatformError> {
        (self.0)(path_from_bytes(&item.path)).map(|_| ())
    }
}

/// Permanent deletion with [`delete::remove_no_follow`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFollowDelete;

impl RemovalExecutor for NoFollowDelete {
    fn remove(&mut self, item: &ScanRemoval, parent: FileIdentity) -> Result<(), PlatformError> {
        delete::remove_no_follow(&item.path, item.identity, Some(parent))
    }
}

#[derive(Debug)]
pub enum FailureReason {
    /// The intent could not be committed; nothing was touched.
    NotRecorded(super::JournalError),
    /// The mapped root no longer has its mapped identity; the run stopped here.
    RootChanged,
    /// The item was replaced or vanished since it was collected; neither version was touched.
    ItemChanged,
    /// The containing folder was replaced, moved or became unresolvable.
    ParentChanged,
    /// The removal policy refused the item.
    Policy(String),
    /// The executor failed (possibly after partial recursive deletion).
    Failed(PlatformError),
    /// Every mutation was attempted but the journal could not mark the operation complete.
    Tracking(super::JournalError),
}

#[derive(Debug)]
pub struct RemovalFailure {
    /// `None` for run-wide failures.
    pub path: Option<Vec<u8>>,
    pub reason: FailureReason,
}

#[derive(Debug, Default)]
pub struct RemovalOutcome {
    /// Paths the executor removed, in request order.
    pub removed: Vec<Vec<u8>>,
    pub failures: Vec<RemovalFailure>,
    /// Parents to refresh: every parent an executor was called for, plus parents of items whose
    /// identity or parent changed. At most [`PARENT_LIMIT`].
    pub changed_parents: BTreeSet<Vec<u8>>,
    /// More parents changed than [`PARENT_LIMIT`]: refresh the whole map.
    pub parents_overflow: bool,
    /// The mapped root was replaced; updates must pause until the user remaps.
    pub root_changed: bool,
    /// Removed items to hide until the index catches up (at most [`OVERLAY_LIMIT`]).
    pub overlay: Vec<ScanRemoval>,
    /// More items were removed than the overlay holds.
    pub overlay_overflow: bool,
    /// Journal position after this run's operation; `None` if the intent was never recorded.
    pub cursor: Option<RemovalCursor>,
}

/// One removal request: a mapped root and the collected items with their collected identities.
pub struct RemovalRun<'j> {
    journal: &'j Journal,
    root: Vec<u8>,
    root_identity: FileIdentity,
    items: Vec<ScanRemoval>,
    parent_limit: usize,
}

impl std::fmt::Debug for RemovalRun<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemovalRun").field("root", &String::from_utf8_lossy(&self.root)).finish()
    }
}

impl<'j> RemovalRun<'j> {
    pub fn new(journal: &'j Journal, root: Vec<u8>, root_identity: FileIdentity, items: Vec<ScanRemoval>) -> Self {
        Self { journal, root, root_identity, items, parent_limit: PARENT_LIMIT }
    }

    pub fn with_parent_limit(mut self, limit: usize) -> Self {
        self.parent_limit = limit;
        self
    }

    /// Runs the removal. `policy` returns a rejection reason for paths that must not be removed
    /// (production: the portable removal policy against the mapped root). `cancel` only aborts
    /// waiting for the journal before anything is recorded; once recorded, every item is attempted
    /// and the operation is completed.
    pub fn execute<E: RemovalExecutor>(
        self,
        executor: &mut E,
        mut policy: impl FnMut(&[u8]) -> Option<String>,
        cancel: &Cancellation,
    ) -> RemovalOutcome {
        let mut outcome = RemovalOutcome::default();
        let operation = match self.journal.begin(&self.root, self.root_identity, &self.items, cancel) {
            Ok(operation) => operation,
            Err(error) => {
                outcome.failures.push(RemovalFailure { path: None, reason: FailureReason::NotRecorded(error) });
                return outcome;
            }
        };
        let limit = self.parent_limit;
        let note_parent = |outcome: &mut RemovalOutcome, path: &[u8]| {
            let parent = paths::parent(path).unwrap_or(b"/");
            if outcome.changed_parents.contains(parent) || outcome.changed_parents.len() < limit {
                outcome.changed_parents.insert(parent.to_vec());
            } else {
                outcome.parents_overflow = true;
            }
        };
        let mut removed = HashSet::new();
        for item in &self.items {
            if current_identity(&self.root) != Some(self.root_identity) {
                outcome.failures.push(RemovalFailure { path: None, reason: FailureReason::RootChanged });
                outcome.root_changed = true;
                break;
            }
            let fail = |outcome: &mut RemovalOutcome, reason| {
                outcome.failures.push(RemovalFailure { path: Some(item.path.clone()), reason });
            };
            if current_identity(&item.path) != Some(item.identity) {
                fail(&mut outcome, FailureReason::ItemChanged);
                if paths::is_within(&item.path, &self.root) {
                    note_parent(&mut outcome, &item.path);
                }
                continue;
            }
            let parent = operation.parent_identity(&item.path);
            let Some(parent) = parent.filter(|_| operation.parent_is_unchanged(&item.path)) else {
                fail(&mut outcome, FailureReason::ParentChanged);
                note_parent(&mut outcome, &item.path);
                continue;
            };
            if let Some(reason) = policy(&item.path) {
                fail(&mut outcome, FailureReason::Policy(reason));
                continue;
            }
            note_parent(&mut outcome, &item.path);
            match executor.remove(item, parent) {
                Ok(()) => {
                    removed.insert(item.path.as_slice());
                    outcome.removed.push(item.path.clone());
                }
                Err(error) => fail(&mut outcome, FailureReason::Failed(error)),
            }
        }
        // Completion must not be cancellable: the mutations already happened.
        if let Err(error) = operation.complete(&Cancellation::new()) {
            outcome.failures.push(RemovalFailure { path: None, reason: FailureReason::Tracking(error) });
        }
        for item in self.items.iter().filter(|i| removed.contains(i.path.as_slice())) {
            if outcome.overlay.len() < OVERLAY_LIMIT {
                outcome.overlay.push(item.clone());
            } else {
                outcome.overlay_overflow = true;
            }
        }
        outcome.cursor = Some(operation.cursor().clone());
        outcome
    }
}
