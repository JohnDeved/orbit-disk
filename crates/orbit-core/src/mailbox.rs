//! The hand-off between discovery workers (producers) and the single index writer (consumer).
//!
//! Ported from `PendingInventoryMailbox` (`Sources/OrbitCore/AsyncListingProducer.swift`).
//!
//! - Discovery calls [`Mailbox::observe`] (progress accounting and the live-inventory hook), then
//!   [`Mailbox::enqueue`], which assigns a sequence number. Batches for one directory are enqueued
//!   in order, and parents before children (the ticket-ordered spool guarantees this).
//! - The writer pulls with [`Mailbox::next`] / [`Mailbox::next_fresh_leaves`], commits them to
//!   SQLite *together with the highest committed sequence* (the watermark), then calls
//!   [`Mailbox::acknowledge`].
//! - Readers call [`Mailbox::capture`] **before** opening their SQLite read transaction and
//!   ignore captured batches whose sequence is at or below the watermark they read from the
//!   database. That keeps a view consistent across the memory/database boundary without
//!   blocking the writer.

use crate::records::{BatchTotals, RecordBatch};
use orbit_platform::{Cancellation, FileIdentity};
use parking_lot::{Condvar, Mutex};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// Batches per frozen page. Snapshots clone one `Arc` per page, not per batch.
const PAGE: usize = 128;
/// How long [`Mailbox::next`] waits for work before returning empty.
const NEXT_WAIT: Duration = Duration::from_millis(25);
/// Grouping limits for small finished folders (`nextFreshLeaves` in Swift).
pub const FRESH_LEAF_MAX_FOLDERS: usize = 32;
pub const FRESH_LEAF_MAX_RECORDS: usize = 1024;
pub const FRESH_LEAF_MAX_BYTES: usize = 256 * 1024;
pub const FRESH_LEAF_MAX_FOLDER_RECORDS: usize = 128;

/// One path component of a queued directory, with the identity it must still have when the
/// writer (or a retry) re-opens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirComponent {
    pub name: Box<[u8]>,
    pub identity: FileIdentity,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListingCompletion {
    /// Total issues seen while listing (may exceed `issues.len()` when messages are capped).
    pub issue_count: u32,
    pub issues: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListingFailure {
    pub message: String,
    pub code: Option<i32>,
}

/// Up to [`orbit_platform::LISTING_BATCH`] (or more, when coalesced) records of one directory.
#[derive(Debug)]
pub struct ListingBatch {
    /// Assigned by [`Mailbox::enqueue`]; `0` before that.
    pub sequence: u64,
    pub parent_path: Arc<[u8]>,
    pub parent_identity: FileIdentity,
    /// Components from the mapped root to `parent_path` (empty for the root itself).
    pub components: Arc<[DirComponent]>,
    pub records: RecordBatch,
    /// Present on the last batch of a directory listing that finished.
    pub completion: Option<ListingCompletion>,
    /// Present when the listing failed; earlier batches of the same directory are discarded.
    pub failure: Option<ListingFailure>,
    /// The `dirty.revision` the listing was started against (0 = fresh directory).
    pub expected_revision: i64,
    /// The full-refresh generation the listing was started against.
    pub expected_full_generation: i64,
    pub totals: BatchTotals,
    pub byte_count: usize,
}

impl ListingBatch {
    pub fn new(
        parent_path: Arc<[u8]>,
        parent_identity: FileIdentity,
        components: Arc<[DirComponent]>,
        records: RecordBatch,
        completion: Option<ListingCompletion>,
        failure: Option<ListingFailure>,
    ) -> Self {
        let totals = records.totals();
        let byte_count = 256
            + parent_path.len()
            + components.iter().map(|c| 64 + c.name.len()).sum::<usize>()
            + records.byte_count()
            + completion.as_ref().map_or(0, |c| c.issues.iter().map(|i| 32 + i.len()).sum());
        Self {
            sequence: 0,
            parent_path,
            parent_identity,
            components,
            records,
            completion,
            failure,
            expected_revision: 0,
            expected_full_generation: 0,
            totals,
            byte_count,
        }
    }

    pub fn with_expectations(mut self, revision: i64, full_generation: i64) -> Self {
        self.expected_revision = revision;
        self.expected_full_generation = full_generation;
        self
    }

    fn is_fresh_leaf(&self) -> bool {
        self.completion.is_some()
            && self.failure.is_none()
            && self.expected_revision == 0
            && self.records.len() < FRESH_LEAF_MAX_FOLDER_RECORDS
            && self.records.metas().iter().all(|m| m.kind != orbit_platform::EntryKind::Directory)
    }
}

/// Discovery counters behind the mapping footer. The estimator lives in `progress`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DiscoveryProgress {
    pub files: u64,
    pub records: u64,
    pub logical_size: u64,
    pub completed_directories: u64,
    pub active_directories: u64,
    pub queued_directories: u64,
    /// Recent ratio of child directories per completed directory (frontier growth signal).
    pub recent_children: f64,
    /// Records + one per batch accepted for saving.
    pub accepted_save_work: u64,
    /// Same unit, acknowledged by the writer.
    pub settled_save_work: u64,
    pub finished: bool,
    /// Totals cannot be trusted as complete (failures, deferred retries).
    pub uncertain: bool,
    /// Some discovered data still needs importing from a reused map.
    pub cache_uncertain: bool,
    pub cancelled: bool,
}

impl DiscoveryProgress {
    /// Share of discovered work already durable, or `None` while it is not comparable.
    pub fn cache_fraction(&self) -> Option<f64> {
        if self.cancelled || self.uncertain || self.cache_uncertain || self.accepted_save_work == 0 {
            return None;
        }
        Some((self.settled_save_work as f64 / self.accepted_save_work as f64).min(1.0))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MailboxStats {
    pub pending_bytes: usize,
    pub peak_pending_bytes: usize,
    pub pending_batches: u64,
    pub committed: u64,
    pub sequence: u64,
    pub frontier: usize,
    pub peak_frontier: usize,
    pub frontier_bytes: usize,
    pub peak_frontier_bytes: usize,
    pub reuse_deferred: usize,
    pub retry_deferred: usize,
}

type Page = Arc<Vec<Arc<ListingBatch>>>;

#[derive(Default)]
struct State {
    /// Full pages, oldest first. `frozen[0][0]` has sequence `frozen_base`.
    frozen: VecDeque<Page>,
    frozen_base: u64,
    /// The page being filled.
    open: Vec<Arc<ListingBatch>>,
    sequence: u64,
    committed: u64,
    revision: u64,
    bytes: usize,
    stats: MailboxStats,
    finished: bool,
    finished_at: Option<Instant>,
    cancelled: bool,
    progress: DiscoveryProgress,
}

impl State {
    fn batch_at(&self, sequence: u64) -> Option<&Arc<ListingBatch>> {
        if sequence < self.frozen_base || sequence > self.sequence {
            return None;
        }
        let index = (sequence - self.frozen_base) as usize;
        let (page, offset) = (index / PAGE, index % PAGE);
        match self.frozen.get(page) {
            Some(p) => p.get(offset),
            None => self.open.get(index - self.frozen.len() * PAGE),
        }
    }
}

type Hook = Box<dyn Fn() + Send + Sync>;
type BatchHook = Box<dyn Fn(&ListingBatch) + Send + Sync>;

pub struct Mailbox {
    generation: uuid::Uuid,
    root_path: Arc<[u8]>,
    root_identity: FileIdentity,
    cancellation: Cancellation,
    /// Mirrors `State::cancelled` so hot-path checks don't take the lock.
    cancelled: AtomicBool,
    state: Mutex<State>,
    changed: Condvar,
    on_accepted: Hook,
    on_batch: BatchHook,
}

impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailbox").field("generation", &self.generation).finish_non_exhaustive()
    }
}

impl Mailbox {
    /// `on_accepted` runs (outside the lock) whenever new work is enqueued or discovery ends;
    /// it typically wakes the writer and the UI. `on_batch` runs from `observe`, before the
    /// batch is enqueued, and feeds the live inventory.
    pub fn new(
        root_path: Arc<[u8]>,
        root_identity: FileIdentity,
        cancellation: Cancellation,
        on_accepted: impl Fn() + Send + Sync + 'static,
        on_batch: impl Fn(&ListingBatch) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            generation: uuid::Uuid::new_v4(),
            root_path,
            root_identity,
            cancellation,
            cancelled: AtomicBool::new(false),
            state: Mutex::new(State { frozen_base: 1, ..State::default() }),
            changed: Condvar::new(),
            on_accepted: Box::new(on_accepted),
            on_batch: Box::new(on_batch),
        })
    }

    pub fn generation(&self) -> uuid::Uuid {
        self.generation
    }

    pub fn root_path(&self) -> &Arc<[u8]> {
        &self.root_path
    }

    pub fn root_identity(&self) -> FileIdentity {
        self.root_identity
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled() || self.cancelled.load(Ordering::Acquire)
    }

    fn check_live(&self, state: &State) -> crate::Result<()> {
        if state.cancelled || self.cancellation.is_cancelled() { Err(crate::CoreError::Cancelled) } else { Ok(()) }
    }

    /// Accounts a discovered batch and hands it to the live inventory. `save` is false for
    /// batches that are only shown (e.g. served from a reused map); `needs_import` marks data
    /// the cache does not hold yet.
    pub fn observe(&self, batch: &ListingBatch, save: bool, needs_import: bool) -> crate::Result<()> {
        if self.is_cancelled() {
            return Err(crate::CoreError::Cancelled);
        }
        (self.on_batch)(batch);
        let mut state = self.state.lock();
        self.check_live(&state)?;
        let progress = &mut state.progress;
        progress.records += batch.records.len() as u64;
        progress.files += batch.totals.files;
        progress.logical_size += batch.totals.logical;
        if save {
            progress.accepted_save_work += batch.records.len() as u64 + 1;
        }
        if needs_import {
            progress.cache_uncertain = true;
        }
        if batch.failure.is_some() {
            progress.uncertain = true;
        }
        state.revision = state.revision.wrapping_add(1);
        Ok(())
    }

    /// Queues a batch for the writer and returns its sequence number.
    pub fn enqueue(&self, mut batch: ListingBatch) -> crate::Result<u64> {
        let sequence = {
            let mut state = self.state.lock();
            self.check_live(&state)?;
            state.sequence += 1;
            batch.sequence = state.sequence;
            state.bytes += batch.byte_count;
            state.stats.peak_pending_bytes = state.stats.peak_pending_bytes.max(state.bytes);
            state.open.push(Arc::new(batch));
            if state.open.len() == PAGE {
                let page = std::mem::replace(&mut state.open, Vec::with_capacity(PAGE));
                state.frozen.push_back(Arc::new(page));
            }
            state.revision = state.revision.wrapping_add(1);
            state.sequence
        };
        self.changed.notify_all();
        (self.on_accepted)();
        Ok(sequence)
    }

    /// The next run of batches for **one** directory, starting right after the watermark,
    /// stopping at a completion/failure or after `max_records`. Waits briefly when idle.
    pub fn next(&self, max_records: usize) -> Vec<Arc<ListingBatch>> {
        let mut state = self.state.lock();
        if state.committed == state.sequence && !state.finished && !state.cancelled {
            self.changed.wait_for(&mut state, NEXT_WAIT);
        }
        if state.cancelled || state.committed >= state.sequence {
            return Vec::new();
        }
        let Some(first) = state.batch_at(state.committed + 1).cloned() else { return Vec::new() };
        let mut result = Vec::new();
        let mut count = 0;
        for sequence in state.committed + 1..=state.sequence {
            let Some(batch) = state.batch_at(sequence) else { break };
            if batch.parent_path != first.parent_path
                || (!result.is_empty() && count + batch.records.len() > max_records)
            {
                break;
            }
            count += batch.records.len();
            let ends = batch.completion.is_some() || batch.failure.is_some() || count >= max_records;
            result.push(batch.clone());
            if ends {
                break;
            }
        }
        result
    }

    /// A group of small, finished, file-only folders that can be committed in one transaction.
    pub fn next_fresh_leaves(&self) -> Vec<Arc<ListingBatch>> {
        let state = self.state.lock();
        if state.cancelled || state.committed >= state.sequence {
            return Vec::new();
        }
        let mut result: Vec<Arc<ListingBatch>> = Vec::new();
        let (mut records, mut bytes) = (0, 0);
        let mut parents: HashSet<&[u8]> = HashSet::new();
        for sequence in state.committed + 1..=state.sequence {
            let Some(batch) = state.batch_at(sequence) else { break };
            let same_generation =
                result.first().is_none_or(|first| first.expected_full_generation == batch.expected_full_generation);
            if !batch.is_fresh_leaf()
                || !same_generation
                || records + batch.records.len() > FRESH_LEAF_MAX_RECORDS
                || bytes + batch.byte_count > FRESH_LEAF_MAX_BYTES
                || !parents.insert(&batch.parent_path)
            {
                break;
            }
            records += batch.records.len();
            bytes += batch.byte_count;
            result.push(batch.clone());
            if result.len() == FRESH_LEAF_MAX_FOLDERS {
                break;
            }
        }
        result
    }

    /// Marks everything through `sequence` as durable and releases it.
    pub fn acknowledge(&self, sequence: u64) {
        {
            let mut state = self.state.lock();
            let end = sequence.min(state.sequence);
            while state.committed < end {
                let next = state.committed + 1;
                if let Some((bytes, records)) = state.batch_at(next).map(|b| (b.byte_count, b.records.len() as u64)) {
                    state.bytes -= bytes;
                    state.progress.settled_save_work += records + 1;
                }
                state.committed = next;
            }
            while state.frozen_base + PAGE as u64 - 1 <= state.committed && !state.frozen.is_empty() {
                state.frozen.pop_front();
                state.frozen_base += PAGE as u64;
            }
            state.revision = state.revision.wrapping_add(1);
        }
        self.changed.notify_all();
    }

    /// A consistent view of every not-yet-acknowledged batch, cheap enough to take per UI read.
    pub fn capture(self: &Arc<Self>) -> MailboxSnapshot {
        let state = self.state.lock();
        MailboxSnapshot {
            generation: self.generation,
            revision: state.revision,
            root_path: self.root_path.clone(),
            root_identity: self.root_identity,
            committed: state.committed,
            first_sequence: state.frozen_base,
            pages: state.frozen.iter().cloned().collect(),
            open: state.open.clone(),
            mailbox: Arc::downgrade(self),
        }
    }

    pub fn update_frontier(&self, count: usize, bytes: usize, active: u64, completed: u64, recent_children: f64) {
        let mut state = self.state.lock();
        if state.cancelled {
            return;
        }
        let stats = &mut state.stats;
        stats.frontier = count;
        stats.frontier_bytes = bytes;
        stats.peak_frontier = stats.peak_frontier.max(count);
        stats.peak_frontier_bytes = stats.peak_frontier_bytes.max(bytes);
        let progress = &mut state.progress;
        progress.queued_directories = count as u64;
        progress.active_directories = active;
        progress.completed_directories = completed;
        progress.recent_children = recent_children;
    }

    /// Directories skipped now because a reused map will supply them later.
    pub fn defer_for_reuse(&self, count: usize) {
        let mut state = self.state.lock();
        if !state.cancelled {
            state.stats.reuse_deferred += count;
            state.progress.uncertain = true;
        }
    }

    /// Directories whose listing must be retried by the writer's local pass.
    pub fn defer_retries(&self, count: usize) {
        let mut state = self.state.lock();
        if !state.cancelled {
            state.stats.retry_deferred += count;
            state.progress.uncertain = true;
        }
    }

    pub fn require_reconciliation(&self) {
        self.state.lock().progress.uncertain = true;
    }

    /// Discovery produced its last batch.
    pub fn finish(&self) {
        {
            let mut state = self.state.lock();
            if state.cancelled {
                return;
            }
            state.finished = true;
            state.finished_at = Some(Instant::now());
            state.revision = state.revision.wrapping_add(1);
        }
        self.changed.notify_all();
        (self.on_accepted)();
    }

    /// Drops all pending work. Readers holding snapshots keep their batches alive.
    pub fn cancel(&self, notify: bool) {
        {
            let mut state = self.state.lock();
            state.cancelled = true;
            self.cancelled.store(true, Ordering::Release);
            state.finished = true;
            state.revision = state.revision.wrapping_add(1);
            state.frozen.clear();
            state.open.clear();
            state.committed = state.sequence;
            state.frozen_base = state.sequence + 1;
            state.bytes = 0;
            state.stats.frontier = 0;
            state.stats.frontier_bytes = 0;
        }
        self.changed.notify_all();
        if notify {
            (self.on_accepted)();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.state.lock().finished
    }

    pub fn finished_at(&self) -> Option<Instant> {
        self.state.lock().finished_at
    }

    /// Discovery finished and the writer has acknowledged everything.
    pub fn is_drained(&self) -> bool {
        let state = self.state.lock();
        state.finished && state.committed == state.sequence
    }

    pub fn last_committed(&self) -> u64 {
        self.state.lock().committed
    }

    pub fn revision(&self) -> u64 {
        self.state.lock().revision
    }

    pub fn progress(&self) -> DiscoveryProgress {
        let state = self.state.lock();
        let mut progress = state.progress;
        progress.finished = state.finished;
        progress.cancelled = state.cancelled || self.cancellation.is_cancelled();
        progress
    }

    pub fn stats(&self) -> MailboxStats {
        let state = self.state.lock();
        MailboxStats {
            pending_bytes: state.bytes,
            pending_batches: state.sequence - state.committed,
            committed: state.committed,
            sequence: state.sequence,
            ..state.stats
        }
    }
}

/// Immutable view of the unacknowledged batches at capture time.
#[derive(Debug, Clone)]
pub struct MailboxSnapshot {
    pub generation: uuid::Uuid,
    pub revision: u64,
    pub root_path: Arc<[u8]>,
    pub root_identity: FileIdentity,
    /// Watermark at capture; batches at or below it are already durable.
    pub committed: u64,
    first_sequence: u64,
    pages: Vec<Page>,
    open: Vec<Arc<ListingBatch>>,
    mailbox: Weak<Mailbox>,
}

impl MailboxSnapshot {
    /// Batches with `sequence > after`, in sequence order. Pass the watermark read from the
    /// database inside the reader's transaction (always `>= self.committed`).
    pub fn batches_after(&self, after: u64) -> impl Iterator<Item = &Arc<ListingBatch>> + '_ {
        let after = after.max(self.committed);
        let skip = after.saturating_sub(self.first_sequence - 1) as usize;
        self.pages.iter().flat_map(|page| page.iter()).chain(self.open.iter()).skip(skip)
    }

    /// The mailbox still exists and has not been cancelled.
    pub fn is_current(&self) -> bool {
        self.mailbox.upgrade().is_some_and(|m| !m.is_cancelled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_platform::{EntryKind, EntryRecord};

    fn id(n: u64) -> FileIdentity {
        FileIdentity { dev: 1, ino: n }
    }

    fn batch(parent: &str, files: usize, dirs: usize, complete: bool) -> ListingBatch {
        let mut records = RecordBatch::default();
        for i in 0..files + dirs {
            let name = format!("e{i}");
            records.push(&EntryRecord {
                name: name.as_bytes(),
                kind: if i < files { EntryKind::File } else { EntryKind::Directory },
                id: id(i as u64 + 100),
                nlink: 1,
                size: 10,
                allocated: 4096,
                is_mount_point: false,
            });
        }
        ListingBatch::new(
            Arc::from(parent.as_bytes()),
            id(1),
            Arc::from(Vec::new()),
            records,
            complete.then(ListingCompletion::default),
            None,
        )
    }

    fn mailbox() -> Arc<Mailbox> {
        Mailbox::new(Arc::from(&b"/r"[..]), id(1), Cancellation::new(), || {}, |_| {})
    }

    #[test]
    fn snapshot_respects_watermark_across_page_boundaries() {
        let m = mailbox();
        for i in 0..300 {
            m.enqueue(batch(&format!("/r/{i}"), 1, 0, true)).unwrap();
        }
        m.acknowledge(130); // frees the first page only
        let snap = m.capture();
        assert_eq!(snap.batches_after(0).map(|b| b.sequence).next(), Some(131));
        assert_eq!(snap.batches_after(0).count(), 170);
        // A reader whose transaction saw a newer watermark skips more.
        assert_eq!(snap.batches_after(250).map(|b| b.sequence).next(), Some(251));
        // Later acknowledgements do not disturb an existing snapshot.
        m.acknowledge(300);
        assert_eq!(snap.batches_after(0).count(), 170);
        assert_eq!(m.stats().pending_bytes, 0);
    }

    #[test]
    fn next_stays_within_one_directory_and_stops_at_completion() {
        let m = mailbox();
        m.enqueue(batch("/r/a", 128, 0, false)).unwrap();
        m.enqueue(batch("/r/a", 5, 0, true)).unwrap();
        m.enqueue(batch("/r/b", 5, 0, true)).unwrap();
        let run = m.next(1024);
        assert_eq!(run.iter().map(|b| b.sequence).collect::<Vec<_>>(), [1, 2]);
        m.acknowledge(2);
        assert_eq!(m.next(1024).iter().map(|b| b.sequence).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn fresh_leaves_group_only_small_file_only_folders() {
        let m = mailbox();
        m.enqueue(batch("/r/a", 3, 0, true)).unwrap();
        m.enqueue(batch("/r/b", 3, 0, true)).unwrap();
        m.enqueue(batch("/r/c", 3, 1, true)).unwrap(); // has a subdirectory: ends the group
        m.enqueue(batch("/r/d", 3, 0, true)).unwrap();
        assert_eq!(m.next_fresh_leaves().len(), 2);
    }

    #[test]
    fn cancel_releases_pending_work() {
        let m = mailbox();
        m.enqueue(batch("/r/a", 3, 0, true)).unwrap();
        m.cancel(false);
        assert!(m.next(1024).is_empty());
        assert!(m.enqueue(batch("/r/b", 1, 0, true)).is_err());
        assert!(m.is_drained());
    }
}
