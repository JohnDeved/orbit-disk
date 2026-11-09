//! One mapping of one root, from start to saved map. Replaces Swift's `ResumableStage` +
//! `DiskScanPreview` orchestration and the mapping worker of `DiskModel`.
//!
//! ```text
//! MappingSession::start(platform, paths, root, options)
//!   1. open the stage (resume committed work; another session holds it -> retry 2 s, then InUse;
//!      already published / invalidated -> retire it and start fresh)
//!   2. removal journal: anchor or replay the stage's cursor before anything is shown
//!   3. pick saved maps of subfolders to reuse (lowers the stage's event floor first)
//!   4. discovery workers -> live inventory (views) + mailbox -> ONE writer thread
//!      (`Stage::step` loop; woken by the mailbox, never spinning)
//!   5. local pass (retries, dirty folders; fed back into the live inventory)
//!   6. publish under the journal fence, retire the stage, then stop
//! ```
//!
//! Reads never wait for the writer. While mapping, views and search come from the live
//! inventory; when it does not cover a folder (or discovery was abandoned for a full
//! reconciliation) from the stage database plus the mailbox's unacknowledged batches; after
//! publication from the saved map. A removal-journal replay that cannot expose the stage yet
//! makes views return `None`.
//!
//! Changes are announced through [`MappingSession::changes`] with latest-value semantics: a
//! monotonic revision, a blocking wait, and an optional callback that fires at most once until
//! the consumer reads the revision again (no backlog, however many batches arrive).

use crate::discovery::{self, DiscoveryHandle, DiscoveryHooks, DiscoveryOptions, Exclusions};
use crate::journal::{Journal, JournalError};
use crate::live::{LiveInventory, SearchHit};
use crate::mailbox::{DiscoveryProgress, Mailbox};
use crate::progress::{DiscoveryEstimator, MappingProgress};
use crate::query::SearchResults;
use crate::records::RecordBatch;
use crate::recovery::{ReplayTarget, ReplayViewHandle, StageReplay, SystemClock};
use crate::reuse::{ReuseCandidate, SessionCache};
use crate::saved::SavedMaps;
use crate::schema::SnapshotInfo;
use crate::scope::TraversalScope;
use crate::stage::cache::StageListingCache;
use crate::stage::{LocalObserver, Opened, Stage, StageCheckpoint, StageConfig, StageReader, StepOutcome, Traversal};
use crate::store::IndexPaths;
use crate::view::FolderTree;
use crate::{CoreError, Result, ScanRemoval};
use orbit_platform::{Cancellation, ChangeCheckpoint, ChangeMonitor, FileIdentity, Platform};
use parking_lot::{Condvar, Mutex, RwLock};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long [`MappingSession::start`] retries a stage another session holds (Swift: 2 s).
pub const STAGE_BUSY_RETRY: Duration = Duration::from_secs(2);
/// Root totals are re-read for the progress footer at most this often.
const WRITER_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound of one idle wait of the writer (journal probes and settlement need a pulse).
const WRITER_IDLE_WAIT: Duration = Duration::from_millis(100);
/// Wait between settlement attempts once the stage has no work (Swift: 50 ms).
const SETTLE_WAIT: Duration = Duration::from_millis(50);

// --- Notifications --------------------------------------------------------------------------

type Callback = Arc<dyn Fn() + Send + Sync>;

/// Latest-value change notification: a monotonic revision, blocking waits, and a callback
/// that fires once per unread change.
pub struct ChangeNotifier {
    revision: AtomicU64,
    armed: AtomicBool,
    waiters: AtomicUsize,
    lock: Mutex<()>,
    changed: Condvar,
    callback: RwLock<Option<Callback>>,
}

impl std::fmt::Debug for ChangeNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeNotifier").field("revision", &self.revision()).finish_non_exhaustive()
    }
}

impl Default for ChangeNotifier {
    fn default() -> Self {
        Self {
            revision: AtomicU64::new(0),
            armed: AtomicBool::new(true),
            waiters: AtomicUsize::new(0),
            lock: Mutex::new(()),
            changed: Condvar::new(),
            callback: RwLock::new(None),
        }
    }
}

impl ChangeNotifier {
    /// Records a change: wakes waiters and, if the consumer has read since the last call, runs
    /// the callback (on the calling thread; it must be cheap and must not call back in).
    pub fn bump(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        if self.armed.swap(false, Ordering::AcqRel)
            && let Some(callback) = self.callback.read().clone()
        {
            callback();
        }
        if self.waiters.load(Ordering::Acquire) > 0 {
            let _guard = self.lock.lock();
            self.changed.notify_all();
        }
    }

    /// The current revision without re-arming the callback.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// The current revision; the next change will run the callback again.
    pub fn take(&self) -> u64 {
        self.armed.store(true, Ordering::Release);
        self.revision()
    }

    /// Runs `callback` after the next unread change (replaces any previous callback).
    pub fn set_callback(&self, callback: Option<Callback>) {
        *self.callback.write() = callback;
        self.armed.store(true, Ordering::Release);
    }

    /// Waits until the revision differs from `seen` or `timeout` passes; returns the revision.
    pub fn wait(&self, seen: u64, timeout: Duration) -> u64 {
        let deadline = Instant::now() + timeout;
        let mut guard = self.lock.lock();
        self.waiters.fetch_add(1, Ordering::AcqRel);
        while self.revision() == seen {
            if self.changed.wait_until(&mut guard, deadline).timed_out() {
                break;
            }
        }
        self.waiters.fetch_sub(1, Ordering::AcqRel);
        self.revision()
    }
}

/// Pause/cancel gate shared by discovery workers and the writer, plus the writer's wake-up.
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    paused: bool,
    cancelled: bool,
    /// Work arrived since the writer last looked.
    wake: bool,
}

impl Gate {
    /// Blocks while paused. Returns false once cancelled.
    fn pass(&self) -> bool {
        self.pass_unless(None)
    }

    /// [`Gate::pass`] that also gives up once `abandoned` is cancelled (checked on every
    /// wake-up; whoever cancels it must call [`Gate::wake_all`]).
    fn pass_unless(&self, abandoned: Option<&Cancellation>) -> bool {
        let mut state = self.state.lock();
        while state.paused && !state.cancelled && !abandoned.is_some_and(Cancellation::is_cancelled) {
            self.changed.wait(&mut state);
        }
        !state.cancelled
    }

    fn wake_all(&self) {
        let _state = self.state.lock();
        self.changed.notify_all();
    }

    fn set(&self, change: impl FnOnce(&mut GateState)) {
        change(&mut self.state.lock());
        self.changed.notify_all();
    }

    fn signal(&self) {
        let mut state = self.state.lock();
        if !state.wake {
            state.wake = true;
            self.changed.notify_all();
        }
    }

    /// Waits for a wake-up, cancellation, or `timeout`, and consumes the wake-up.
    fn sleep(&self, timeout: Duration) {
        let mut state = self.state.lock();
        if !state.wake && !state.cancelled {
            self.changed.wait_for(&mut state, timeout);
        }
        state.wake = false;
    }
}

struct GateHooks {
    gate: Arc<Gate>,
    cancel: Cancellation,
    inner: Option<Arc<dyn DiscoveryHooks>>,
}

impl DiscoveryHooks for GateHooks {
    fn before_directory(&self, path: &[u8], ticket: u64) {
        self.gate.pass_unless(Some(&self.cancel));
        if let Some(inner) = &self.inner {
            inner.before_directory(path, ticket);
        }
    }

    fn records(&self, path: &[u8], records: &mut RecordBatch) {
        if let Some(inner) = &self.inner {
            inner.records(path, records);
        }
    }
}

// --- Configuration ----------------------------------------------------------------------------

/// The traversal scope of `root` on this platform (volume aliases, mount boundaries).
pub fn mapping_scope<P: Platform>(platform: &P, root: &[u8], identity: FileIdentity) -> TraversalScope {
    let layout = platform.volume_layout(&discovery::native_path(root), identity);
    TraversalScope::new(root, identity, layout)
}

/// Directories every mapping skips: the engine's own folders, cloud storage under `home`, and
/// `extra`.
pub fn mapping_exclusions<P: Platform>(
    platform: &P,
    paths: &IndexPaths,
    scope: &TraversalScope,
    extra: &[PathBuf],
) -> Exclusions {
    let mut roots = paths.exclusion_roots();
    roots.extend_from_slice(extra);
    let layout = orbit_platform::VolumeLayout { root_mount: None, boot: scope.boot().cloned() };
    Exclusions::resolve(platform, &roots, &layout)
}

/// The stage policy matching discovery's scope and exclusions.
pub fn stage_config(scope: &TraversalScope, exclusions: &Exclusions) -> StageConfig {
    let traversal = if scope.is_path_sensitive() {
        let scope = scope.clone();
        Traversal::Custom(Arc::new(move |path: &[u8], dev: u64, mount: bool| scope.allows(path, dev, mount)))
    } else {
        Traversal::SameDevice
    };
    StageConfig {
        traversal,
        excluded_paths: exclusions.paths().map(<[u8]>::to_vec).collect(),
        excluded_identities: exclusions.identities().collect(),
        ..StageConfig::default()
    }
}

/// Options of one mapping session.
#[derive(Clone)]
pub struct SessionOptions {
    /// Discovery workers (clamped to 1..=64).
    pub workers: usize,
    /// List this folder (and the way to it) first.
    pub preferred: Option<Vec<u8>>,
    /// The change-history position captured before mapping; `None` captures it with
    /// [`Platform::new_monitor`].
    pub checkpoint: Option<ChangeCheckpoint>,
    /// User home, for cloud-storage exclusions.
    pub home: Option<PathBuf>,
    /// More directories to skip.
    pub exclusions: Vec<PathBuf>,
    /// Reuse listings from saved maps of subfolders (off for forced re-maps).
    pub reuse_saved_maps: bool,
    /// Replay the removal journal into the stage and fence publication with it.
    pub removal_journal: bool,
    /// Delete the stage after a successful publication.
    pub retire_after_publish: bool,
    /// Build the saved map's search accelerator right after publication.
    pub prepare_search: bool,
    /// Test and diagnostics hooks for discovery.
    pub hooks: Option<Arc<dyn DiscoveryHooks>>,
    /// Runs on the writer thread once the stage is complete, right before publication (test
    /// barrier and diagnostics).
    pub before_publish: Option<Arc<dyn Fn() + Send + Sync>>,
    /// How long to retry a stage another session holds before failing with `InUse`.
    pub busy_retry: Duration,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            workers: discovery::DEFAULT_WORKERS,
            preferred: None,
            checkpoint: None,
            home: None,
            exclusions: Vec::new(),
            reuse_saved_maps: true,
            removal_journal: true,
            retire_after_publish: true,
            prepare_search: true,
            hooks: None,
            before_publish: None,
            busy_retry: STAGE_BUSY_RETRY,
        }
    }
}

impl std::fmt::Debug for SessionOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionOptions").field("workers", &self.workers).finish_non_exhaustive()
    }
}

/// Where a session stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Discovery and the writer are running.
    Mapping,
    /// Discovery finished; the writer is saving and reconciling.
    Saving,
    /// Sealing and replacing the saved map.
    Publishing,
    Published,
    Cancelled,
    Failed,
}

/// The end of a session.
#[derive(Clone, Debug)]
pub enum SessionOutcome {
    Running,
    Published(SnapshotInfo),
    Cancelled,
    Failed(Arc<CoreError>),
}

// --- Shared state -----------------------------------------------------------------------------

struct State {
    phase: Phase,
    outcome: SessionOutcome,
    estimator: DiscoveryEstimator,
    cached_file_count: Option<u64>,
    /// The local pass and replay settled (only publication remains).
    finished: bool,
    /// Removal replay asked to re-check everything since the last settlement.
    checking: bool,
}

struct Shared<P: Platform> {
    paths: IndexPaths,
    root: Vec<u8>,
    root_identity: FileIdentity,
    cancel: Cancellation,
    discovery_cancel: Cancellation,
    maps: Arc<SavedMaps<P>>,
    mailbox: Arc<Mailbox>,
    /// The mailbox feeds the writer (discovery was started and not abandoned).
    async_active: AtomicBool,
    live: Arc<RwLock<Option<Arc<LiveInventory>>>>,
    discovery: Mutex<Option<DiscoveryHandle>>,
    /// The report of discovery once it ended.
    report: Mutex<Option<discovery::DiscoveryReport>>,
    resumed: bool,
    stage_reader: StageReader,
    stage_preferred: Arc<Mutex<Option<Vec<u8>>>>,
    replay_view: Option<ReplayViewHandle>,
    gate: Arc<Gate>,
    notifier: Arc<ChangeNotifier>,
    state: Mutex<State>,
    finished: Condvar,
    clock: Instant,
    reused: Vec<ReuseCandidate>,
}

impl<P: Platform> Shared<P> {
    fn set_phase(&self, phase: Phase) {
        let changed = {
            let mut state = self.state.lock();
            let changed = state.phase != phase;
            state.phase = phase;
            changed
        };
        if changed {
            self.notifier.bump();
        }
    }

    fn live(&self) -> Option<Arc<LiveInventory>> {
        self.live.read().clone()
    }

    /// Stops discovery for good (cancellation or a full reconciliation) and joins its workers.
    fn stop_discovery(&self) {
        self.discovery_cancel.cancel();
        self.gate.wake_all();
        let handle = self.discovery.lock().take();
        if let Some(handle) = handle {
            if !handle.is_finished() {
                handle.cancel();
            }
            *self.report.lock() = Some(handle.join());
        }
        self.mailbox.cancel(true);
    }

    /// Discovery is abandoned while the stage must be reconciled completely: the live
    /// inventory is incomplete from now on, so reads switch to the stage.
    fn abandon_live(&self) {
        self.async_active.store(false, Ordering::Release);
        self.stop_discovery();
        *self.live.write() = None;
        self.notifier.bump();
    }
}

/// [`ReplayTarget`] over the session's stage (Swift `WALListingStage` + `DurableCrawler`).
struct ReplayStage<'a, P: Platform> {
    stage: &'a mut Stage<P>,
    shared: Option<&'a Shared<P>>,
}

impl<P: Platform> ReplayTarget for ReplayStage<'_, P> {
    fn root(&self) -> &[u8] {
        self.stage.root()
    }

    fn removal_cursor(&mut self) -> Result<Option<crate::journal::RemovalCursor>> {
        Ok(self
            .stage
            .writer_ref()
            .removal_cursor()?
            .map(|(identity, sequence)| crate::journal::RemovalCursor { identity, sequence }))
    }

    fn write_removal_cursor(&mut self, cursor: &crate::journal::RemovalCursor) -> Result<()> {
        self.stage.writer().write_removal_cursor(&cursor.identity, cursor.sequence)
    }

    fn is_fresh_for_removal_anchor(&mut self) -> Result<bool> {
        self.stage.writer_ref().is_fresh_for_removal_anchor()
    }

    fn reset_replay_parents(&mut self) -> Result<()> {
        self.stage.writer().reset_replay_parents()
    }

    fn add_replay_parents(&mut self, parents: &[Vec<u8>]) -> Result<()> {
        self.stage.writer().add_replay_parents(parents)
    }

    fn replay_parents(&mut self, after: Option<&[u8]>, limit: usize) -> Result<Vec<Vec<u8>>> {
        self.stage.writer_ref().replay_parents(after, limit)
    }

    fn mark_changed(&mut self, directory: &[u8]) -> Result<()> {
        self.stage.writer().mark_changed(directory)
    }

    fn mark_full_refresh(&mut self) -> Result<()> {
        self.stage.writer().mark_full_refresh()
    }

    fn has_replay_dirty_work(&mut self) -> Result<bool> {
        self.stage.writer_ref().has_replay_dirty_work()
    }

    fn has_pending_directories(&mut self) -> Result<bool> {
        self.stage.writer_ref().has_pending_directories()
    }

    fn has_active_async_discovery(&self) -> bool {
        self.shared.is_some_and(|s| s.async_active.load(Ordering::Acquire) && !s.mailbox.is_finished())
    }

    fn validate_root(&mut self) -> Result<()> {
        self.stage.validate_root()
    }

    fn before_full_reconciliation(&mut self) {
        if let Some(shared) = self.shared {
            shared.abandon_live();
        }
    }
}

/// Feeds the writer's local listings into the current live inventory.
struct LiveFeed {
    live: Arc<RwLock<Option<Arc<LiveInventory>>>>,
    notifier: Arc<ChangeNotifier>,
}

impl LocalObserver for LiveFeed {
    fn listed(&self, path: &[u8], identity: FileIdentity, records: &RecordBatch) {
        let live = self.live.read().clone();
        if let Some(live) = live
            && live.relist(path, identity, records)
        {
            self.notifier.bump();
        }
    }

    fn unavailable(&self, path: &[u8], identity: FileIdentity, missing: bool) {
        let live = self.live.read().clone();
        if let Some(live) = live {
            live.unavailable(path, identity, missing);
            self.notifier.bump();
        }
    }
}

// --- The session ------------------------------------------------------------------------------

/// A running mapping. Dropping it cancels mapping and waits for its threads; committed work
/// stays in the stage and the next session for the same root resumes it.
pub struct MappingSession<P: Platform> {
    shared: Arc<Shared<P>>,
    writer: Option<JoinHandle<()>>,
}

impl<P: Platform> std::fmt::Debug for MappingSession<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappingSession")
            .field("root", &String::from_utf8_lossy(&self.shared.root))
            .field("phase", &self.phase())
            .finish()
    }
}

#[allow(clippy::too_many_arguments)]
fn open_stage<P: Platform>(
    platform: &Arc<P>,
    paths: &IndexPaths,
    root: &[u8],
    root_identity: FileIdentity,
    checkpoint: &StageCheckpoint,
    cancel: &Cancellation,
    config: &StageConfig,
    busy_retry: Duration,
) -> Result<Stage<P>> {
    let deadline = Instant::now() + busy_retry;
    let mut retired = false;
    loop {
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let opened =
            Stage::open(platform.clone(), paths.clone(), root, checkpoint.clone(), cancel.clone(), config.clone());
        match opened {
            Ok(Opened::Stage(stage)) => return Ok(*stage),
            Ok(Opened::AlreadyPublished(id)) if !retired => {
                Stage::<P>::retire_published(paths, root, root_identity, &id, cancel)?;
                retired = true;
            }
            Ok(Opened::Invalidated) if !retired => {
                Stage::<P>::retire_invalidated(paths, root, root_identity, cancel)?;
                retired = true;
            }
            Ok(_) => return Err(CoreError::SnapshotChanged),
            Err(CoreError::InUse(_)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

impl<P: Platform> MappingSession<P> {
    /// Opens (or resumes) the stage for `root` and starts mapping. Fails with
    /// [`CoreError::InUse`] when another session keeps the stage for more than
    /// [`STAGE_BUSY_RETRY`].
    pub fn start(platform: Arc<P>, paths: IndexPaths, root: &[u8], options: SessionOptions) -> Result<Self> {
        let root = crate::paths::standardize(root).ok_or_else(|| CoreError::Invalid("not an absolute path".into()))?;
        let cancel = Cancellation::new();
        let checkpoint = match &options.checkpoint {
            Some(checkpoint) => checkpoint.clone(),
            None => platform.new_monitor().capture(&discovery::native_path(&root)).unwrap_or(ChangeCheckpoint {
                root: discovery::native_path(&root),
                identity: String::new(),
                event_id: 0,
            }),
        };
        let stage_checkpoint = StageCheckpoint {
            root_path: root.clone(),
            identity: checkpoint.identity.clone(),
            event_id: checkpoint.event_id,
        };
        let (_, root_identity) =
            platform.without_materialization(|| platform.open_root(&discovery::native_path(&root)))??;
        let scope = mapping_scope(&*platform, &root, root_identity);
        let mut extra = options.exclusions.clone();
        if let Some(home) = &options.home {
            extra.extend(platform.cloud_roots(home));
        }
        let exclusions = mapping_exclusions(&*platform, &paths, &scope, &extra);
        let config = stage_config(&scope, &exclusions);
        let mut stage = open_stage(
            &platform,
            &paths,
            &root,
            root_identity,
            &stage_checkpoint,
            &cancel,
            &config,
            options.busy_retry,
        )?;
        let maps = Arc::new(SavedMaps::new(platform.clone(), paths.clone()));

        // Removal history first: nothing staged is exposed before the journal was replayed.
        let journal = if options.removal_journal { Some(Arc::new(maps.journal()?)) } else { None };
        let mut replay = match &journal {
            Some(journal) => {
                let mut target = ReplayStage { stage: &mut stage, shared: None };
                let fresh = !target.stage.is_resumed();
                let clock = Arc::new(SystemClock::default());
                let mut replay = StageReplay::open(journal.clone(), &mut target, cancel.clone(), clock, fresh)?;
                replay.prepare(&mut target)?;
                Some(replay)
            }
            None => None,
        };
        let blocked = replay.as_ref().is_some_and(|r| r.view().get().blocked);

        let resumed = stage.is_resumed();
        let reused = if options.reuse_saved_maps && !blocked {
            let history = (stage.checkpoint().identity.clone(), stage.checkpoint().event_id);
            let candidates = crate::reuse::select_candidates(
                &*platform,
                &paths,
                journal.as_deref(),
                &root,
                root_identity,
                (history.0.as_str(), history.1),
                &cancel,
            );
            if let Some(oldest) = candidates.iter().map(|c| c.snapshot.event_id).min() {
                stage.lower_checkpoint(oldest)?;
            }
            candidates
        } else {
            Vec::new()
        };
        let previous_file_count = maps
            .snapshot(&root)
            .and_then(|saved| maps.store().node(&saved, &root, None).ok().flatten())
            .map(|node| node.meta.file_count);

        let notifier = Arc::new(ChangeNotifier::default());
        let gate = Arc::new(Gate::default());
        let discovery_cancel = Cancellation::new();
        let live = Arc::new(LiveInventory::new(scope));
        let slot = Arc::new(RwLock::new(Some(live.clone())));
        let mailbox = {
            let (gate, notifier_accepted, notifier_batch, live) =
                (gate.clone(), notifier.clone(), notifier.clone(), live.clone());
            Mailbox::new(
                Arc::from(root.as_slice()),
                root_identity,
                discovery_cancel.clone(),
                move || {
                    gate.signal();
                    notifier_accepted.bump();
                },
                move |batch| {
                    live.append(batch);
                    notifier_batch.bump();
                },
            )
        };
        stage.set_local_observer(Some(Box::new(LiveFeed { live: slot.clone(), notifier: notifier.clone() })));
        let shared = Arc::new(Shared {
            paths: paths.clone(),
            root: root.clone(),
            root_identity,
            cancel: cancel.clone(),
            discovery_cancel: discovery_cancel.clone(),
            maps,
            mailbox: mailbox.clone(),
            async_active: AtomicBool::new(!blocked),
            live: slot.clone(),
            discovery: Mutex::new(None),
            report: Mutex::new(None),
            resumed,
            stage_reader: stage.reader(),
            stage_preferred: stage.preferred_handle(),
            replay_view: replay.as_ref().map(StageReplay::view),
            gate: gate.clone(),
            notifier,
            state: Mutex::new(State {
                phase: Phase::Mapping,
                outcome: SessionOutcome::Running,
                estimator: DiscoveryEstimator::new(previous_file_count),
                cached_file_count: None,
                finished: false,
                checking: false,
            }),
            finished: Condvar::new(),
            clock: Instant::now(),
            reused,
        });
        if blocked {
            // Swift: discovery is not started while the replay withholds the stage.
            shared.abandon_live();
        } else {
            let stage_cache = resumed.then(|| StageListingCache::new(stage.database().to_path_buf(), root_identity));
            let generation = stage.full_generation()?;
            let cache = SessionCache::new(stage_cache, shared.reused.clone(), generation);
            let options = DiscoveryOptions {
                workers: options.workers,
                exclusions,
                preferred: options.preferred.clone(),
                cache: (!cache.is_empty()).then(|| Arc::new(cache) as Arc<dyn discovery::cache::ListingCache>),
                full_generation: generation,
                hooks: Some(Arc::new(GateHooks {
                    gate: gate.clone(),
                    cancel: discovery_cancel.clone(),
                    inner: options.hooks.clone(),
                })),
                ..DiscoveryOptions::default()
            };
            let handle = discovery::start(platform.clone(), mailbox, live, options, discovery_cancel)?;
            *shared.discovery.lock() = Some(handle);
        }
        if let Some(preferred) = &options.preferred {
            stage.prioritize(Some(preferred));
        }
        let writer_shared = shared.clone();
        let settings = WriterSettings {
            retire: options.retire_after_publish,
            prepare_search: options.prepare_search,
            before_publish: options.before_publish.clone(),
        };
        let writer = std::thread::Builder::new()
            .name("orbit-stage-writer".into())
            .spawn(move || run_writer(writer_shared, stage, replay.take(), journal, settings))
            .map_err(CoreError::Io)?;
        Ok(Self { shared, writer: Some(writer) })
    }

    pub fn root(&self) -> &[u8] {
        &self.shared.root
    }

    pub fn root_identity(&self) -> FileIdentity {
        self.shared.root_identity
    }

    /// The stage existed before: committed work was resumed.
    pub fn is_resumed(&self) -> bool {
        self.shared.resumed
    }

    /// Discovery counters (cache hits, failures, cursor statistics) so far or at its end.
    pub fn discovery_report(&self) -> Option<discovery::DiscoveryReport> {
        if let Some(handle) = self.shared.discovery.lock().as_ref() {
            return Some(handle.report());
        }
        *self.shared.report.lock()
    }

    /// Saved maps of subfolders whose listings this mapping reuses.
    pub fn reused_maps(&self) -> &[ReuseCandidate] {
        &self.shared.reused
    }

    /// List `path` (and the way to it) before other work, in discovery and in the local pass.
    pub fn prioritize(&self, path: Option<&[u8]>) {
        let path = path.filter(|p| crate::paths::is_within(p, &self.shared.root));
        if let Some(handle) = self.shared.discovery.lock().as_ref() {
            handle.prioritize(path);
        }
        *self.shared.stage_preferred.lock() = path.map(<[u8]>::to_vec);
    }

    /// Holds discovery workers (before their next folder) and the writer (before its next
    /// step). Reads keep working.
    pub fn pause(&self) {
        self.shared.gate.set(|s| s.paused = true);
        self.shared.notifier.bump();
    }

    pub fn resume(&self) {
        self.shared.gate.set(|s| s.paused = false);
        self.shared.notifier.bump();
    }

    pub fn is_paused(&self) -> bool {
        self.shared.gate.state.lock().paused
    }

    /// Stops mapping. Committed work stays in the stage for the next session.
    pub fn cancel(&self) {
        self.shared.cancel.cancel();
        self.shared.discovery_cancel.cancel();
        self.shared.gate.set(|s| s.cancelled = true);
        if let Some(handle) = self.shared.discovery.lock().as_ref() {
            handle.cancel();
        }
        self.shared.notifier.bump();
    }

    /// Change notifications for the UI (revision, waits, callback).
    pub fn changes(&self) -> &ChangeNotifier {
        &self.shared.notifier
    }

    /// Monotonic; changes whenever anything a reader could see changed.
    pub fn revision(&self) -> u64 {
        self.shared.notifier.revision()
    }

    pub fn phase(&self) -> Phase {
        self.shared.state.lock().phase
    }

    pub fn outcome(&self) -> SessionOutcome {
        self.shared.state.lock().outcome.clone()
    }

    /// Waits up to `timeout` for the session to end.
    pub fn wait(&self, timeout: Duration) -> SessionOutcome {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock();
        while matches!(state.outcome, SessionOutcome::Running) {
            if self.shared.finished.wait_until(&mut state, deadline).timed_out() {
                break;
            }
        }
        state.outcome.clone()
    }

    fn published(&self) -> Option<SnapshotInfo> {
        match &self.shared.state.lock().outcome {
            SessionOutcome::Published(snapshot) => Some(snapshot.clone()),
            _ => None,
        }
    }

    /// Discovery counters (files, records, frontier, save work) as of now.
    pub fn discovery_progress(&self) -> DiscoveryProgress {
        let mut work = self.shared.mailbox.progress();
        work.cancelled |= self.shared.cancel.is_cancelled();
        work
    }

    /// The mapping footer: estimated discovery fraction and basis, cache fraction, cached file
    /// count. Cheap: two short lock holds.
    pub fn progress(&self) -> MappingProgress {
        let mut work = self.discovery_progress();
        let async_active = self.shared.async_active.load(Ordering::Acquire);
        if !async_active {
            work.uncertain = true;
        }
        let now = self.shared.clock.elapsed().as_secs_f64();
        let mut state = self.shared.state.lock();
        if state.checking {
            work.uncertain = true;
        }
        let published = matches!(state.outcome, SessionOutcome::Published(_));
        if published {
            // Discovery's mailbox is released after publication; the map is complete.
            work.cancelled = false;
        }
        let complete = !work.cancelled && (state.finished || (work.finished && !work.uncertain));
        let estimate = state.estimator.estimate(&work, complete, now);
        let cache_fraction = if work.cancelled {
            None
        } else if state.finished {
            Some(1.0)
        } else {
            work.cache_fraction()
        };
        MappingProgress {
            revision: self.shared.notifier.revision(),
            cached_file_count: state.cached_file_count,
            discovery_fraction: estimate.fraction,
            discovery_complete: complete,
            estimate_basis: estimate.basis,
            cache_fraction,
            cache_finishing: !work.cancelled && state.finished && !published,
        }
    }

    /// The removal overlay and readiness of the stage, or `None` when the stage must not be
    /// shown yet.
    fn replay_overlay(&self) -> Option<(Arc<[ScanRemoval]>, u64)> {
        match &self.shared.replay_view {
            Some(handle) => {
                let view = handle.get();
                (!view.blocked).then_some((view.overlay, view.generation))
            }
            None => Some((Arc::from([]), 0)),
        }
    }

    fn replay_unchanged(&self, generation: u64) -> bool {
        self.shared.replay_view.as_ref().is_none_or(|h| {
            let view = h.get();
            !view.blocked && view.generation == generation
        })
    }

    fn with_overlay(removals: &[ScanRemoval], overlay: &[ScanRemoval]) -> Vec<ScanRemoval> {
        let mut all = Vec::with_capacity(removals.len() + overlay.len());
        all.extend_from_slice(removals);
        all.extend_from_slice(overlay);
        all
    }

    /// The folder at `path` from the best source right now; `None` when it is not known (yet)
    /// or the stage must not be shown while removals are reconciled.
    pub fn view(&self, path: &[u8], removals: &[ScanRemoval]) -> Result<Option<FolderTree>> {
        if !crate::paths::is_within(path, &self.shared.root) {
            return Ok(None);
        }
        if let Some(saved) = self.published() {
            return missing_as_none(self.shared.maps.view(&saved, path, removals, None));
        }
        let Some((overlay, generation)) = self.replay_overlay() else { return Ok(None) };
        let all = Self::with_overlay(removals, &overlay);
        let mut tree = None;
        if let Some(live) = self.shared.live() {
            tree = live.view(path, &all);
        }
        if tree.is_none() {
            let pending = self.shared.async_active.load(Ordering::Acquire).then(|| self.shared.mailbox.capture());
            let revision = self.shared.notifier.revision();
            tree = missing_as_none(self.shared.stage_reader.view(path, &all, pending.as_ref(), revision, None))?;
        }
        if !self.replay_unchanged(generation) {
            return Ok(None);
        }
        Ok(tree)
    }

    /// Up to `limit` matches for `query` from the best source, plus the exact count; `None`
    /// while the stage must not be shown.
    pub fn search(&self, query: &str, removals: &[ScanRemoval], limit: usize) -> Result<Option<SearchResults>> {
        if let Some(saved) = self.published() {
            return self.shared.maps.search(&saved, query, removals, limit, None).map(Some);
        }
        let Some((overlay, generation)) = self.replay_overlay() else { return Ok(None) };
        let all = Self::with_overlay(removals, &overlay);
        let needle = crate::fold::fold_query(query);
        let results = if let Some(live) = self.shared.live() {
            let (hits, total): (Vec<SearchHit>, usize) = live.search(&needle, limit, &all);
            SearchResults { hits, total }
        } else {
            let conn = crate::schema::open_reader(self.shared.stage_reader.database(), None, true)?;
            conn.execute_batch("BEGIN")?;
            let result =
                crate::query::search_inventory(&conn, &self.shared.root, &needle, limit, None, &all, true, None);
            let _ = conn.execute_batch("ROLLBACK");
            result?
        };
        Ok(self.replay_unchanged(generation).then_some(results))
    }

    /// Files discovered so far (after `removals`), from the best source.
    pub fn discovered_file_count(&self, removals: &[ScanRemoval]) -> Result<Option<u64>> {
        if let Some(saved) = self.published() {
            let tree = missing_as_none(self.shared.maps.view(&saved, &self.shared.root, removals, None))?;
            return Ok(tree.map(|t| t.node(FolderTree::FOLDER).meta.file_count));
        }
        let Some((overlay, generation)) = self.replay_overlay() else { return Ok(None) };
        let all = Self::with_overlay(removals, &overlay);
        let count = match self.shared.live() {
            Some(live) => live.file_count(&all),
            None => self.view(&self.shared.root, removals)?.map(|t| t.node(FolderTree::FOLDER).meta.file_count),
        };
        Ok(count.filter(|_| self.replay_unchanged(generation)))
    }

    /// The live inventory proves `path` is not a folder (Swift `confirmsMissingDirectory`).
    pub fn confirms_missing_directory(&self, path: &[u8]) -> bool {
        self.replay_overlay().is_some() && self.shared.live().is_some_and(|live| live.confirms_missing_directory(path))
    }
}

fn missing_as_none(result: Result<FolderTree>) -> Result<Option<FolderTree>> {
    match result {
        Ok(tree) => Ok(Some(tree)),
        Err(CoreError::Invalid(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

impl<P: Platform> Drop for MappingSession<P> {
    fn drop(&mut self) {
        if matches!(self.outcome(), SessionOutcome::Running) {
            self.cancel();
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        self.shared.stop_discovery();
    }
}

// --- The writer thread ------------------------------------------------------------------------

#[derive(Clone)]
struct WriterSettings {
    retire: bool,
    prepare_search: bool,
    before_publish: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn run_writer<P: Platform>(
    shared: Arc<Shared<P>>,
    mut stage: Stage<P>,
    mut replay: Option<StageReplay>,
    journal: Option<Arc<Journal>>,
    settings: WriterSettings,
) {
    let result = write_and_publish(&shared, &mut stage, replay.as_mut(), settings.before_publish.as_deref());
    drop(journal);
    shared.stop_discovery();
    let (phase, outcome) = match result {
        Ok(snapshot) => (Phase::Published, SessionOutcome::Published(snapshot)),
        Err(error) if error.is_cancelled() || shared.cancel.is_cancelled() => {
            (Phase::Cancelled, SessionOutcome::Cancelled)
        }
        Err(error) => (Phase::Failed, SessionOutcome::Failed(Arc::new(error))),
    };
    let published = match &outcome {
        SessionOutcome::Published(snapshot) => Some(snapshot.clone()),
        _ => None,
    };
    // Readers switch to the saved map as soon as the outcome is visible.
    {
        let mut state = shared.state.lock();
        state.phase = phase;
        state.outcome = outcome;
        if published.is_some() {
            state.finished = true;
        }
    }
    drop(stage);
    if let Some(snapshot) = &published {
        // Release the in-memory inventory, then the stage files.
        *shared.live.write() = None;
        if settings.retire {
            let _ = Stage::<P>::retire_published(
                &shared.paths,
                &shared.root,
                shared.root_identity,
                &snapshot.id,
                &Cancellation::new(),
            );
        }
    }
    shared.finished.notify_all();
    shared.notifier.bump();
    if let Some(snapshot) = published
        && settings.prepare_search
    {
        let _ = shared.maps.prepare_search(&snapshot, &shared.cancel);
    }
}

fn report_writer_progress<P: Platform>(shared: &Shared<P>, stage: &Stage<P>) {
    if let Ok(Some((_, _, files))) = stage.writer_ref().folder_totals(&shared.root) {
        shared.state.lock().cached_file_count = Some(files.max(0) as u64);
    }
}

fn write_and_publish<P: Platform>(
    shared: &Shared<P>,
    stage: &mut Stage<P>,
    mut replay: Option<&mut StageReplay>,
    before_publish: Option<&(dyn Fn() + Send + Sync)>,
) -> Result<SnapshotInfo> {
    let mut last_progress = Instant::now();
    loop {
        // Map until the stage has no work and the removal replay settled.
        loop {
            if !shared.gate.pass() || shared.cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            if let Some(replay) = replay.as_deref_mut() {
                let generation = replay.view().get().generation;
                replay.refresh_if_needed(&mut ReplayStage { stage: &mut *stage, shared: Some(shared) })?;
                if replay.view().get().generation != generation {
                    shared.state.lock().checking = true;
                    shared.notifier.bump();
                }
            }
            let mailbox = shared.async_active.load(Ordering::Acquire).then_some(&*shared.mailbox);
            let outcome = stage.step(mailbox)?;
            if shared.mailbox.is_finished() {
                let mut state = shared.state.lock();
                if state.phase == Phase::Mapping {
                    state.phase = Phase::Saving;
                }
            }
            match outcome {
                StepOutcome::Progressed { .. } => {
                    if shared.live().is_none() {
                        shared.notifier.bump();
                    }
                    if last_progress.elapsed() >= WRITER_PROGRESS_INTERVAL {
                        last_progress = Instant::now();
                        report_writer_progress(shared, stage);
                    }
                }
                StepOutcome::Idle => shared.gate.sleep(WRITER_IDLE_WAIT),
                StepOutcome::Done => {
                    let settled = match replay.as_deref_mut() {
                        Some(replay) => {
                            let generation = replay.view().get().generation;
                            let settled = replay
                                .settle_if_possible(&mut ReplayStage { stage: &mut *stage, shared: Some(shared) })?;
                            if replay.view().get().generation != generation {
                                shared.notifier.bump();
                            }
                            settled
                        }
                        None => true,
                    };
                    if settled {
                        break;
                    }
                    shared.gate.sleep(SETTLE_WAIT);
                }
            }
        }
        report_writer_progress(shared, stage);
        {
            let mut state = shared.state.lock();
            state.finished = true;
            state.checking = false;
        }
        if let Some(hook) = before_publish {
            hook();
        }
        if shared.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        shared.set_phase(Phase::Publishing);
        let elapsed = shared.clock.elapsed().as_secs_f64();
        let mut journal_changed = false;
        let published = match replay.as_deref_mut() {
            Some(replay) => {
                let replay: &StageReplay = replay;
                let mut fence = |rename: &mut dyn FnMut() -> Result<()>| -> Result<()> {
                    replay.fence_publication(|_| rename().map_err(JournalError::Target)).map_err(|error| {
                        if matches!(error, JournalError::JournalChanged) {
                            journal_changed = true;
                        }
                        CoreError::from(error)
                    })
                };
                stage.publish(elapsed, crate::publish::PublishOptions { fence: Some(&mut fence), ..Default::default() })
            }
            None => stage.publish(elapsed, crate::publish::PublishOptions::default()),
        };
        match published {
            Ok(snapshot) => return Ok(snapshot),
            Err(_) if journal_changed && !shared.cancel.is_cancelled() => {
                // A removal was recorded meanwhile: replay it and settle again (Swift
                // `publishLocal`'s retry loop).
                {
                    let mut state = shared.state.lock();
                    state.finished = false;
                    state.checking = true;
                }
                shared.set_phase(Phase::Saving);
                if let Some(replay) = replay.as_deref_mut() {
                    replay.prepare(&mut ReplayStage { stage: &mut *stage, shared: Some(shared) })?;
                }
            }
            Err(error) => return Err(error),
        }
    }
}
