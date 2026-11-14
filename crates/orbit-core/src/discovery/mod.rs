//! Parallel directory discovery (Swift `AsyncListingProducer` + `DiscoveryScheduler` +
//! `DiscoveryListingSpool` + `SecureDirectoryCursor`).
//!
//! A pool of workers lists directories through the platform layer and publishes every batch
//! twice: immediately into the [`LiveInventory`] (through [`Mailbox::observe`] and its
//! `on_batch` hook) and, in parent-before-child ticket order, into the [`Mailbox`] for the
//! durable writer. Discovery never waits for the writer.
//!
//! # Scheduling
//! Work stealing (`crossbeam-deque`): each worker pushes the subdirectories it finds onto its
//! own LIFO deque (depth first, so its descriptor cache keeps hitting) and idle workers steal
//! the oldest (shallowest) entries from others. A directory is *claimed* in the live inventory
//! before it is listed; the claim is what makes a queued job valid, so stale jobs (replaced or
//! dropped directories) and duplicates are skipped. Tickets are assigned at claim time.
//!
//! # Preferred path
//! [`DiscoveryHandle::prioritize`] (or [`DiscoveryOptions::preferred`]) makes workers claim the
//! next unlisted directory on the way to that path before taking queued work, so the folder a
//! user is looking at is listed first (Swift `checkPreference`). Preference ends when the path
//! itself has been claimed or turns out to be unreachable.
//!
//! # Safety
//! Every directory is opened component by component from the root with identity checks
//! ([`cursor`]); symlinks are never followed; mount points and other devices are not entered
//! ([`TraversalScope`]); excluded directories (cache, stage, cloud) are filtered by path and
//! identity before their record is published. Every worker runs inside
//! [`Platform::without_materialization`].

pub mod cache;
pub(crate) mod cursor;
pub mod exclusions;
mod spool;

use crate::live::arena::{NONE, NodeId};
use crate::live::{LiveInventory, Preferred};
use crate::mailbox::{DirComponent, ListingBatch, ListingCompletion, ListingFailure, Mailbox};
use crate::records::RecordBatch;
use crate::scope::{TraversalScope, join_into};
use crate::{CoreError, Result};
use cache::{ListingCache, ListingCacheReader};
use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use cursor::Cursor;
pub use cursor::{CursorStats, MAX_CACHED};
pub(crate) use exclusions::path_bytes;
pub use exclusions::{Exclusions, scan_exclusions};
use hashbrown::HashTable;
use orbit_platform::{
    Cancellation, DirectoryLister, EntryKind, EntryRecord, FileIdentity, Issue, LISTING_BATCH, ListingSink, Platform,
    PlatformError, Stop,
};
use parking_lot::{Condvar, Mutex, RwLock};
use spool::Spool;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Default worker count. On a whole Mac (Apple M5, 10 cores, Nov 2026) discovery alone took
/// 26–28 s with four workers and 17–19 s with eight; 12 and 16 were no faster (17.0 and 18.5 s)
/// and spent far more system CPU. Opening a folder for reading is serialized system-wide (about
/// 18 µs each); listing and the metadata reads behind it scale, so eight workers keep the open
/// path busy while others list. On a small, cached fixture 1–8 workers can measure the same
/// (`docs/benchmarks/rust-port/discovery-scaling.md`): more workers help where folders are not
/// cached, as on a whole drive.
pub const DEFAULT_WORKERS: usize = 8;
/// Records in the first published batch of a directory (immediately visible, fresh-leaf sized).
pub const FIRST_BATCH: usize = LISTING_BATCH;
/// Records per later batch of a wide directory (fewer lock round trips).
pub const LATER_BATCH: usize = 1024;
/// Issue messages kept per listing (all are counted).
const MAX_ISSUES: usize = 100;
/// Each worker re-checks that the root still names the mapped root after this many listings.
const ROOT_CHECK_INTERVAL: u32 = 64;

/// Test and diagnostics hooks. Defaults do nothing.
pub trait DiscoveryHooks: Send + Sync {
    /// Called on the worker right before a claimed directory is read.
    fn before_directory(&self, _path: &[u8], _ticket: u64) {}
    /// May rewrite each delivered chunk of records before they are accepted.
    fn records(&self, _path: &[u8], _records: &mut RecordBatch) {}
}

pub type DeferDirectory = dyn Fn(&[u8]) -> bool + Send + Sync;

#[derive(Clone)]
pub struct DiscoveryOptions {
    /// Worker threads (clamped to 1..=64).
    pub workers: usize,
    pub exclusions: Exclusions,
    /// List this folder (and the way to it) first.
    pub preferred: Option<Vec<u8>>,
    /// Reuse completed listings of a resumed stage.
    pub cache: Option<Arc<dyn ListingCache>>,
    /// Directories a reused map will supply later: skipped and counted as deferred.
    pub defer_directory: Option<Arc<DeferDirectory>>,
    /// Full-refresh generation the batches are produced against (when no cache says otherwise).
    pub full_generation: i64,
    /// Descriptor cache per worker (≤ 64).
    pub max_cached_directories: usize,
    pub hooks: Option<Arc<dyn DiscoveryHooks>>,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            workers: DEFAULT_WORKERS,
            exclusions: Exclusions::none(),
            preferred: None,
            cache: None,
            defer_directory: None,
            full_generation: 0,
            max_cached_directories: MAX_CACHED,
            hooks: None,
        }
    }
}

impl std::fmt::Debug for DiscoveryOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryOptions")
            .field("workers", &self.workers)
            .field("preferred", &self.preferred.as_ref().map(|p| String::from_utf8_lossy(p).into_owned()))
            .field("cache", &self.cache.is_some())
            .finish_non_exhaustive()
    }
}

/// Counters reported when discovery ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiscoveryReport {
    pub directories: u64,
    pub records: u64,
    pub cache_hits: u64,
    pub failures: u64,
    /// Queued jobs skipped because their directory was claimed elsewhere, replaced or dropped.
    pub skipped: u64,
    pub deferred: u64,
    pub cursor: CursorStats,
    pub peak_spool_batches: usize,
    /// Most bytes waiting for the writer in the mailbox.
    pub peak_pending_bytes: usize,
    /// Time workers waited for the writer at the mailbox's pending limit.
    pub writer_wait: Duration,
    pub cancelled: bool,
    /// The mapped root was replaced during discovery (discovery was cancelled).
    pub root_replaced: bool,
    pub elapsed: Duration,
}

/// A queued directory.
struct Job {
    /// Live node of the parent (`NONE` for the root).
    parent: NodeId,
    path: Arc<[u8]>,
    identity: FileIdentity,
    components: Arc<[DirComponent]>,
}

impl Job {
    fn byte_count(&self) -> usize {
        128 + self.path.len() + self.components.iter().map(|c| 64 + c.name.len()).sum::<usize>()
    }
}

/// A claimed directory being listed.
struct Claimed {
    node: NodeId,
    path: Arc<[u8]>,
    identity: FileIdentity,
    components: Arc<[DirComponent]>,
}

#[derive(Default)]
struct Counters {
    directories: AtomicU64,
    records: AtomicU64,
    cache_hits: AtomicU64,
    failures: AtomicU64,
    skipped: AtomicU64,
    deferred: AtomicU64,
    opens: AtomicU64,
    revalidations: AtomicU64,
    uncached: AtomicU64,
}

struct Frontier {
    completed: u64,
    recent_children: f64,
}

struct Shared<P: Platform> {
    platform: Arc<P>,
    scope: TraversalScope,
    live: Arc<LiveInventory>,
    mailbox: Arc<Mailbox>,
    spool: Spool,
    exclusions: Exclusions,
    cache: Option<Arc<dyn ListingCache>>,
    defer_directory: Option<Arc<DeferDirectory>>,
    hooks: Option<Arc<dyn DiscoveryHooks>>,
    full_generation: i64,
    max_cached: usize,
    cancel: Cancellation,
    /// Publishers hold it shared; cancellation takes it exclusively so nothing is published
    /// after `cancel` returns.
    publication: RwLock<()>,
    injector: Injector<Job>,
    stealers: Vec<Stealer<Job>>,
    /// Queued + active jobs; zero means discovery is done.
    outstanding: AtomicUsize,
    active: AtomicUsize,
    frontier_bytes: AtomicUsize,
    frontier: Mutex<Frontier>,
    epoch: AtomicU64,
    sleepers: AtomicUsize,
    sleep: Mutex<()>,
    wake: Condvar,
    tickets: AtomicU64,
    preferred: Mutex<Option<Vec<u8>>>,
    preference_active: AtomicBool,
    running: AtomicUsize,
    finished: AtomicBool,
    root_replaced: AtomicBool,
    counters: Counters,
    started: Instant,
    elapsed: Mutex<Option<Duration>>,
}

trait Control: Send + Sync {
    fn prioritize(&self, path: Option<&[u8]>);
    fn cancel(&self);
    fn is_finished(&self) -> bool;
    fn report(&self) -> DiscoveryReport;
}

/// Running discovery. Dropping the handle does not stop discovery; call [`cancel`] for that.
///
/// [`cancel`]: DiscoveryHandle::cancel
pub struct DiscoveryHandle {
    control: Arc<dyn Control>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for DiscoveryHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryHandle").field("workers", &self.threads.len()).finish()
    }
}

impl DiscoveryHandle {
    /// List `path` (and the way to it) before other queued work; `None` clears the preference.
    pub fn prioritize(&self, path: Option<&[u8]>) {
        self.control.prioritize(path);
    }

    /// Stops all workers. Nothing is published to the live inventory or mailbox after this
    /// returns; the mailbox is cancelled.
    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// All workers have ended (the mailbox is finished or cancelled).
    pub fn is_finished(&self) -> bool {
        self.control.is_finished()
    }

    pub fn report(&self) -> DiscoveryReport {
        self.control.report()
    }

    /// Waits for all workers.
    pub fn join(self) -> DiscoveryReport {
        for thread in self.threads {
            let _ = thread.join();
        }
        self.control.report()
    }
}

/// Converts core path bytes to a native path.
pub(crate) fn native_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Starts discovery of the mailbox's root.
///
/// `live` must be the inventory installed as `mailbox`'s `on_batch` hook (see
/// [`LiveInventory::hook`]) and built for the same root; `cancel` should be the cancellation
/// given to the mailbox. Fails when the root cannot be opened or is no longer the directory the
/// mailbox was created for.
pub fn start<P: Platform>(
    platform: Arc<P>,
    mailbox: Arc<Mailbox>,
    live: Arc<LiveInventory>,
    options: DiscoveryOptions,
    cancel: Cancellation,
) -> Result<DiscoveryHandle> {
    let scope = live.scope().clone();
    if scope.root_path() != &mailbox.root_path()[..] || scope.root_identity() != mailbox.root_identity() {
        return Err(CoreError::Invalid("the live inventory and mailbox describe different roots".into()));
    }
    let root_path = native_path(scope.root_path());
    let identity = platform.without_materialization(|| platform.open_root(&root_path).map(|(_, id)| id))??;
    if identity != scope.root_identity() {
        return Err(PlatformError::IdentityChanged { path: root_path }.into());
    }
    let workers = options.workers.clamp(1, 64);
    let locals: Vec<Worker<Job>> = (0..workers).map(|_| Worker::new_lifo()).collect();
    let stealers = locals.iter().map(Worker::stealer).collect();
    let preferred = options.preferred.clone();
    let shared = Arc::new(Shared {
        platform,
        live,
        spool: Spool::new(mailbox.clone()),
        mailbox,
        exclusions: options.exclusions,
        cache: options.cache,
        defer_directory: options.defer_directory,
        hooks: options.hooks,
        full_generation: options.full_generation,
        max_cached: options.max_cached_directories,
        cancel,
        publication: RwLock::new(()),
        injector: Injector::new(),
        stealers,
        outstanding: AtomicUsize::new(1),
        active: AtomicUsize::new(0),
        frontier_bytes: AtomicUsize::new(0),
        frontier: Mutex::new(Frontier { completed: 0, recent_children: 0.0 }),
        epoch: AtomicU64::new(0),
        sleepers: AtomicUsize::new(0),
        sleep: Mutex::new(()),
        wake: Condvar::new(),
        tickets: AtomicU64::new(0),
        preference_active: AtomicBool::new(preferred.is_some()),
        preferred: Mutex::new(preferred),
        running: AtomicUsize::new(workers),
        finished: AtomicBool::new(false),
        root_replaced: AtomicBool::new(false),
        counters: Counters::default(),
        started: Instant::now(),
        elapsed: Mutex::new(None),
        scope,
    });
    let root = Job {
        parent: NONE,
        path: shared.live.root_path().clone(),
        identity: shared.scope.root_identity(),
        components: Arc::from(Vec::new()),
    };
    shared.frontier_bytes.store(root.byte_count(), Ordering::Relaxed);
    shared.mailbox.update_frontier(1, root.byte_count(), 0, 0, 0.0);
    shared.injector.push(root);
    let mut threads = Vec::with_capacity(workers);
    for (index, local) in locals.into_iter().enumerate() {
        let shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name(format!("orbit-discovery-{index}"))
            .spawn(move || shared.run_worker(index, local))
            .map_err(CoreError::Io)?;
        threads.push(thread);
    }
    Ok(DiscoveryHandle { control: shared, threads })
}

impl<P: Platform> Control for Shared<P> {
    fn prioritize(&self, path: Option<&[u8]>) {
        let mut preferred = self.preferred.lock();
        *preferred = path.map(<[u8]>::to_vec);
        self.preference_active.store(preferred.is_some(), Ordering::Release);
        drop(preferred);
        self.wake_all();
    }

    fn cancel(&self) {
        self.cancel_all();
    }

    fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    fn report(&self) -> DiscoveryReport {
        let c = &self.counters;
        let mailbox = self.mailbox.stats();
        DiscoveryReport {
            directories: c.directories.load(Ordering::Relaxed),
            records: c.records.load(Ordering::Relaxed),
            cache_hits: c.cache_hits.load(Ordering::Relaxed),
            failures: c.failures.load(Ordering::Relaxed),
            skipped: c.skipped.load(Ordering::Relaxed),
            deferred: c.deferred.load(Ordering::Relaxed),
            cursor: CursorStats {
                opens: c.opens.load(Ordering::Relaxed),
                revalidations: c.revalidations.load(Ordering::Relaxed),
                uncached: c.uncached.load(Ordering::Relaxed),
            },
            peak_spool_batches: self.spool.peak_buffered(),
            peak_pending_bytes: mailbox.peak_pending_bytes,
            writer_wait: mailbox.limited,
            cancelled: self.cancel.is_cancelled(),
            root_replaced: self.root_replaced.load(Ordering::Relaxed),
            elapsed: self.elapsed.lock().unwrap_or_else(|| self.started.elapsed()),
        }
    }
}

enum Task {
    Queued(Job),
    Preferred(crate::live::ClaimedDir),
}

impl<P: Platform> Shared<P> {
    fn cancel_all(&self) {
        {
            let _fence = self.publication.write();
            self.cancel.cancel();
            self.mailbox.cancel(false);
        }
        self.spool.cancel();
        self.wake_all();
        // Wake the writer and UI once the fence is released.
        self.mailbox.cancel(true);
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled() || self.mailbox.is_cancelled()
    }

    fn wake_all(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        if self.sleepers.load(Ordering::SeqCst) > 0 {
            let _guard = self.sleep.lock();
            self.wake.notify_all();
        }
    }

    fn run_worker(self: Arc<Self>, index: usize, local: Worker<Job>) {
        let outcome = self.platform.without_materialization(|| self.work(index, &local));
        if outcome.is_err() {
            self.cancel_all();
        }
        if self.running.fetch_sub(1, Ordering::AcqRel) == 1 {
            let cancelled = self.is_cancelled();
            self.spool.finish(cancelled);
            *self.elapsed.lock() = Some(self.started.elapsed());
            self.finished.store(true, Ordering::Release);
        }
    }

    fn work(&self, index: usize, local: &Worker<Job>) {
        let mut cursor = match Cursor::new(&*self.platform, &self.scope, self.max_cached) {
            Ok(cursor) => cursor,
            Err(_) => {
                self.root_replaced.store(true, Ordering::Relaxed);
                self.cancel_all();
                return;
            }
        };
        let mut lister = self.platform.new_lister();
        let mut reader = self.cache.as_ref().and_then(|c| c.open_reader());
        let mut scratch = WorkerScratch::default();
        let mut since_root_check = 0u32;
        while let Some(task) = self.next_task(index, local) {
            let claimed = match task {
                Task::Queued(job) => {
                    // Claims mutate the live inventory, so they sit behind the same fence as
                    // publication: nothing changes after `cancel` returns.
                    let node = {
                        let _fence = self.publication.read();
                        if self.cancel.is_cancelled() {
                            None
                        } else if job.parent == NONE {
                            self.live.claim_root().then_some(crate::live::arena::ROOT)
                        } else {
                            let name = &job.components.last().expect("child jobs have components").name;
                            self.live.claim_child(job.parent, name, job.identity)
                        }
                    };
                    let bytes = job.byte_count();
                    self.frontier_bytes.fetch_sub(bytes, Ordering::Relaxed);
                    match node {
                        Some(node) => {
                            Claimed { node, path: job.path, identity: job.identity, components: job.components }
                        }
                        None => {
                            self.counters.skipped.fetch_add(1, Ordering::Relaxed);
                            self.complete(Vec::new(), local);
                            continue;
                        }
                    }
                }
                Task::Preferred(dir) => Claimed {
                    node: dir.node,
                    path: Arc::from(dir.path),
                    identity: dir.identity,
                    components: Arc::from(dir.components),
                },
            };
            self.active.fetch_add(1, Ordering::Relaxed);
            let ticket = self.tickets.fetch_add(1, Ordering::Relaxed);
            if let Some(hooks) = &self.hooks {
                hooks.before_directory(&claimed.path, ticket);
            }
            let children =
                self.discover(&claimed, ticket, &mut cursor, &mut lister, reader.as_deref_mut(), &mut scratch);
            self.spool.close(ticket);
            self.active.fetch_sub(1, Ordering::Relaxed);
            since_root_check += 1;
            if since_root_check >= ROOT_CHECK_INTERVAL {
                since_root_check = 0;
                self.check_root(&cursor);
            }
            self.complete(children, local);
        }
        self.check_root(&cursor);
        let stats = cursor.stats;
        self.counters.opens.fetch_add(stats.opens, Ordering::Relaxed);
        self.counters.revalidations.fetch_add(stats.revalidations, Ordering::Relaxed);
        self.counters.uncached.fetch_add(stats.uncached, Ordering::Relaxed);
    }

    fn check_root(&self, cursor: &Cursor<'_, P>) {
        if !self.is_cancelled() && cursor.validate_root().is_err() {
            self.root_replaced.store(true, Ordering::Relaxed);
            self.cancel_all();
        }
    }

    /// Queues `children` and retires the job that produced them.
    fn complete(&self, children: Vec<Job>, local: &Worker<Job>) {
        let count = children.len();
        if count > 0 && !self.is_cancelled() {
            self.outstanding.fetch_add(count, Ordering::AcqRel);
            let bytes: usize = children.iter().map(Job::byte_count).sum();
            self.frontier_bytes.fetch_add(bytes, Ordering::Relaxed);
            for child in children {
                local.push(child);
            }
        }
        let (completed, recent) = {
            let mut frontier = self.frontier.lock();
            frontier.completed += 1;
            frontier.recent_children = if frontier.completed == 1 {
                count as f64
            } else {
                frontier.recent_children * 0.875 + count as f64 * 0.125
            };
            (frontier.completed, frontier.recent_children)
        };
        let remaining = self.outstanding.fetch_sub(1, Ordering::AcqRel) - 1;
        let active = self.active.load(Ordering::Relaxed);
        self.mailbox.update_frontier(
            remaining.saturating_sub(active),
            self.frontier_bytes.load(Ordering::Relaxed),
            active as u64,
            completed,
            recent,
        );
        if count > 0 || remaining == 0 {
            self.wake_all();
        }
    }

    fn steal(&self, index: usize, local: &Worker<Job>) -> Option<Job> {
        loop {
            let mut retry = false;
            match self.injector.steal_batch_and_pop(local) {
                Steal::Success(job) => return Some(job),
                Steal::Retry => retry = true,
                Steal::Empty => {}
            }
            let n = self.stealers.len();
            for offset in 1..n {
                match self.stealers[(index + offset) % n].steal_batch_and_pop(local) {
                    Steal::Success(job) => return Some(job),
                    Steal::Retry => retry = true,
                    Steal::Empty => {}
                }
            }
            if !retry {
                return None;
            }
        }
    }

    fn next_task(&self, index: usize, local: &Worker<Job>) -> Option<Task> {
        loop {
            if self.is_cancelled() {
                return None;
            }
            let epoch = self.epoch.load(Ordering::SeqCst);
            if self.preference_active.load(Ordering::Acquire) {
                let preferred = self.preferred.lock().clone();
                if let Some(path) = preferred {
                    let claimed = {
                        let _fence = self.publication.read();
                        if self.cancel.is_cancelled() {
                            return None;
                        }
                        self.live.claim_toward(&path)
                    };
                    match claimed {
                        Preferred::Claimed(dir) => {
                            self.outstanding.fetch_add(1, Ordering::AcqRel);
                            return Some(Task::Preferred(dir));
                        }
                        Preferred::Waiting => {}
                        Preferred::Done => {
                            let guard = self.preferred.lock();
                            if guard.as_deref() == Some(&path[..]) {
                                self.preference_active.store(false, Ordering::Release);
                            }
                        }
                    }
                }
            }
            if let Some(job) = local.pop().or_else(|| self.steal(index, local)) {
                return Some(Task::Queued(job));
            }
            if self.outstanding.load(Ordering::Acquire) == 0 {
                return None;
            }
            let mut guard = self.sleep.lock();
            self.sleepers.fetch_add(1, Ordering::SeqCst);
            if self.epoch.load(Ordering::SeqCst) == epoch
                && self.outstanding.load(Ordering::Acquire) != 0
                && !self.is_cancelled()
            {
                // The epoch protocol makes wake-ups reliable; the timeout is only a safety net.
                self.wake.wait_for(&mut guard, Duration::from_millis(5));
            }
            self.sleepers.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn publish(&self, ticket: u64, batch: ListingBatch, save: bool) -> Result<()> {
        {
            let _fence = self.publication.read();
            if self.cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            self.mailbox.observe(&batch, save, false)?;
        }
        self.counters.records.fetch_add(batch.records.len() as u64, Ordering::Relaxed);
        if save {
            self.spool.append(ticket, batch);
        }
        Ok(())
    }

    /// Lists one claimed directory and returns its subdirectories to queue.
    fn discover(
        &self,
        dir: &Claimed,
        ticket: u64,
        cursor: &mut Cursor<'_, P>,
        lister: &mut P::Lister,
        reader: Option<&mut (dyn ListingCacheReader + 'static)>,
        scratch: &mut WorkerScratch,
    ) -> Vec<Job> {
        self.counters.directories.fetch_add(1, Ordering::Relaxed);
        let mut revision = 0;
        let mut generation = self.full_generation;
        if let Some(reader) = reader {
            generation = reader.generation();
            if let Some(cached) = reader.listing(&dir.path, dir.identity) {
                revision = cached.revision;
                generation = cached.generation;
                if let Some(records) = cached.records {
                    let mut listing = Listing::new(self, dir, ticket, cached.import, revision, generation, scratch);
                    let records: Vec<EntryRecord<'_>> = records.iter().collect();
                    let accepted = listing.accept(&records).is_ok() && listing.finish(0, Vec::new()).is_ok();
                    if accepted {
                        self.counters.cache_hits.fetch_add(1, Ordering::Relaxed);
                        return listing.into_children();
                    }
                    if self.is_cancelled() {
                        return Vec::new();
                    }
                    // A rejected cached listing (duplicates) falls back to the filesystem.
                    self.live.begin(&dir.path, dir.identity);
                }
            }
        }
        let opened = match cursor.open(&dir.components) {
            Ok(opened) => opened,
            Err(error) => return self.fail(dir, ticket, revision, generation, Vec::new(), error.into()),
        };
        let mut listing = Listing::new(self, dir, ticket, true, revision, generation, scratch);
        let result = lister.list(opened.dir(), &dir.path, &self.cancel, &mut listing);
        drop(opened);
        let outcome = match result {
            Ok(_) => {
                let (issue_count, issues) = listing.take_issues();
                listing.finish(issue_count, issues)
            }
            Err(PlatformError::Cancelled) if listing.duplicate => {
                Err(CoreError::Invalid("the directory listed the same name twice".into()))
            }
            Err(PlatformError::Cancelled) if listing.failure.is_some() => Err(listing.failure.take().expect("checked")),
            Err(error) => Err(error.into()),
        };
        let children = listing.into_children();
        match outcome {
            Ok(()) => children,
            Err(error) => self.fail(dir, ticket, revision, generation, children, error),
        }
    }

    /// Swift's `catch` in `discover`: classify, update the live inventory, publish a failure.
    fn fail(
        &self,
        dir: &Claimed,
        ticket: u64,
        revision: i64,
        generation: i64,
        mut children: Vec<Job>,
        error: CoreError,
    ) -> Vec<Job> {
        if self.is_cancelled() || error.is_cancelled() {
            return Vec::new();
        }
        self.counters.failures.fetch_add(1, Ordering::Relaxed);
        let code = match &error {
            CoreError::Platform(e) => e.code(),
            _ => None,
        };
        let kind = code.map(|c| std::io::Error::from_raw_os_error(c).kind());
        let missing = matches!(error, CoreError::Platform(PlatformError::IdentityChanged { .. }))
            || matches!(kind, Some(std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory));
        let denied = matches!(kind, Some(std::io::ErrorKind::PermissionDenied));
        if missing || denied {
            let _fence = self.publication.read();
            if !self.cancel.is_cancelled() {
                self.live.unavailable_node(dir.node, dir.identity, missing);
            }
            children.clear();
        }
        self.mailbox.defer_retries(1);
        let failure = ListingFailure { message: error.to_string(), code };
        let batch = ListingBatch::new(
            dir.path.clone(),
            dir.identity,
            dir.components.clone(),
            RecordBatch::default(),
            None,
            Some(failure),
        )
        .with_expectations(revision, generation);
        if self.publish(ticket, batch, true).is_err() {
            return Vec::new();
        }
        children
    }
}

/// Per-worker buffers reused across directories.
#[derive(Default)]
struct WorkerScratch {
    names: HashTable<u64>,
    children: Vec<ChildRecord>,
    child_names: Vec<u8>,
    path: Vec<u8>,
    hook: RecordBatch,
}

#[derive(Clone, Copy)]
struct ChildRecord {
    name_start: u32,
    name_len: u32,
    identity: FileIdentity,
}

fn name_hash(name: &[u8]) -> u64 {
    let mut h = foldhash::fast::FixedState::with_seed(0x6469_7363_6f76_6572).build_hasher();
    h.write(name);
    h.finish()
}

/// One directory listing in progress: the [`ListingSink`] handed to the platform lister.
struct Listing<'a, P: Platform> {
    shared: &'a Shared<P>,
    dir: &'a Claimed,
    ticket: u64,
    save: bool,
    revision: i64,
    generation: i64,
    pending: RecordBatch,
    published: bool,
    issue_count: u32,
    issues: Vec<String>,
    duplicate: bool,
    failure: Option<CoreError>,
    deferred: usize,
    needs_paths: bool,
    scratch: &'a mut WorkerScratch,
}

impl<'a, P: Platform> Listing<'a, P> {
    fn new(
        shared: &'a Shared<P>,
        dir: &'a Claimed,
        ticket: u64,
        save: bool,
        revision: i64,
        generation: i64,
        scratch: &'a mut WorkerScratch,
    ) -> Self {
        scratch.names.clear();
        scratch.children.clear();
        scratch.child_names.clear();
        let needs_paths =
            shared.exclusions.has_paths() || shared.scope.is_path_sensitive() || shared.defer_directory.is_some();
        Self {
            shared,
            dir,
            ticket,
            save,
            revision,
            generation,
            pending: RecordBatch::with_capacity(16, 256),
            published: false,
            issue_count: 0,
            issues: Vec::new(),
            duplicate: false,
            failure: None,
            deferred: 0,
            needs_paths,
            scratch,
        }
    }

    fn accept(&mut self, records: &[EntryRecord<'_>]) -> std::result::Result<(), Stop> {
        // Reject the whole chunk before exposing any of it when a name repeats.
        for record in records {
            let hash = name_hash(record.name);
            if self.scratch.names.find(hash, |h| *h == hash).is_some() {
                self.duplicate = true;
                return Err(Stop);
            }
            self.scratch.names.insert_unique(hash, hash, |h| *h);
        }
        let shared = self.shared;
        let scope = &shared.scope;
        for record in records {
            if record.kind == EntryKind::Directory {
                if self.needs_paths {
                    join_into(&mut self.scratch.path, &self.dir.path, record.name);
                }
                let path = &self.scratch.path[..if self.needs_paths { self.scratch.path.len() } else { 0 }];
                if shared.exclusions.excludes(path, record.id) || (scope.is_path_sensitive() && scope.excludes(path)) {
                    continue;
                }
                self.pending.push(record);
                let allowed = if scope.is_path_sensitive() {
                    scope.allows(path, record.id.dev, record.is_mount_point)
                } else {
                    !record.is_mount_point && record.id.dev == scope.root_identity().dev
                };
                if allowed {
                    if let Some(defer) = &shared.defer_directory
                        && defer(path)
                    {
                        self.deferred += 1;
                    } else {
                        let start = self.scratch.child_names.len() as u32;
                        self.scratch.child_names.extend_from_slice(record.name);
                        self.scratch.children.push(ChildRecord {
                            name_start: start,
                            name_len: record.name.len() as u32,
                            identity: record.id,
                        });
                    }
                }
            } else {
                self.pending.push(record);
            }
            let threshold = if self.published { LATER_BATCH } else { FIRST_BATCH };
            if self.pending.len() >= threshold && self.flush(None).is_err() {
                return Err(Stop);
            }
        }
        Ok(())
    }

    fn flush(&mut self, completion: Option<ListingCompletion>) -> Result<()> {
        let capacity = if completion.is_some() { 0 } else { LATER_BATCH };
        let records = std::mem::replace(&mut self.pending, RecordBatch::with_capacity(capacity, capacity * 24));
        self.published = true;
        let batch = ListingBatch::new(
            self.dir.path.clone(),
            self.dir.identity,
            self.dir.components.clone(),
            records,
            completion,
            None,
        )
        .with_expectations(self.revision, self.generation);
        match self.shared.publish(self.ticket, batch, self.save) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.failure = Some(CoreError::Cancelled);
                Err(error)
            }
        }
    }

    fn take_issues(&mut self) -> (u32, Vec<String>) {
        (self.issue_count, std::mem::take(&mut self.issues))
    }

    fn finish(&mut self, issue_count: u32, issues: Vec<String>) -> Result<()> {
        self.flush(Some(ListingCompletion { issue_count, issues }))
    }

    fn into_children(self) -> Vec<Job> {
        if self.deferred > 0 {
            self.shared.mailbox.defer_for_reuse(self.deferred);
            self.shared.counters.deferred.fetch_add(self.deferred as u64, Ordering::Relaxed);
        }
        let scratch = self.scratch;
        let mut jobs = Vec::with_capacity(scratch.children.len());
        for child in &scratch.children {
            let name = &scratch.child_names[child.name_start as usize..(child.name_start + child.name_len) as usize];
            let path: Arc<[u8]> = Arc::from(crate::paths::join(&self.dir.path, name));
            let mut components = Vec::with_capacity(self.dir.components.len() + 1);
            components.extend(self.dir.components.iter().cloned());
            components.push(DirComponent { name: name.into(), identity: child.identity });
            jobs.push(Job { parent: self.dir.node, path, identity: child.identity, components: Arc::from(components) });
        }
        jobs
    }
}

impl<P: Platform> ListingSink for Listing<'_, P> {
    fn batch(&mut self, records: &[EntryRecord<'_>]) -> std::result::Result<(), Stop> {
        if self.shared.cancel.is_cancelled() {
            return Err(Stop);
        }
        if let Some(hooks) = &self.shared.hooks {
            let mut chunk = std::mem::take(&mut self.scratch.hook);
            chunk.clear();
            chunk.extend(records);
            hooks.records(&self.dir.path, &mut chunk);
            let rewritten: Vec<EntryRecord<'_>> = chunk.iter().collect();
            let result = self.accept(&rewritten);
            drop(rewritten);
            self.scratch.hook = chunk;
            return result;
        }
        self.accept(records)
    }

    fn issue(&mut self, issue: Issue) {
        self.issue_count = self.issue_count.saturating_add(1);
        if self.issues.len() < MAX_ISSUES {
            self.issues.push(format!("{}: {}", String::from_utf8_lossy(&issue.path), issue.message));
        }
    }
}

#[cfg(test)]
mod tests;
