//! Removal replay for a live mapping stage (Swift `StageRemovalReplay`).
//!
//! A stage stores a removal cursor like a saved map. Before exposing staged data, the stage
//! replays journal operations past its cursor: affected parents are persisted
//! ([`ReplayTarget::add_replay_parents`]) and re-queued, removed items are overlaid on views, and
//! the cursor advances only once the re-queued work has been written and the journal has not
//! moved ([`StageReplay::settle_if_possible`]). A live operation holds the cursor back without
//! hiding the unrelated staged map. A fresh, empty stage anchors its cursor just before the first
//! live operation, so nothing that could still mutate the tree is skipped.
//!
//! Methods run on the stage's single writer; [`ReplayViewHandle`] is safe to read elsewhere.

use super::{OVERLAY_LIMIT, RecoveryClock, probe_page};
use crate::journal::{Fence, Journal, JournalError, JournalResult, MAX_PAGE, Relation, RemovalCursor};
use crate::types::ScanRemoval;
use orbit_platform::Cancellation;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

/// Replay parents are marked changed in pages of this size.
const PARENT_PAGE: usize = 128;
/// Minimum time between journal probes in [`StageReplay::refresh_if_needed`].
pub const PROBE_INTERVAL: Duration = Duration::from_secs(1);

/// The stage that replay updates. Implemented by the durable-index owner (Swift
/// `WALListingStage` + `DurableCrawler`).
pub trait ReplayTarget {
    /// The mapped root (standardized byte path).
    fn root(&self) -> &[u8];
    /// The durable cursor stored in the stage (`orbit_removal_cursor`).
    fn removal_cursor(&mut self) -> crate::Result<Option<RemovalCursor>>;
    /// Durably replaces the stored cursor.
    fn write_removal_cursor(&mut self, cursor: &RemovalCursor) -> crate::Result<()>;
    /// True when the stage holds no listed rows or queued work yet, so it may anchor at the
    /// current journal position instead of replaying history.
    fn is_fresh_for_removal_anchor(&mut self) -> crate::Result<bool>;
    /// Clears the persisted replay-parent set.
    fn reset_replay_parents(&mut self) -> crate::Result<()>;
    /// Persists parents to re-list (insert-or-ignore, one transaction).
    fn add_replay_parents(&mut self, parents: &[Vec<u8>]) -> crate::Result<()>;
    /// Persisted replay parents ordered by bytes, strictly after `after`.
    fn replay_parents(&mut self, after: Option<&[u8]>, limit: usize) -> crate::Result<Vec<Vec<u8>>>;
    /// Re-queues one directory for listing.
    fn mark_changed(&mut self, directory: &[u8]) -> crate::Result<()>;
    /// Queues a full refresh of the stage.
    fn mark_full_refresh(&mut self) -> crate::Result<()>;
    /// Re-queued work (dirty directories, prunes, full refresh) is still pending.
    fn has_replay_dirty_work(&mut self) -> crate::Result<bool>;
    /// Directories are still waiting to be listed.
    fn has_pending_directories(&mut self) -> crate::Result<bool>;
    /// Discovery is still running; a removed file's parent may exist only in its queue.
    fn has_active_async_discovery(&self) -> bool;
    /// Fails unless the root still is the mapped directory.
    fn validate_root(&mut self) -> crate::Result<()>;
    /// Called before a full reconciliation is queued (overflow, covered root, replaced journal).
    fn before_full_reconciliation(&mut self) {}
}

/// What views must apply on top of staged data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayView {
    /// Removed items to hide (identity-checked by the reader).
    pub overlay: Arc<[ScanRemoval]>,
    /// The staged map must not be shown (replay not prepared, overflow, or full reconciliation).
    pub blocked: bool,
    /// The durable cursor the stage's data covers.
    pub cursor: Option<RemovalCursor>,
    /// Changes whenever any field changes; readers drop results computed under an older one.
    pub generation: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ReplayViewHandle(Arc<Mutex<ReplayView>>);

impl ReplayViewHandle {
    pub fn get(&self) -> ReplayView {
        self.0.lock().clone()
    }

    fn update(&self, change: impl FnOnce(&mut ReplayView)) {
        let mut view = self.0.lock();
        change(&mut view);
        view.generation = view.generation.wrapping_add(1);
    }
}

pub struct StageReplay {
    journal: Arc<Journal>,
    cancel: Cancellation,
    clock: Arc<dyn RecoveryClock>,
    view: ReplayViewHandle,
    full_reconciliation: bool,
    target: Option<RemovalCursor>,
    needs_settle: bool,
    live_barrier: Option<i64>,
    next_probe: Duration,
    deferred_async_replay: bool,
    #[cfg(test)]
    pub(crate) after_fresh_capture: Option<Box<dyn FnMut() + Send>>,
    #[cfg(test)]
    pub(crate) after_replay_head: Option<Box<dyn FnMut(Option<i64>) + Send>>,
}

impl std::fmt::Debug for StageReplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageReplay").field("target", &self.target).field("live_barrier", &self.live_barrier).finish()
    }
}

impl StageReplay {
    /// [`StageReplay::new`] + [`StageReplay::anchor`].
    pub fn open<T: ReplayTarget>(
        journal: Arc<Journal>,
        target: &mut T,
        cancel: Cancellation,
        clock: Arc<dyn RecoveryClock>,
        fresh: bool,
    ) -> JournalResult<Self> {
        let mut replay = Self::new(journal, cancel, clock);
        replay.anchor(target, fresh)?;
        Ok(replay)
    }

    pub fn new(journal: Arc<Journal>, cancel: Cancellation, clock: Arc<dyn RecoveryClock>) -> Self {
        Self {
            journal,
            cancel,
            clock,
            view: ReplayViewHandle(Arc::new(Mutex::new(ReplayView { blocked: true, ..ReplayView::default() }))),
            full_reconciliation: false,
            target: None,
            needs_settle: true,
            live_barrier: None,
            next_probe: Duration::ZERO,
            deferred_async_replay: false,
            #[cfg(test)]
            after_fresh_capture: None,
            #[cfg(test)]
            after_replay_head: None,
        }
    }

    /// Loads the stage's cursor. A `fresh` stage without one anchors at the journal's high-water,
    /// or just before the first *live* pending operation, and stores that cursor. Abandoned
    /// operations before the anchor are safe to skip: their mutations happened before the stage
    /// lists anything.
    pub fn anchor<T: ReplayTarget>(&mut self, target: &mut T, fresh: bool) -> JournalResult<()> {
        let mut covered = target.removal_cursor()?;
        if fresh && covered.is_none() && target.is_fresh_for_removal_anchor()? {
            let end = self.journal.with_fence(&self.cancel, |f| {
                Ok(RemovalCursor { identity: f.identity().to_string(), sequence: f.high_water() })
            })?;
            let mut after = 0;
            let mut anchor = end.sequence;
            while after < end.sequence {
                self.check()?;
                let (count, last, first_live) = self.journal.with_fence(&self.cancel, |fence| {
                    same_journal(fence, &end)?;
                    let page: Vec<_> = fence
                        .pending_operations(after, MAX_PAGE)?
                        .into_iter()
                        .filter(|e| e.sequence <= end.sequence)
                        .collect();
                    for entry in &page {
                        if fence.is_live(entry.sequence)? {
                            return Ok((page.len(), None, Some(entry.sequence)));
                        }
                    }
                    Ok((page.len(), page.last().map(|e| e.sequence), None))
                })?;
                if let Some(live) = first_live {
                    anchor = live - 1;
                    break;
                }
                match last {
                    Some(last) if count == MAX_PAGE => after = last,
                    _ => break,
                }
            }
            #[cfg(test)]
            if let Some(hook) = self.after_fresh_capture.as_mut() {
                hook();
            }
            let cursor = RemovalCursor { identity: end.identity, sequence: anchor };
            target.write_removal_cursor(&cursor)?;
            covered = Some(cursor);
        }
        self.view.update(|v| v.cursor = covered);
        Ok(())
    }

    pub fn view(&self) -> ReplayViewHandle {
        self.view.clone()
    }

    fn check(&self) -> JournalResult<()> {
        if self.cancel.is_cancelled() { Err(JournalError::Cancelled) } else { Ok(()) }
    }

    /// Replays every operation between the stored cursor and the journal's current high-water.
    pub fn prepare<T: ReplayTarget>(&mut self, target: &mut T) -> JournalResult<()> {
        self.view.update(|v| v.blocked = true);
        target.reset_replay_parents()?;
        let root = target.root().to_vec();
        let covered = self.view.get().cursor;
        let (end, mismatch, initial, expected_root) = self.journal.with_fence(&self.cancel, |fence| {
            let end = RemovalCursor { identity: fence.identity().to_string(), sequence: fence.high_water() };
            let mismatch = covered.as_ref().is_some_and(|c| c.identity != end.identity || c.sequence > end.sequence);
            let start = if mismatch { 0 } else { covered.as_ref().map_or(0, |c| c.sequence) };
            let binding = if start < end.sequence { fence.root_binding(&root) } else { None };
            Ok((end, mismatch, start, binding))
        })?;
        let mut sequence = initial;
        let mut kept: Vec<ScanRemoval> = Vec::new();
        let mut overflow = false;
        let mut full = mismatch;
        let mut pending_live: Option<i64> = None;
        let journal = Arc::clone(&self.journal);
        let clock = Arc::clone(&self.clock);
        while sequence < end.sequence {
            self.check()?;
            let (next, remaining) = journal.with_fence(&self.cancel, |fence| {
                same_journal(fence, &end)?;
                if fence.root_binding(&root) != expected_root {
                    return Err(JournalError::JournalChanged);
                }
                probe_page(&journal, fence, &root, sequence, end.sequence, expected_root.as_ref(), clock.as_ref())
            })?;
            #[cfg(test)]
            if let Some(hook) = self.after_replay_head.as_mut() {
                hook(remaining.first().map(|e| e.sequence));
            }
            sequence = next;
            if remaining.is_empty() {
                if sequence < end.sequence {
                    clock.pause();
                }
                continue;
            }
            for entry in remaining {
                let mut after: Option<Vec<u8>> = None;
                loop {
                    self.check()?;
                    let (page, live) = journal.with_fence(&self.cancel, |fence| {
                        same_journal(fence, &end)?;
                        if fence.root_binding(&root) != expected_root {
                            return Err(JournalError::JournalChanged);
                        }
                        let page = fence.intersections(entry.sequence, &root, after.as_deref(), MAX_PAGE)?;
                        let current = fence
                            .operations(entry.sequence - 1, 1)?
                            .into_iter()
                            .next()
                            .filter(|c| c.sequence == entry.sequence)
                            .ok_or_else(|| JournalError::Invalid("Missing removal operation".into()))?;
                        let live = !page.is_empty() && !current.completed && fence.is_live(entry.sequence)?;
                        Ok((page, live))
                    })?;
                    if live && pending_live.is_none() {
                        pending_live = Some(entry.sequence);
                    }
                    let mut parents: Vec<Vec<u8>> = Vec::new();
                    let mut process = |relation: Relation, removal: ScanRemoval| match relation {
                        Relation::Inside => {
                            match memchr::memrchr(b'/', &removal.path) {
                                Some(0) => parents.push(b"/".to_vec()),
                                Some(i) => parents.push(removal.path[..i].to_vec()),
                                None => full = true,
                            }
                            if !overflow {
                                if kept.len() < OVERLAY_LIMIT {
                                    kept.push(removal);
                                } else {
                                    kept = Vec::new();
                                    overflow = true;
                                }
                            }
                        }
                        Relation::CoversRoot | Relation::Unresolved => full = true,
                    };
                    for item in &page {
                        process(item.relation, item.removal.clone());
                        if let Some(original) = item.original_relation {
                            process(
                                original,
                                ScanRemoval { path: item.original_path.clone(), identity: item.removal.identity },
                            );
                        }
                    }
                    if !parents.is_empty() {
                        target.add_replay_parents(&parents)?;
                    }
                    match page.last() {
                        Some(last) if page.len() == MAX_PAGE => after = Some(last.original_path.clone()),
                        _ => break,
                    }
                }
                sequence = entry.sequence;
            }
        }
        if initial < end.sequence {
            journal.with_fence(&self.cancel, |fence| {
                same_journal(fence, &end)?;
                if fence.root_binding(&root) != expected_root {
                    return Err(JournalError::JournalChanged);
                }
                Ok(())
            })?;
        }
        target.validate_root()?;
        if full || overflow {
            target.before_full_reconciliation();
        }
        // A removed file's parent may still exist only in the discovery queue.
        self.deferred_async_replay = target.has_active_async_discovery() && initial < end.sequence;
        if pending_live.is_some() {
            // The owner may still change the filesystem; dirty parents once it finishes.
        } else if full {
            target.mark_full_refresh()?;
        } else {
            let mut after: Option<Vec<u8>> = None;
            loop {
                self.check()?;
                let page = target.replay_parents(after.as_deref(), PARENT_PAGE)?;
                for path in &page {
                    target.mark_changed(path)?;
                }
                match page.last() {
                    Some(last) if page.len() == PARENT_PAGE => after = Some(last.clone()),
                    _ => break,
                }
            }
        }
        self.full_reconciliation = full;
        self.live_barrier = pending_live;
        self.next_probe = self.clock.now() + PROBE_INTERVAL;
        self.target = Some(end);
        self.needs_settle = true;
        let blocked = full || overflow;
        self.view.update(|v| {
            v.overlay = kept.into();
            v.blocked = blocked;
        });
        Ok(())
    }

    /// Advances the durable cursor once replayed work is written and the journal is unchanged.
    /// Returns true when the stage is settled at its target.
    pub fn settle_if_possible<T: ReplayTarget>(&mut self, target: &mut T) -> JournalResult<bool> {
        let Some(goal) = self.target.clone() else { return Err(JournalError::JournalChanged) };
        if self.live_barrier.is_some() || target.has_active_async_discovery() {
            return Ok(false);
        }
        if self.deferred_async_replay {
            self.deferred_async_replay = false;
            self.prepare(target)?;
            return Ok(false);
        }
        if !self.needs_settle {
            return Ok(true);
        }
        if target.has_replay_dirty_work()? {
            return Ok(false);
        }
        if self.full_reconciliation && target.has_pending_directories()? {
            return Ok(false);
        }
        let stable = self.journal.with_fence(&self.cancel, |f| Ok(f.is_at(&goal)))?;
        if !stable {
            self.prepare(target)?;
            return Ok(false);
        }
        target.write_removal_cursor(&goal)?;
        self.needs_settle = false;
        self.view.update(|v| {
            v.cursor = Some(goal);
            v.overlay = Arc::from([]);
            v.blocked = false;
        });
        Ok(true)
    }

    /// At most once per [`PROBE_INTERVAL`]: re-prepares when the journal moved or the live
    /// operation holding the cursor back has finished.
    pub fn refresh_if_needed<T: ReplayTarget>(&mut self, target: &mut T) -> JournalResult<()> {
        let now = self.clock.now();
        let Some(goal) = self.target.clone() else { return Ok(()) };
        if now < self.next_probe {
            return Ok(());
        }
        self.next_probe = now + PROBE_INTERVAL;
        let barrier = self.live_barrier;
        let changed = self.journal.with_fence(&self.cancel, |fence| {
            if !fence.is_at(&goal) {
                return Ok(true);
            }
            let Some(barrier) = barrier else { return Ok(false) };
            let current = fence
                .operations(barrier - 1, 1)?
                .into_iter()
                .next()
                .filter(|c| c.sequence == barrier)
                .ok_or_else(|| JournalError::Invalid("Missing live removal".into()))?;
            Ok(current.completed || !fence.is_live(barrier)?)
        })?;
        if changed {
            self.prepare(target)?;
        }
        Ok(())
    }

    /// Runs `action` (the stage's publication rename) under the fence, only while the journal is
    /// exactly at the stage's durable cursor.
    pub fn fence_publication<R>(&self, action: impl FnOnce(&Fence<'_>) -> JournalResult<R>) -> JournalResult<R> {
        let Some(covered) = self.view.get().cursor else { return Err(JournalError::JournalChanged) };
        self.journal.with_fence(&self.cancel, |fence| {
            if !fence.is_at(&covered) {
                return Err(JournalError::JournalChanged);
            }
            fence.validate()?;
            action(fence)
        })
    }
}

fn same_journal(fence: &Fence<'_>, end: &RemovalCursor) -> JournalResult<()> {
    if fence.identity() != end.identity || fence.high_water() < end.sequence {
        return Err(JournalError::JournalChanged);
    }
    Ok(())
}
