//! Removal recovery: turning journal history into repairs for saved maps and mapping stages.
//!
//! Ported from `Sources/OrbitCore/RemovalRecovery.swift` (planning), the saved-map repair loop in
//! `DiskModel` (driver, [`reconcile_saved_map`]) and `StageRemovalReplay.swift` ([`StageReplay`]).
//! The store/stage owners apply repairs through [`RecoveryTarget`] and [`ReplayTarget`].
//!
//! Planning rules:
//! - The journal's high-water is captured once under the fence; nothing beyond it is consumed.
//!   The plan's cursor is that high-water.
//! - Operations are probed 128 per fence with a 5 ms budget between probes, and the fence is
//!   released between pages so long histories do not block other processes' removals.
//! - The root's identity *and* canonical spelling are re-checked around every probe; a change
//!   is [`JournalError::JournalChanged`].
//! - `CoversRoot` / `Unresolved` items, or more than 10,000 distinct affected parents, require a
//!   full refresh. More than 10,000 removals overflow the overlay (the map is withheld while
//!   affected parents are reconciled).
//! - An incomplete operation that intersects the root is probed for liveness; a live one is
//!   reported and its cursor must not be retired.

#![cfg(unix)]

mod replay;

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;

pub use replay::{ReplayTarget, ReplayView, ReplayViewHandle, StageReplay};

use crate::journal::{
    Fence, Intersection, Journal, JournalError, JournalResult, OperationEntry, Relation, RemovalCursor,
    current_identity, is_standard_path,
};
use crate::paths;
use crate::types::ScanRemoval;
use orbit_platform::{Cancellation, FileIdentity};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

/// Operations fetched per fence while looking for the next relevant one.
pub const OPERATION_PAGE: usize = 128;
/// Items fetched per fence while reading one relevant operation.
pub const ITEM_PAGE: usize = 256;
/// Time spent probing operations before the fence is released. Checked between probes.
pub const PROBE_BUDGET: Duration = Duration::from_millis(5);
/// Removals kept for the overlay; more set `overlay_overflow`.
pub const OVERLAY_LIMIT: usize = 10_000;
/// Distinct affected parents tracked; more require a full refresh.
pub const PARENT_LIMIT: usize = 10_000;
/// Publication attempts before [`reconcile_saved_map`] gives up on a busy journal.
pub const PUBLISH_ATTEMPTS: usize = 8;

/// Time source for recovery budgets and the replay probe interval. Inject a fake in tests.
pub trait RecoveryClock: Send + Sync {
    /// Monotonic time since an arbitrary origin.
    fn now(&self) -> Duration;
    /// Called between journal pages that found nothing relevant, so other processes can take
    /// the fence. Production sleeps briefly.
    fn pause(&self);
}

#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self { origin: Instant::now() }
    }
}

impl RecoveryClock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn pause(&self) {
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// What a root must do to catch up with the journal (Swift `RemovalRecovery`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryPlan {
    /// The captured high-water. Publish repairs with this cursor only.
    pub cursor: RemovalCursor,
    /// Items removed inside the root (at most [`OVERLAY_LIMIT`]), as seen from the root.
    pub removals: Vec<ScanRemoval>,
    /// Parents of every relevant removal (at most [`PARENT_LIMIT`]).
    pub affected_parents: BTreeSet<Vec<u8>>,
    pub overlay_overflow: bool,
    /// A relevant operation is still running.
    pub has_live_operation: bool,
    pub requires_full_refresh: bool,
}

impl RecoveryPlan {
    fn empty(cursor: RemovalCursor) -> Self {
        Self {
            cursor,
            removals: Vec::new(),
            affected_parents: BTreeSet::new(),
            overlay_overflow: false,
            has_live_operation: false,
            requires_full_refresh: false,
        }
    }

    /// Nothing to apply.
    pub fn is_empty(&self) -> bool {
        self.removals.is_empty()
            && self.affected_parents.is_empty()
            && !self.overlay_overflow
            && !self.has_live_operation
            && !self.requires_full_refresh
    }

    fn accept(&mut self, item: ScanRemoval) {
        let parent = match memchr::memrchr(b'/', &item.path) {
            Some(0) => b"/".to_vec(),
            Some(i) => item.path[..i].to_vec(),
            None => {
                self.requires_full_refresh = true;
                return;
            }
        };
        if self.removals.len() < OVERLAY_LIMIT {
            self.removals.push(item);
        } else {
            self.overlay_overflow = true;
        }
        if self.affected_parents.len() < PARENT_LIMIT || self.affected_parents.contains(&parent) {
            self.affected_parents.insert(parent);
        } else {
            self.requires_full_refresh = true;
        }
    }

    fn absorb(&mut self, page: &[Intersection]) -> bool {
        let mut relevant = false;
        for item in page {
            match item.relation {
                Relation::Inside => self.accept(item.removal.clone()),
                Relation::CoversRoot | Relation::Unresolved => self.requires_full_refresh = true,
            }
            relevant = true;
            if let Some(original) = item.original_relation {
                match original {
                    Relation::Inside => {
                        self.accept(ScanRemoval { path: item.original_path.clone(), identity: item.removal.identity })
                    }
                    Relation::CoversRoot | Relation::Unresolved => self.requires_full_refresh = true,
                }
            }
        }
        relevant
    }
}

/// Scans pages of operations under one fence for the first one intersecting `root`. Returns the
/// last sequence known to be irrelevant and the remaining page starting at the relevant one.
pub(crate) fn probe_page(
    journal: &Journal,
    fence: &Fence<'_>,
    root: &[u8],
    after: i64,
    end: i64,
    expected_root: Option<&crate::journal::RootBinding>,
    clock: &dyn RecoveryClock,
) -> JournalResult<(i64, Vec<OperationEntry>)> {
    let operations: Vec<_> =
        fence.operations(after, OPERATION_PAGE)?.into_iter().filter(|o| o.sequence <= end).collect();
    if operations.is_empty() {
        return Err(JournalError::Invalid("Missing removal journal entries".into()));
    }
    if fence.root_binding(root).as_ref() != expected_root {
        return Err(JournalError::JournalChanged);
    }
    let mut scanned = after;
    let deadline = clock.now() + PROBE_BUDGET;
    for (index, operation) in operations.iter().enumerate() {
        let relevant = !fence.intersections(operation.sequence, root, None, 1)?.is_empty();
        #[cfg(test)]
        crate::journal::Hooks::run(&journal.hooks.after_recovery_probe);
        #[cfg(not(test))]
        let _ = journal;
        if fence.root_binding(root).as_ref() != expected_root {
            return Err(JournalError::JournalChanged);
        }
        if relevant {
            return Ok((scanned, operations[index..].to_vec()));
        }
        scanned = operation.sequence;
        if expected_root.is_none() || clock.now() >= deadline {
            break;
        }
    }
    Ok((scanned, Vec::new()))
}

impl Journal {
    /// Plans recovery of `root` from `cursor` (`None` = from the beginning).
    ///
    /// Fails with [`JournalError::JournalChanged`] when the cursor belongs to another journal or
    /// lies beyond its history, or when the root's binding changes while reading; callers then
    /// use [`Journal::recover_or_reset`].
    pub fn recover(
        &self,
        root: &[u8],
        cursor: Option<&RemovalCursor>,
        cancel: &Cancellation,
        clock: &dyn RecoveryClock,
    ) -> JournalResult<RecoveryPlan> {
        if !is_standard_path(root) {
            return Err(JournalError::Invalid("recovery root".into()));
        }
        let start = cursor.map_or(0, |c| c.sequence);
        let (captured, expected_root) = self.with_fence(cancel, |fence| {
            if let Some(c) = cursor
                && (c.identity != fence.identity() || c.sequence < fence.floor() || c.sequence > fence.high_water())
            {
                return Err(JournalError::JournalChanged);
            }
            let end = RemovalCursor { identity: fence.identity().to_string(), sequence: fence.high_water() };
            let binding = if start == end.sequence { None } else { fence.root_binding(root) };
            Ok((end, binding))
        })?;
        let mut plan = RecoveryPlan::empty(captured.clone());
        if start == captured.sequence {
            return Ok(plan);
        }
        let same_journal = |fence: &Fence<'_>| {
            if fence.identity() != captured.identity || fence.high_water() < captured.sequence {
                Err(JournalError::JournalChanged)
            } else {
                Ok(())
            }
        };
        let mut sequence = start;
        while sequence < captured.sequence {
            let (next, remaining) = self.with_fence(cancel, |fence| {
                same_journal(fence)?;
                probe_page(self, fence, root, sequence, captured.sequence, expected_root.as_ref(), clock)
            })?;
            sequence = next;
            let Some(operation) = remaining.into_iter().next() else {
                if sequence < captured.sequence {
                    clock.pause();
                }
                continue;
            };
            let mut after: Option<Vec<u8>> = None;
            let mut relevant = false;
            loop {
                let page = self.with_fence(cancel, |fence| {
                    same_journal(fence)?;
                    if fence.root_binding(root) != expected_root {
                        return Err(JournalError::JournalChanged);
                    }
                    fence.intersections(operation.sequence, root, after.as_deref(), ITEM_PAGE)
                })?;
                relevant |= plan.absorb(&page);
                match page.last() {
                    Some(last) if page.len() == ITEM_PAGE => after = Some(last.original_path.clone()),
                    _ => break,
                }
            }
            if relevant && !operation.completed {
                let live = self.with_fence(cancel, |fence| {
                    same_journal(fence)?;
                    fence.is_live(operation.sequence)
                })?;
                plan.has_live_operation |= live;
            }
            sequence = operation.sequence;
        }
        self.with_fence(cancel, |fence| {
            same_journal(fence)?;
            if fence.root_binding(root) != expected_root {
                return Err(JournalError::JournalChanged);
            }
            Ok(())
        })?;
        Ok(plan)
    }

    /// [`Journal::recover`], restarting from the beginning (and requiring a full refresh) when
    /// the saved cursor no longer matches this journal.
    pub fn recover_or_reset(
        &self,
        root: &[u8],
        cursor: Option<&RemovalCursor>,
        cancel: &Cancellation,
        clock: &dyn RecoveryClock,
    ) -> JournalResult<RecoveryPlan> {
        match self.recover(root, cursor, cancel, clock) {
            Err(JournalError::JournalChanged) => {
                let mut plan = self.recover(root, None, cancel, clock)?;
                plan.requires_full_refresh = true;
                Ok(plan)
            }
            other => other,
        }
    }
}

/// A saved map (or a private working copy of it) that recovery repairs. Implemented by the
/// durable-index owner. All mutations stay private until [`RecoveryTarget::publish`].
pub trait RecoveryTarget {
    /// The mapped root (standardized byte path).
    fn root(&self) -> &[u8];

    /// The removal cursor stored with the published map (`orbit_removal_cursor`), if any.
    fn removal_cursor(&mut self) -> crate::Result<Option<RemovalCursor>>;

    /// The identity the map records for each path (`None` = not indexed), in input order.
    /// Recovery uses it to find, for every affected parent, the nearest indexed ancestor that
    /// is still the same directory on disk.
    fn indexed_identities(&mut self, paths: &[Vec<u8>]) -> crate::Result<Vec<Option<FileIdentity>>>;

    /// Drops the rows (and subtrees) for these removals, but only rows whose stored identity
    /// equals the removal's identity; anything else at the path (a replacement) is left alone.
    /// Recovery only passes items whose recorded identity is no longer at the path on disk.
    fn apply_removals(&mut self, removals: &[ScanRemoval], cancel: &Cancellation) -> crate::Result<()>;

    /// Re-lists these directories (each verified as the indexed directory) so replacements and
    /// failed removals are reflected.
    fn reconcile_directories(&mut self, directories: &[Vec<u8>], cancel: &Cancellation) -> crate::Result<()>;

    /// Discards private repairs and schedules a full refresh. When that refresh starts after this
    /// call, it may publish with `cursor`.
    fn require_full_refresh(&mut self, cursor: &RemovalCursor) -> crate::Result<()>;

    /// Atomically publishes the repaired map with `cursor` stored as its removal cursor. Called
    /// with the fence held, `fence.is_at(cursor)` true and the fence just validated; do the
    /// final rename here and nothing slow.
    fn publish(&mut self, cursor: &RemovalCursor, fence: &Fence<'_>) -> crate::Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SavedRecovery {
    /// The stored cursor already covers the journal; nothing changed.
    UpToDate(RemovalCursor),
    /// Repairs were published together with this cursor.
    Published(RemovalCursor),
    /// A live operation touches the root; nothing was published or retired. Overlay
    /// `plan.removals` and retry after the operation ends.
    Live(RecoveryPlan),
    /// The target was told to refresh fully (replaced journal, covered root, unresolved items or
    /// too many parents).
    FullRefresh(RecoveryPlan),
}

/// Brings a saved map up to date with the journal: plan, apply identity-checked removals,
/// reconcile affected directories, then publish under the fence only if no newer operation was
/// appended meanwhile (otherwise re-plan from the stored cursor).
pub fn reconcile_saved_map<T: RecoveryTarget>(
    journal: &Journal,
    target: &mut T,
    cancel: &Cancellation,
    clock: &dyn RecoveryClock,
) -> JournalResult<SavedRecovery> {
    let root = target.root().to_vec();
    let stored = target.removal_cursor()?;
    for _ in 0..PUBLISH_ATTEMPTS {
        let plan = journal.recover_or_reset(&root, stored.as_ref(), cancel, clock)?;
        if stored.as_ref() == Some(&plan.cursor) && plan.is_empty() {
            return Ok(SavedRecovery::UpToDate(plan.cursor));
        }
        if plan.has_live_operation {
            return Ok(SavedRecovery::Live(plan));
        }
        if plan.requires_full_refresh {
            target.require_full_refresh(&plan.cursor)?;
            return Ok(SavedRecovery::FullRefresh(plan));
        }
        let gone: Vec<ScanRemoval> =
            plan.removals.iter().filter(|r| current_identity(&r.path) != Some(r.identity)).cloned().collect();
        for chunk in gone.chunks(OPERATION_PAGE) {
            check(cancel)?;
            target.apply_removals(chunk, cancel)?;
        }
        let Some(directories) = reconcile_set(target, &root, &plan.affected_parents, cancel)? else {
            target.require_full_refresh(&plan.cursor)?;
            let mut plan = plan;
            plan.requires_full_refresh = true;
            return Ok(SavedRecovery::FullRefresh(plan));
        };
        let directories: Vec<_> = directories.into_iter().collect();
        for chunk in directories.chunks(OPERATION_PAGE) {
            check(cancel)?;
            target.reconcile_directories(chunk, cancel)?;
        }
        let published = journal.with_fence(cancel, |fence| {
            if !fence.is_at(&plan.cursor) {
                return Err(JournalError::ChangedDuringReconciliation);
            }
            fence.validate()?;
            target.publish(&plan.cursor, fence)?;
            Ok(())
        });
        match published {
            Ok(()) => return Ok(SavedRecovery::Published(plan.cursor)),
            Err(JournalError::ChangedDuringReconciliation) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(JournalError::ChangedDuringReconciliation)
}

fn check(cancel: &Cancellation) -> JournalResult<()> {
    if cancel.is_cancelled() { Err(JournalError::Cancelled) } else { Ok(()) }
}

/// For each affected parent, the nearest ancestor (the parent itself first) that the map indexes
/// with the identity the same path has on disk now. `None` when even the root does not qualify.
fn reconcile_set<T: RecoveryTarget>(
    target: &mut T,
    root: &[u8],
    parents: &BTreeSet<Vec<u8>>,
    cancel: &Cancellation,
) -> JournalResult<Option<BTreeSet<Vec<u8>>>> {
    let mut resolved = BTreeSet::new();
    let mut pending: BTreeSet<Vec<u8>> = parents.iter().filter(|p| paths::is_within(p, root)).cloned().collect();
    while !pending.is_empty() {
        check(cancel)?;
        let level: Vec<Vec<u8>> = std::mem::take(&mut pending).into_iter().collect();
        let mut indexed = Vec::with_capacity(level.len());
        for chunk in level.chunks(crate::journal::MAX_PAGE) {
            indexed.extend(target.indexed_identities(chunk)?);
        }
        for (path, identity) in level.into_iter().zip(indexed) {
            let on_disk = crate::journal::canonical::directory_identity_no_follow(&path);
            if identity.is_some() && identity == on_disk {
                resolved.insert(path);
            } else if path.as_slice() == root {
                return Ok(None);
            } else if let Some(parent) = paths::parent(&path) {
                pending.insert(parent.to_vec());
            }
        }
    }
    Ok(Some(resolved))
}
