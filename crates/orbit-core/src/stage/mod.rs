//! The resumable mapping stage: one private SQLite database per root that discovery results are
//! committed into while mapping runs, browsable at any time and published as the saved map when
//! complete. Ported from `ResumableStage.init`, `DurableCrawler` (`WALCrawler.swift`) and
//! `ListingStage` (`WALListingStage.swift`).
//!
//! # API
//!
//! ```text
//! Stage::open(platform, paths, root, checkpoint, cancel, config) -> Opened::{Stage, AlreadyPublished, Invalidated}
//!     takes the stage lease (CoreError::InUse when another session maps this root), checks the
//!     invalidation generation and saved-map expectation under the root mutation lease, resumes
//!     the committed stage or starts a new one.
//! stage.step(Some(&mailbox)) -> StepOutcome::{Progressed, Idle, Done}
//!     one writer transaction group: consumes mailbox batches while discovery runs, then the
//!     local pass (full-refresh marking -> prune -> trim -> link reconciliation -> next queued
//!     directory, listed through `P::Lister`). `Done` means the stage can be published.
//! stage.reader().view(path, removals, Some(&mailbox.capture()), revision, cancel) -> FolderTree
//!     capture the mailbox snapshot *before* calling; the view merges batches above the durable
//!     watermark (totals, ancestors, hard-link ownership across the boundary).
//! stage.publish(elapsed, PublishOptions) -> SnapshotInfo          (see `crate::publish`)
//! Stage::retire_published / retire_invalidated                   (delete finished stages)
//! ```
//!
//! The writer is generic over the platform (`P: Platform`) for root validation, secure
//! directory opens and listing; nothing on the hot path goes through `dyn`.

pub mod cache;
pub mod writer;

#[cfg(test)]
mod tests;

use crate::lease::{RootMutationLease, SnapshotExpectation, StageLease};
use crate::mailbox::{DirComponent, ListingBatch, Mailbox, MailboxSnapshot};
use crate::records::RecordBatch;
use crate::schema::{self, SnapshotInfo};
use crate::store::{IndexPaths, Projection, Reader, build_tree};
use crate::view::{FolderTree, ViewSource};
use crate::{CoreError, Result, ScanRemoval};
use orbit_platform::{
    Cancellation, DirectoryLister, EntryRecord, FileIdentity, Issue, ListingSink, Platform, PlatformError, Stop,
};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
pub use writer::{AppendOutcome, Completion, FreshLeaf, ListingCursor, QueuedDirectory, Traversal, Watermark, Writer};

/// The change-history position a stage was started from. Swift: `StageCheckpoint`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageCheckpoint {
    pub root_path: Vec<u8>,
    /// Opaque history identity (`ChangeCheckpoint::identity`).
    pub identity: String,
    pub event_id: u64,
}

/// Mapping policy for one stage.
#[derive(Clone, Debug)]
pub struct StageConfig {
    pub traversal: Traversal,
    /// Directories never listed nor shown (the app's own cache, locks and stages).
    pub excluded_paths: HashSet<Vec<u8>>,
    pub excluded_identities: HashSet<FileIdentity>,
    /// Records per later transaction of a wide listing (at least 128).
    pub write_chunk: usize,
    /// A new stage starts as a copy of this saved map of the root (a forced rescan): listings
    /// then write only what changed instead of every row (see [`Stage::open`]).
    pub seed: Option<crate::schema::SnapshotInfo>,
}

impl Default for StageConfig {
    fn default() -> Self {
        Self {
            traversal: Traversal::SameDevice,
            excluded_paths: HashSet::new(),
            excluded_identities: HashSet::new(),
            seed: None,
            write_chunk: writer::WRITE_CHUNK,
        }
    }
}

/// Result of [`Stage::open`].
pub enum Opened<P: Platform> {
    Stage(Box<Stage<P>>),
    /// The stage was already sealed and published (or its snapshot is the saved map): retire
    /// it with [`Stage::retire_published`].
    AlreadyPublished(String),
    /// The cache was invalidated after this stage started: retire it with
    /// [`Stage::retire_invalidated`] and start fresh.
    Invalidated,
}

impl<P: Platform> std::fmt::Debug for Opened<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stage(stage) => f.debug_tuple("Stage").field(stage).finish(),
            Self::AlreadyPublished(id) => f.debug_tuple("AlreadyPublished").field(id).finish(),
            Self::Invalidated => f.write_str("Invalidated"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    /// One transaction group committed. `completed` names a directory whose listing finished.
    Progressed { records: usize, completed: Option<Vec<u8>> },
    /// Discovery is still running but nothing is ready to write.
    Idle,
    /// No async or local work remains: the stage can be published.
    Done,
}

impl StepOutcome {
    fn progressed() -> Self {
        Self::Progressed { records: 0, completed: None }
    }
}

/// One step of the asynchronous writer keeps consuming mailbox work in a single transaction
/// group until this much time passed (or [`GROUP_RECORDS`] were written), then commits it once.
/// A crash loses at most one group; its batches are still pending and are written again.
pub const GROUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);
/// Records per transaction group.
pub const GROUP_RECORDS: usize = 131_072;

/// How long a validated folder stays trusted by the fast validation path.
const VALIDATION_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// Validated folders remembered at most (the memo is cleared when it reaches this size).
const VALIDATED_LIMIT: usize = 262_144;

/// Parent folders the fast validation path keeps open.
const PARENT_CACHE: usize = 16;

/// Descriptors the writer's validation cursor keeps open (Swift: 32).
const CURSOR_DEPTH: usize = 32;

struct CursorLevel<D> {
    name: Box<[u8]>,
    identity: FileIdentity,
    dir: D,
}

struct AsyncListing {
    path: Arc<[u8]>,
    cursor: ListingCursor,
    skipped: bool,
}

/// Runs between a mailbox commit and its acknowledgement (removal replay hooks in here).
pub type BeforeAcknowledge = Box<dyn FnMut(&mut Writer) -> Result<()> + Send>;

pub struct Stage<P: Platform> {
    platform: Arc<P>,
    paths: IndexPaths,
    root: Vec<u8>,
    root_identity: FileIdentity,
    writer: Writer,
    /// `None` for a repair stage over a private working copy of a saved map.
    lease: Option<StageLease>,
    database: PathBuf,
    checkpoint: StageCheckpoint,
    resumed: bool,
    expected: SnapshotExpectation,
    generation: String,
    cancel: Cancellation,
    config: StageConfig,
    preview: SnapshotInfo,
    preferred: Arc<Mutex<Option<Vec<u8>>>>,
    async_listing: Option<AsyncListing>,
    async_finished: Option<uuid::Uuid>,
    lister: Option<P::Lister>,
    /// Open ancestors reused by [`Stage::validate_components`] (root first).
    ancestors: Vec<CursorLevel<P::Dir>>,
    /// Whether written listings revalidate their folder's path against the filesystem (off when
    /// replayable change history covers the scan, see [`Stage::set_recheck_identities`]).
    recheck_identities: bool,
    /// Folders (path → identity) validated during the current transaction group; cleared with
    /// each group so a check is never older than one group.
    validated: std::collections::HashMap<Vec<u8>, (FileIdentity, std::time::Instant)>,
    /// Parents reopened by the fast validation path, most recent first (at most
    /// [`PARENT_CACHE`]): discovery interleaves folders from a few subtrees at a time.
    parents: Vec<(Vec<u8>, FileIdentity, P::Dir)>,
    before_acknowledge: Option<BeforeAcknowledge>,
    /// The highest mailbox sequence written into the open transaction group, acknowledged once
    /// the group commits.
    group_ack: Option<u64>,
    /// The root was validated for the open transaction group: its other steps skip the check,
    /// and the group validates again right before it commits.
    group_root_checked: bool,
    observer: Option<Box<dyn LocalObserver>>,
}

/// Receives what the writer's local pass learns from the filesystem, so an in-memory inventory
/// built by discovery stays complete while the writer re-lists folders (retries, dirty folders,
/// removal replay). Called on the writer thread; keep it cheap.
pub trait LocalObserver: Send {
    /// `path` (with `identity`) was listed completely; `records` is the full listing.
    fn listed(&self, path: &[u8], identity: FileIdentity, records: &RecordBatch);
    /// `path` could not be listed: `missing` when it vanished or was replaced, otherwise it is
    /// unreadable and stays as an inaccessible, empty folder.
    fn unavailable(&self, path: &[u8], identity: FileIdentity, missing: bool);
}

impl<P: Platform> std::fmt::Debug for Stage<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stage").field("root", &String::from_utf8_lossy(&self.root)).finish_non_exhaustive()
    }
}

pub(crate) fn path_of(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

fn private_issue(path: &[u8], message: &str) -> String {
    format!("{}: {message}", String::from_utf8_lossy(path))
}

impl<P: Platform> Stage<P> {
    /// Opens or resumes the stage for `root`. `current` is the change-history checkpoint
    /// captured before mapping starts (ignored when resuming: the stage keeps its original).
    pub fn open(
        platform: Arc<P>,
        paths: IndexPaths,
        root: &[u8],
        current: StageCheckpoint,
        cancel: Cancellation,
        config: StageConfig,
    ) -> Result<Opened<P>> {
        if current.root_path != root {
            return Err(CoreError::Invalid("the checkpoint belongs to another root".into()));
        }
        let (_, root_identity) = platform.open_root(path_of(root))?;
        crate::lease::create_private_dir(&paths.cache)?;
        crate::lease::create_private_dir(&paths.locks)?;
        crate::lease::create_private_dir(&paths.staging)?;
        let (expected, generation) = {
            let lease = RootMutationLease::acquire(&paths.locks, root, &cancel)?;
            (SnapshotExpectation::capture(&paths.cache, root)?, lease.invalidation_generation()?)
        };
        let mut lease = StageLease::acquire(&paths.staging, root, root_identity)?;
        lease.validate_paths()?;
        if let Some(seed) = config.seed.as_ref()
            && lease.is_empty()
            && expected == SnapshotExpectation::Matching(seed.id.clone())
            && Self::seed_stage(&platform, &paths, root, root_identity, seed, &expected, &cancel, &config, &mut lease)
                .is_err()
        {
            // Unseeded: the stage is still the empty file; the rescan writes every row.
            lease.remove_seed()?;
        }
        let mut writer = Writer::open(lease.database_path(), root, root_identity, config.traversal.clone())?;
        let previous = writer.scan_start()?;
        let prior_expected = writer.scan_expectation()?;
        let completed = writer.completed_snapshot()?;
        let prior_generation = writer.scan_generation()?;
        lease.validate_paths()?;
        let mutation = RootMutationLease::acquire(&paths.locks, root, &cancel)?;
        expected.verify(&paths.cache, root)?;
        if mutation.invalidation_generation()? != generation {
            return Err(CoreError::SnapshotChanged);
        }
        if previous.is_some() {
            match prior_generation {
                None => return Err(CoreError::SnapshotChanged),
                Some(prior) if prior != generation => return Ok(Opened::Invalidated),
                Some(_) => {}
            }
        }
        if let Some((id, published)) = &completed {
            if *published && expected != SnapshotExpectation::Absent {
                return Ok(Opened::AlreadyPublished(id.clone()));
            }
            if expected == SnapshotExpectation::Matching(id.clone())
                || SnapshotExpectation::descends_from_stage(id, &paths.cache, root)?
            {
                return Ok(Opened::AlreadyPublished(id.clone()));
            }
        }
        let checkpoint = match &previous {
            Some((identity, event_id)) => {
                StageCheckpoint { root_path: root.to_vec(), identity: identity.clone(), event_id: *event_id }
            }
            None => current,
        };
        if previous.is_some() && prior_expected.is_none() {
            return Err(CoreError::Invalid("missing original snapshot expectation".into()));
        }
        let mut original = prior_expected.unwrap_or_else(|| expected.clone());
        if original != expected && expected == SnapshotExpectation::Absent && previous.is_some() {
            writer.rebase_expectation(&original, &SnapshotExpectation::Absent)?;
            original = SnapshotExpectation::Absent;
        }
        if original != expected {
            return Err(CoreError::SnapshotChanged);
        }
        writer.preserve_scan_start(&checkpoint.identity, checkpoint.event_id, Some(&original), Some(&generation))?;
        writer.register_root()?;
        drop(mutation);
        lease.validate_paths()?;
        let preview = writer.preview_snapshot(&checkpoint.identity, checkpoint.event_id)?;
        let database = lease.database_path().to_path_buf();
        Ok(Opened::Stage(Box::new(Self {
            platform,
            paths,
            root: root.to_vec(),
            root_identity,
            writer,
            lease: Some(lease),
            database,
            checkpoint,
            resumed: previous.is_some(),
            expected: original,
            generation,
            cancel,
            config,
            preview,
            preferred: Arc::new(Mutex::new(None)),
            async_listing: None,
            async_finished: None,
            lister: None,
            ancestors: Vec::new(),
            recheck_identities: true,
            validated: std::collections::HashMap::new(),
            parents: Vec::new(),
            before_acknowledge: None,
            group_ack: None,
            group_root_checked: false,
            observer: None,
        })))
    }

    /// Makes the empty stage of a forced rescan a copy of the saved map `seed`: cloned (copy on
    /// write) beside the stage while the saved map is held, verified, reset to an unsealed stage
    /// whose every directory must be listed again (a new full-refresh generation), and renamed
    /// over the stage file. Unchanged folders then cost reads only (`Writer::upsert`); before,
    /// a rescan rebuilt all rows and indexes. The listing after the new checkpoint confirms or
    /// replaces every row, and folders no longer found are pruned.
    #[allow(clippy::too_many_arguments)]
    fn seed_stage(
        platform: &Arc<P>,
        paths: &IndexPaths,
        root: &[u8],
        root_identity: FileIdentity,
        seed: &crate::schema::SnapshotInfo,
        expected: &SnapshotExpectation,
        cancel: &Cancellation,
        config: &StageConfig,
        lease: &mut StageLease,
    ) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        lease.remove_seed()?;
        let copy = lease.seed_path();
        {
            let _held = RootMutationLease::acquire(&paths.locks, root, cancel)?;
            expected.verify(&paths.cache, root)?;
            platform.clone_file(&seed.database, &copy)?;
        }
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600))?;
        {
            let mut writer = Writer::open(&copy, root, root_identity, config.traversal.clone())?;
            writer.prepare_seed(&seed.id)?;
            writer.checkpoint_for_clone(std::time::Duration::from_secs(5), cancel)?;
        }
        lease.install_seed()
    }

    /// A writer over a private working copy of a saved map (see `crate::saved`), used to re-list
    /// changed folders before the copy replaces the saved map. No stage lease: the copy is a
    /// hidden file only this process knows; publication goes through the store.
    pub fn open_repair(
        platform: Arc<P>,
        paths: IndexPaths,
        copy: &SnapshotInfo,
        root_identity: FileIdentity,
        cancel: Cancellation,
        config: StageConfig,
    ) -> Result<Self> {
        let root = copy.root_path.clone();
        let writer = Writer::open(&copy.database, &root, root_identity, config.traversal.clone())?;
        if writer.directory(&root)?.map(|d| d.identity) != Some(root_identity) {
            return Err(CoreError::SnapshotChanged);
        }
        let checkpoint =
            StageCheckpoint { root_path: root.clone(), identity: copy.event_identity.clone(), event_id: copy.event_id };
        let generation = writer.scan_generation()?.unwrap_or_default();
        Ok(Self {
            platform,
            paths,
            root,
            root_identity,
            writer,
            lease: None,
            database: copy.database.clone(),
            checkpoint,
            resumed: true,
            expected: SnapshotExpectation::Matching(copy.id.clone()),
            generation,
            cancel,
            config,
            preview: copy.clone(),
            preferred: Arc::new(Mutex::new(None)),
            async_listing: None,
            async_finished: None,
            lister: None,
            ancestors: Vec::new(),
            recheck_identities: true,
            validated: std::collections::HashMap::new(),
            parents: Vec::new(),
            before_acknowledge: None,
            group_ack: None,
            group_root_checked: false,
            observer: None,
        })
    }

    /// Deletes a stage whose snapshot is published (or is the saved map's origin).
    pub fn retire_published(
        paths: &IndexPaths,
        root: &[u8],
        root_identity: FileIdentity,
        id: &str,
        cancel: &Cancellation,
    ) -> Result<()> {
        let lease = StageLease::acquire(&paths.staging, root, root_identity)?;
        let mutation = RootMutationLease::acquire(&paths.locks, root, cancel)?;
        let current = SnapshotExpectation::capture(&paths.cache, root)?;
        let completed = {
            let writer = Writer::open(lease.database_path(), root, root_identity, Traversal::SameDevice)?;
            writer.completed_snapshot()?
        };
        let Some((completed, published)) = completed else { return Err(CoreError::SnapshotChanged) };
        if completed != id
            || !(published
                || current == SnapshotExpectation::Matching(id.to_string())
                || SnapshotExpectation::descends_from_stage(id, &paths.cache, root)?)
        {
            return Err(CoreError::SnapshotChanged);
        }
        mutation.validate()?;
        lease.remove_stage_files()
    }

    /// Deletes a stage started before the last cache invalidation.
    pub fn retire_invalidated(
        paths: &IndexPaths,
        root: &[u8],
        root_identity: FileIdentity,
        cancel: &Cancellation,
    ) -> Result<()> {
        let lease = StageLease::acquire(&paths.staging, root, root_identity)?;
        let mutation = RootMutationLease::acquire(&paths.locks, root, cancel)?;
        let prior = {
            let writer = Writer::open(lease.database_path(), root, root_identity, Traversal::SameDevice)?;
            writer.scan_generation()?
        };
        match prior {
            Some(prior) if prior != mutation.invalidation_generation()? => {}
            _ => return Err(CoreError::SnapshotChanged),
        }
        mutation.validate()?;
        lease.remove_stage_files()
    }

    pub fn root(&self) -> &[u8] {
        &self.root
    }

    pub fn root_identity(&self) -> FileIdentity {
        self.root_identity
    }

    pub fn checkpoint(&self) -> &StageCheckpoint {
        &self.checkpoint
    }

    /// The stage existed before this open (committed progress was resumed).
    pub fn is_resumed(&self) -> bool {
        self.resumed
    }

    pub fn expectation(&self) -> &SnapshotExpectation {
        &self.expected
    }

    /// The cache invalidation generation this stage was started under.
    pub fn invalidation_generation(&self) -> &str {
        &self.generation
    }

    pub fn paths(&self) -> &IndexPaths {
        &self.paths
    }

    pub fn platform(&self) -> &Arc<P> {
        &self.platform
    }

    pub fn cancellation(&self) -> &Cancellation {
        &self.cancel
    }

    /// The stage lease; `None` for a repair stage.
    pub fn lease(&self) -> Option<&StageLease> {
        self.lease.as_ref()
    }

    /// The stage database file.
    pub fn database(&self) -> &Path {
        &self.database
    }

    /// Installs (or removes) the observer of the local pass.
    pub fn set_local_observer(&mut self, observer: Option<Box<dyn LocalObserver>>) {
        self.observer = observer;
    }

    /// Lowers the scan-start event floor (a reused saved map is older than this mapping). Must
    /// run before any record from that map is committed.
    pub fn lower_checkpoint(&mut self, event_id: u64) -> Result<()> {
        let identity = self.checkpoint.identity.clone();
        self.writer.lower_checkpoint(&identity, event_id)?;
        if event_id < self.checkpoint.event_id {
            self.checkpoint.event_id = event_id;
            self.preview.event_id = event_id;
        }
        Ok(())
    }

    /// The stage's partial snapshot (database path and preview id) for readers.
    pub fn preview(&self) -> &SnapshotInfo {
        &self.preview
    }

    /// The full-refresh generation discovery must stamp on its batches.
    pub fn full_generation(&self) -> Result<i64> {
        Ok(self.writer.full_refresh_state()?.0)
    }

    /// Direct access for removal replay, refresh marking and probes.
    pub fn writer(&mut self) -> &mut Writer {
        &mut self.writer
    }

    pub fn writer_ref(&self) -> &Writer {
        &self.writer
    }

    /// Whether asynchronous listings revalidate their folder's path before they are written
    /// (default on). Turn it off only when replayable change history captured before the scan
    /// reports any folder replaced meanwhile.
    pub fn set_recheck_identities(&mut self, recheck: bool) {
        self.recheck_identities = recheck;
    }

    pub fn set_before_acknowledge(&mut self, hook: Option<BeforeAcknowledge>) {
        self.before_acknowledge = hook;
    }

    /// Lists this folder (and its queued ancestors) first in the local pass.
    pub fn prioritize(&self, path: Option<&[u8]>) {
        *self.preferred.lock() = path.filter(|p| crate::paths::is_within(p, &self.root)).map(<[u8]>::to_vec);
    }

    /// The shared preferred-folder slot, so another thread can call [`Stage::prioritize`]'s
    /// equivalent while the writer owns the stage. Store only paths inside the root.
    pub fn preferred_handle(&self) -> Arc<Mutex<Option<Vec<u8>>>> {
        self.preferred.clone()
    }

    /// A cheap, cloneable handle for building views from other threads.
    pub fn reader(&self) -> StageReader {
        StageReader {
            inner: Arc::new(ReaderInner {
                database: self.database.clone(),
                root: self.root.clone(),
                root_identity: self.root_identity,
                traversal: self.config.traversal.clone(),
                after_watermark: Mutex::new(None),
            }),
        }
    }

    /// The root still resolves to the identity this stage maps.
    pub fn validate_root(&self) -> Result<()> {
        let (_, identity) = self.platform.open_root(path_of(&self.root))?;
        if identity != self.root_identity {
            return Err(PlatformError::IdentityChanged { path: path_of(&self.root).to_path_buf() }.into());
        }
        Ok(())
    }

    /// One writer step. Pass the mailbox while discovery for this stage runs.
    pub fn step(&mut self, mailbox: Option<&Mailbox>) -> Result<StepOutcome> {
        let platform = self.platform.clone();
        platform.without_materialization(|| self.step_inner(mailbox))?
    }

    fn step_inner(&mut self, mailbox: Option<&Mailbox>) -> Result<StepOutcome> {
        if self.cancel.is_cancelled() {
            if let Some(mailbox) = mailbox {
                mailbox.cancel(true);
            }
            return Err(CoreError::Cancelled);
        }
        if let Some(mailbox) = mailbox
            && self.async_finished != Some(mailbox.generation())
        {
            match self.step_async_group(mailbox) {
                Ok(Some(outcome)) => return Ok(outcome),
                Ok(None) => {
                    self.async_finished = Some(mailbox.generation());
                    self.ancestors.clear();
                }
                Err(error) => {
                    mailbox.cancel(true);
                    return Err(error);
                }
            }
        }
        self.step_local()
    }

    // --- Mailbox consumption (Swift `DurableCrawler.consumeAsync`) --------------------------------

    /// Checks that `components` still name the same directories, through a bounded cursor of
    /// open ancestors (Swift `SecureDirectoryCursor`): cached ancestors are revalidated by name
    /// and identity with [`Platform::child_identity`], new ones are opened with
    /// [`Platform::open_child`], and the listed directory itself is only `fstatat`ed.
    fn validate_components(&mut self, components: &[DirComponent]) -> Result<()> {
        if !self.recheck_identities {
            return Ok(());
        }
        if self.validated_through_parent(components) {
            return Ok(());
        }
        let result = self.validate_with_cursor(components);
        match &result {
            Ok(()) => self.remember_validated(components),
            Err(_) => {
                self.ancestors.clear();
                self.validated.clear();
                self.parents.clear();
            }
        }
        result
    }

    /// The cheap path of [`Stage::validate_components`]: the folder itself, or its parent, was
    /// validated earlier in this transaction group. Then the parent is reopened by path (or
    /// reused for a sibling) and must have exactly the validated identity, so a replaced or
    /// symlinked ancestor cannot pass; only the folder's own entry is checked inside it. Anything
    /// unexpected returns `false` and the full walk decides. Discovery interleaves folders from
    /// many subtrees, and the single-chain cursor reopened every ancestor for each of them.
    fn validated_through_parent(&mut self, components: &[DirComponent]) -> bool {
        let Some((last, ancestors)) = components.split_last() else { return false };
        let path = self.component_path(components);
        let ttl = VALIDATION_TTL;
        let fresh = |entry: Option<&(FileIdentity, std::time::Instant)>, identity: FileIdentity| {
            entry.is_some_and(|(id, at)| *id == identity && at.elapsed() < ttl)
        };
        if fresh(self.validated.get(&path), last.identity) {
            return true;
        }
        let parent_path = self.component_path(ancestors);
        let parent_identity = ancestors.last().map_or(self.root_identity, |c| c.identity);
        if !fresh(self.validated.get(&parent_path), parent_identity) {
            return false;
        }
        match self.parents.iter().position(|(p, id, _)| *p == parent_path && *id == parent_identity) {
            Some(0) => {}
            Some(index) => {
                let entry = self.parents.remove(index);
                self.parents.insert(0, entry);
            }
            None => {
                let Ok((dir, identity)) = self.platform.open_root(path_of(&parent_path)) else { return false };
                if identity != parent_identity {
                    return false;
                }
                self.parents.truncate(PARENT_CACHE - 1);
                self.parents.insert(0, (parent_path, identity, dir));
            }
        }
        let Some((_, _, dir)) = self.parents.first() else { return false };
        if !self.platform.child_identity(dir, &last.name).is_ok_and(|identity| identity == last.identity) {
            return false;
        }
        self.validated.insert(path, (last.identity, std::time::Instant::now()));
        true
    }

    /// Records the root and every component of a fully validated path.
    fn remember_validated(&mut self, components: &[DirComponent]) {
        if self.validated.len() >= VALIDATED_LIMIT {
            self.validated.clear();
        }
        let now = std::time::Instant::now();
        let mut path = self.root.clone();
        self.validated.insert(path.clone(), (self.root_identity, now));
        for component in components {
            path = crate::paths::join(&path, &component.name);
            self.validated.insert(path.clone(), (component.identity, now));
        }
    }

    fn component_path(&self, components: &[DirComponent]) -> Vec<u8> {
        components.iter().fold(self.root.clone(), |path, c| crate::paths::join(&path, &c.name))
    }

    fn validate_with_cursor(&mut self, components: &[DirComponent]) -> Result<()> {
        if self.ancestors.is_empty() {
            let (dir, identity) = self.platform.open_root(path_of(&self.root))?;
            if identity != self.root_identity {
                return Err(PlatformError::IdentityChanged { path: path_of(&self.root).to_path_buf() }.into());
            }
            self.ancestors.push(CursorLevel { name: Box::default(), identity, dir });
        }
        let Some((last, ancestors)) = components.split_last() else { return Ok(()) };
        for (index, component) in ancestors.iter().enumerate() {
            let level = index + 1;
            let cached =
                self.ancestors.get(level).is_some_and(|c| c.name == component.name && c.identity == component.identity);
            if cached {
                if self.platform.child_identity(&self.ancestors[level - 1].dir, &component.name)? != component.identity
                {
                    return Err(PlatformError::IdentityChanged { path: path_of(&component.name).to_path_buf() }.into());
                }
            } else {
                self.ancestors.truncate(level);
                let dir =
                    self.platform.open_child(&self.ancestors[level - 1].dir, &component.name, component.identity)?;
                self.ancestors.push(CursorLevel { name: component.name.clone(), identity: component.identity, dir });
            }
        }
        let parent = &self.ancestors[ancestors.len()].dir;
        if self.platform.child_identity(parent, &last.name)? != last.identity {
            return Err(PlatformError::IdentityChanged { path: path_of(&last.name).to_path_buf() }.into());
        }
        self.ancestors.truncate(CURSOR_DEPTH);
        Ok(())
    }

    fn registered(&mut self, batch: &ListingBatch) -> Result<bool> {
        Ok(self.writer.directory(&batch.parent_path)?.is_some_and(|d| d.identity == batch.parent_identity)
            && self.validate_components(&batch.components).is_ok())
    }

    fn settle(&mut self, mailbox: &Mailbox, sequence: u64) -> Result<()> {
        if let Some(hook) = self.before_acknowledge.as_mut() {
            hook(&mut self.writer)?;
        }
        while self.writer.reconcile_links_chunk(Some(&self.cancel))? {}
        if self.writer.in_group() {
            // Written, not yet durable: acknowledged after the group commits.
            mailbox.consume(sequence);
            self.group_ack = Some(self.group_ack.map_or(sequence, |s| s.max(sequence)));
        } else {
            mailbox.acknowledge(sequence);
        }
        Ok(())
    }

    /// Asynchronous writing in one transaction group: [`Stage::step_async`] repeatedly until the
    /// group's budget is spent or there is nothing ready, then one durable commit and one
    /// acknowledgement. Before, every folder (or small-folder group) was its own `FULL` commit
    /// that rewrote the same ancestor and index pages. The group never outlives this call, so
    /// work between steps (journal replay, publication) never runs inside it. On any error the
    /// whole group is discarded and its batches are offered again.
    fn step_async_group(&mut self, mailbox: &Mailbox) -> Result<Option<StepOutcome>> {
        let started = std::time::Instant::now();
        self.writer.begin_group()?;
        self.group_ack = None;
        self.group_root_checked = false;
        self.parents.clear();
        let (mut records, mut completed, mut progressed) = (0usize, None, false);
        let outcome = loop {
            match self.step_async(mailbox) {
                Ok(Some(StepOutcome::Progressed { records: written, completed: folder })) => {
                    progressed = true;
                    records += written;
                    completed = folder.or(completed);
                    if started.elapsed() >= GROUP_BUDGET || records >= GROUP_RECORDS || self.cancel.is_cancelled() {
                        break Ok(Some(StepOutcome::Progressed { records, completed }));
                    }
                }
                Ok(other) if progressed => {
                    // Nothing more ready (or async work ended): commit what the group has; the
                    // next step sees the rest (`Idle`, or `None` once the mailbox drained).
                    let _ = other;
                    break Ok(Some(StepOutcome::Progressed { records, completed }));
                }
                Ok(other) => break Ok(other),
                Err(error) => break Err(error),
            }
        };
        let committed = outcome.and_then(|outcome| {
            // Steps after the first skipped the root check: a root replaced while the group was
            // open rolls the whole group back.
            if self.group_root_checked {
                self.validate_root()?;
            }
            self.writer.commit_group()?;
            Ok(outcome)
        });
        self.group_root_checked = false;
        match committed {
            Ok(outcome) => {
                if let Some(sequence) = self.group_ack.take() {
                    mailbox.acknowledge(sequence);
                }
                Ok(outcome)
            }
            Err(error) => {
                if self.writer.in_group() {
                    self.writer.rollback_group();
                }
                self.group_ack = None;
                mailbox.rewind();
                self.async_listing = None;
                Err(error)
            }
        }
    }

    fn step_async(&mut self, mailbox: &Mailbox) -> Result<Option<StepOutcome>> {
        if mailbox.is_cancelled() && self.async_listing.is_none() && mailbox.is_drained() {
            return Ok(None);
        }
        // Inside a transaction group the root is checked once per group (and again before it
        // commits) instead of once per step: each check reopens the root, and on a whole-drive
        // map these opens took about 5% of the writer's time.
        if !(self.writer.in_group() && self.group_root_checked) {
            self.validate_root()?;
            self.group_root_checked = self.writer.in_group();
        }
        // Finish marking the old queue before accepting this generation's listings; otherwise
        // their completions are rejected and every discovered folder is walked again.
        if self.writer.mark_full_refresh_chunk()? {
            return Ok(Some(StepOutcome::progressed()));
        }
        if self.async_listing.is_none() {
            let leaves = mailbox.next_fresh_leaves();
            if leaves.len() > 1
                && let Some(outcome) = self.consume_fresh_leaves(&leaves, mailbox)?
            {
                return Ok(Some(outcome));
            }
        }
        let limit = if self.async_listing.is_none() { writer::FIRST_BATCH } else { self.config.write_chunk.max(128) };
        let pending = mailbox.next(limit);
        let (Some(first), Some(last)) = (pending.first(), pending.last()) else {
            if mailbox.is_drained() || mailbox.is_cancelled() {
                self.async_listing = None;
                return Ok(None);
            }
            return Ok(Some(StepOutcome::Idle));
        };
        if self.writer.full_refresh_state()?.0 != first.expected_full_generation {
            mailbox.cancel(true);
            return Ok(Some(StepOutcome::progressed()));
        }
        if self.async_listing.is_none() {
            let skipped = !self.registered(first)?;
            self.async_listing =
                Some(AsyncListing { path: first.parent_path.clone(), cursor: ListingCursor::default(), skipped });
        }
        let mut listing = self.async_listing.take().expect("listing set above");
        if listing.path != first.parent_path {
            mailbox.cancel(true);
            return Ok(Some(StepOutcome::progressed()));
        }
        let records: Vec<&RecordBatch> = pending.iter().map(|b| &b.records).collect();
        let count: usize = records.iter().map(|r| r.len()).sum();
        let watermark = Watermark { generation: mailbox.generation(), sequence: last.sequence };
        let ended = last.completion.is_some() || last.failure.is_some();
        let parent = first.parent_path.clone();
        let mut completed = false;
        if listing.skipped {
            self.writer.acknowledge_async(watermark)?;
        } else {
            if let Some(completion) = &last.completion
                && listing.cursor.epoch.is_none()
                && count < writer::FIRST_BATCH
            {
                completed = self.writer.complete_fresh_listing(
                    &parent,
                    &records,
                    Completion {
                        revision: first.expected_revision,
                        generation: first.expected_full_generation,
                        issue_count: completion.issue_count,
                        issues: &completion.issues,
                    },
                    Some(watermark),
                )?;
            }
            if !completed {
                let mut pruned = false;
                if count > 0 || !ended {
                    match self.writer.append(&parent, &mut listing.cursor, &records, Some(watermark))? {
                        AppendOutcome::Committed => {}
                        AppendOutcome::Rejected | AppendOutcome::PruneRequired => {
                            pruned = true;
                            listing.skipped = true;
                            self.writer.acknowledge_async(watermark)?;
                        }
                    }
                }
                if !pruned {
                    if let Some(completion) = &last.completion {
                        completed = self.writer.finish(
                            &parent,
                            &mut listing.cursor,
                            first.expected_revision,
                            Some(first.expected_full_generation),
                            completion.issue_count,
                            &completion.issues,
                            Some(watermark),
                        )?;
                    } else if last.failure.is_some() && count == 0 {
                        self.writer.acknowledge_async(watermark)?;
                    }
                }
            }
        }
        if listing.skipped || (last.completion.is_some() && !completed) || last.failure.is_some() {
            mailbox.require_reconciliation();
        }
        self.async_listing = if ended { None } else { Some(listing) };
        self.settle(mailbox, last.sequence)?;
        Ok(Some(StepOutcome::Progressed { records: count, completed: completed.then(|| parent.to_vec()) }))
    }

    fn consume_fresh_leaves(&mut self, leaves: &[Arc<ListingBatch>], mailbox: &Mailbox) -> Result<Option<StepOutcome>> {
        let mut accepted = Vec::with_capacity(leaves.len());
        for leaf in leaves {
            if self.cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            let Some(completion) = &leaf.completion else { break };
            if !self.writer.directory(&leaf.parent_path)?.is_some_and(|d| d.identity == leaf.parent_identity)
                || !self.writer.can_complete_fresh_listing(&leaf.parent_path)?
                || self.validate_components(&leaf.components).is_err()
            {
                break;
            }
            accepted.push(FreshLeaf {
                path: &leaf.parent_path,
                identity: leaf.parent_identity,
                records: &leaf.records,
                expected_revision: leaf.expected_revision,
                expected_generation: leaf.expected_full_generation,
                issue_count: completion.issue_count,
                issues: &completion.issues,
                watermark: Watermark { generation: mailbox.generation(), sequence: leaf.sequence },
            });
        }
        if accepted.len() < 2 {
            return Ok(None);
        }
        let committed = self.writer.complete_fresh_leaf_listings(&accepted, &self.cancel)?;
        if committed == 0 {
            return Ok(None);
        }
        let records = leaves[..committed].iter().map(|l| l.records.len()).sum();
        self.settle(mailbox, leaves[committed - 1].sequence)?;
        Ok(Some(StepOutcome::Progressed { records, completed: Some(leaves[committed - 1].parent_path.to_vec()) }))
    }

    // --- Local pass (Swift `DurableCrawler.stepLocal`) ---------------------------------------------

    fn step_local(&mut self) -> Result<StepOutcome> {
        self.validate_root()?;
        self.writer.prepare_queue_indexes(&self.cancel)?;
        if self.writer.mark_full_refresh_chunk()?
            || self.writer.prune_one_chunk()?
            || self.writer.trim_one_chunk()?
            || self.writer.reconcile_links_chunk(Some(&self.cancel))?
        {
            return Ok(StepOutcome::progressed());
        }
        let preferred = self.preferred.lock().clone();
        let Some(directory) = self.writer.next_directory(preferred.as_deref())? else {
            return Ok(StepOutcome::Done);
        };
        self.list_directory(directory, preferred.as_deref())
    }

    fn recover_directory(&mut self, directory: &QueuedDirectory, error: CoreError) -> Result<StepOutcome> {
        if directory.path == self.root {
            return Err(error);
        }
        enum Recovery {
            Block,
            Requeue,
        }
        let recovery = match &error {
            CoreError::Platform(PlatformError::IdentityChanged { .. }) => Some(Recovery::Requeue),
            CoreError::Platform(PlatformError::Dataless { .. }) => Some(Recovery::Block),
            CoreError::Platform(PlatformError::Os { code, .. }) => match *code {
                libc::EACCES | libc::EPERM => Some(Recovery::Block),
                libc::ENOENT | libc::ENOTDIR | libc::ELOOP => Some(Recovery::Requeue),
                _ => None,
            },
            _ => None,
        };
        if let Some(observer) = &self.observer {
            match recovery {
                Some(Recovery::Block) => observer.unavailable(&directory.path, directory.identity, false),
                Some(Recovery::Requeue) => observer.unavailable(&directory.path, directory.identity, true),
                None => {}
            }
        }
        match recovery {
            Some(Recovery::Block) => {
                // The platform error names only the component it opened; the skipped-items
                // sheet lists full paths (Swift `ScanError.filesystem(path, code)`).
                let reason = match &error {
                    CoreError::Platform(PlatformError::Os { code, .. }) => {
                        std::io::Error::from_raw_os_error(*code).to_string()
                    }
                    CoreError::Platform(PlatformError::Dataless { .. }) => {
                        "cloud content is not available offline".to_owned()
                    }
                    other => other.to_string(),
                };
                let message = private_issue(&directory.path, &reason);
                self.writer.block_directory(&directory.path, &message)?;
                Ok(StepOutcome::progressed())
            }
            Some(Recovery::Requeue) => {
                self.writer.invalidate_directory(&directory.path)?;
                Ok(StepOutcome::progressed())
            }
            None => Err(error),
        }
    }

    fn open_directory(&self, directory: &QueuedDirectory) -> Result<P::Dir> {
        let (mut dir, identity) = self.platform.open_root(path_of(&self.root))?;
        if identity != self.root_identity {
            return Err(PlatformError::IdentityChanged { path: path_of(&self.root).to_path_buf() }.into());
        }
        let Some(components) = crate::paths::relative_components(&directory.path, &self.root) else {
            return Err(CoreError::Invalid("queued directory outside the root".into()));
        };
        let mut accumulated = self.root.clone();
        for name in components {
            accumulated = crate::paths::join(&accumulated, name);
            let expected = self
                .writer
                .directory(&accumulated)?
                .ok_or_else(|| PlatformError::IdentityChanged { path: path_of(&accumulated).to_path_buf() })?;
            dir = self.platform.open_child(&dir, name, expected.identity)?;
        }
        if accumulated != directory.path {
            return Err(CoreError::Invalid("queued directory path is not canonical".into()));
        }
        Ok(dir)
    }

    fn list_directory(&mut self, directory: QueuedDirectory, preferred: Option<&[u8]>) -> Result<StepOutcome> {
        let dir = match self.open_directory(&directory) {
            Ok(dir) => dir,
            Err(error) if error.is_cancelled() => return Err(error),
            Err(error) => return self.recover_directory(&directory, error),
        };
        let path = directory.path.clone();
        let revision = self.writer.dirty_revision(&path)?;
        let generation = self.writer.full_refresh_state()?.0;
        let buffering = revision == 0
            && path != self.root
            && preferred != Some(path.as_slice())
            && self.writer.can_complete_fresh_listing(&path)?;
        let mut lister = self.lister.take().unwrap_or_else(|| self.platform.new_lister());
        let mut sink = LocalSink {
            writer: &mut self.writer,
            config: &self.config,
            preferred: &self.preferred,
            cancel: &self.cancel,
            path: &path,
            pending: RecordBatch::default(),
            cursor: ListingCursor::default(),
            buffering,
            batches: 0,
            records: 0,
            last_commit: None,
            issues: Vec::new(),
            issue_count: 0,
            error: None,
            observed: self.observer.is_some().then(RecordBatch::default),
        };
        let listed = lister.list(&dir, &path, &self.cancel, &mut sink);
        let LocalSink { pending, mut cursor, buffering, records, issues, issue_count, error, observed, .. } = sink;
        self.lister = Some(lister);
        drop(dir);
        if let Some(error) = error {
            return match error {
                SinkError::PruneRequired => Ok(StepOutcome::progressed()),
                SinkError::Core(error) if error.is_cancelled() => Err(error),
                SinkError::Core(error) => self.recover_directory(&directory, error),
            };
        }
        if let Err(error) = listed {
            if matches!(error, PlatformError::Cancelled) {
                return Err(CoreError::Cancelled);
            }
            return self.recover_directory(&directory, error.into());
        }
        self.validate_root()?;
        if let (Some(observer), Some(observed)) = (&self.observer, &observed) {
            observer.listed(&path, directory.identity, observed);
        }
        let completion = Completion { revision, generation, issue_count, issues: &issues };
        let mut total = records;
        if buffering && self.writer.complete_fresh_listing(&path, &[&pending], completion, None)? {
            total += pending.len();
            while self.writer.reconcile_links_chunk(Some(&self.cancel))? {}
            return Ok(StepOutcome::Progressed { records: total, completed: Some(path) });
        }
        if !pending.is_empty() {
            match self.writer.append(&path, &mut cursor, &[&pending], None)? {
                AppendOutcome::Committed => total += pending.len(),
                _ => return Ok(StepOutcome::progressed()),
            }
            while self.writer.reconcile_links_chunk(Some(&self.cancel))? {}
        }
        self.validate_root()?;
        let finished =
            self.writer.finish(&path, &mut cursor, revision, Some(generation), issue_count, &issues, None)?;
        Ok(StepOutcome::Progressed { records: total, completed: finished.then_some(path) })
    }
}

enum SinkError {
    PruneRequired,
    Core(CoreError),
}

/// Buffers one local listing and commits it with the stage's batching rules: the first 128
/// records immediately, then every `write_chunk` records or 50 ms; a fresh small folder is
/// held back and committed whole with its completion.
struct LocalSink<'a> {
    writer: &'a mut Writer,
    config: &'a StageConfig,
    preferred: &'a Mutex<Option<Vec<u8>>>,
    cancel: &'a Cancellation,
    path: &'a [u8],
    pending: RecordBatch,
    cursor: ListingCursor,
    buffering: bool,
    batches: usize,
    records: usize,
    last_commit: Option<std::time::Instant>,
    issues: Vec<String>,
    issue_count: u32,
    error: Option<SinkError>,
    /// Every accepted record, kept for the [`LocalObserver`].
    observed: Option<RecordBatch>,
}

impl LocalSink<'_> {
    fn commit(&mut self, limit: Option<usize>) -> std::result::Result<(), SinkError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let count = limit.map_or(self.pending.len(), |l| l.min(self.pending.len()));
        let outcome = if count == self.pending.len() {
            let outcome = self.writer.append(self.path, &mut self.cursor, &[&self.pending], None);
            self.pending.clear();
            outcome
        } else {
            let mut head = RecordBatch::with_capacity(count, 0);
            let mut tail = RecordBatch::default();
            for (index, record) in self.pending.iter().enumerate() {
                if index < count { head.push(&record) } else { tail.push(&record) }
            }
            self.pending = tail;
            self.writer.append(self.path, &mut self.cursor, &[&head], None)
        };
        match outcome.map_err(SinkError::Core)? {
            AppendOutcome::Committed => {}
            _ => return Err(SinkError::PruneRequired),
        }
        while self.writer.reconcile_links_chunk(Some(self.cancel)).map_err(SinkError::Core)? {}
        self.records += count;
        self.batches += 1;
        self.last_commit = Some(self.writer.now());
        Ok(())
    }

    fn accept(&mut self, records: &[EntryRecord<'_>]) -> std::result::Result<(), SinkError> {
        let mut child = Vec::new();
        for record in records {
            if record.is_dir() {
                writer::join_into(&mut child, self.path, record.name);
                if self.config.excluded_paths.contains(&child) || self.config.excluded_identities.contains(&record.id) {
                    continue;
                }
            }
            self.pending.push(record);
            if let Some(observed) = &mut self.observed {
                observed.push(record);
            }
        }
        if self.buffering {
            let preferred = self.preferred.lock().as_deref() == Some(self.path);
            // A short batch arrives at EOF; a full batch must be visible before the next read.
            if records.len() < writer::FIRST_BATCH && self.pending.len() < writer::FIRST_BATCH && !preferred {
                return Ok(());
            }
            self.buffering = false;
            self.commit(Some(writer::FIRST_BATCH))?;
        }
        let elapsed = self.last_commit.is_some_and(|t| self.writer.now().duration_since(t) >= writer::COMMIT_INTERVAL);
        if self.batches == 0 || self.pending.len() >= self.config.write_chunk.max(128) || elapsed {
            self.commit(None)?;
        }
        Ok(())
    }
}

impl ListingSink for LocalSink<'_> {
    fn batch(&mut self, records: &[EntryRecord<'_>]) -> std::result::Result<(), Stop> {
        if self.cancel.is_cancelled() {
            return Err(Stop);
        }
        self.accept(records).map_err(|error| {
            self.error = Some(error);
            Stop
        })
    }

    fn issue(&mut self, issue: Issue) {
        self.issue_count = self.issue_count.saturating_add(1);
        if self.issues.len() < 100 {
            self.issues.push(private_issue(&issue.path, &issue.message));
        }
    }
}

// --- Stage views -------------------------------------------------------------------------------

type ReadHook = Box<dyn Fn() + Send + Sync>;

struct ReaderInner {
    database: PathBuf,
    root: Vec<u8>,
    root_identity: FileIdentity,
    traversal: Traversal,
    after_watermark: Mutex<Option<ReadHook>>,
}

/// Builds stage views from any thread while the writer runs. Swift: `DiskIndexStore.stagedView`.
#[derive(Clone)]
pub struct StageReader {
    inner: Arc<ReaderInner>,
}

impl std::fmt::Debug for StageReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageReader").field("database", &self.inner.database).finish_non_exhaustive()
    }
}

impl StageReader {
    pub fn database(&self) -> &Path {
        &self.inner.database
    }

    /// Test hook: runs after the durable watermark was read inside the view's transaction.
    pub fn set_after_watermark(&self, hook: Option<ReadHook>) {
        *self.inner.after_watermark.lock() = hook;
    }

    /// The view of `path` from one read transaction plus the pending batches of `pending`
    /// (captured by the caller *before* this call) above the durable watermark.
    pub fn view(
        &self,
        path: &[u8],
        removals: &[ScanRemoval],
        pending: Option<&MailboxSnapshot>,
        revision: u64,
        cancel: Option<&Cancellation>,
    ) -> Result<FolderTree> {
        // One connection per view, closed afterwards: an idle open connection would block the
        // `journal_mode=DELETE` switch at publication.
        let conn = schema::open_reader(&self.inner.database, cancel, true)?;
        let result = self.view_in(&conn, path, removals, pending, revision, cancel);
        let _ = conn.execute_batch("ROLLBACK");
        result
    }

    fn view_in(
        &self,
        conn: &rusqlite::Connection,
        path: &[u8],
        removals: &[ScanRemoval],
        pending: Option<&MailboxSnapshot>,
        revision: u64,
        cancel: Option<&Cancellation>,
    ) -> Result<FolderTree> {
        conn.execute_batch("BEGIN")?;
        if let Some(snapshot) = pending
            && (snapshot.root_path.as_ref() != self.inner.root.as_slice()
                || snapshot.root_identity != self.inner.root_identity)
        {
            return Err(CoreError::Invalid("the mailbox belongs to another root".into()));
        }
        // The first read starts the snapshot every later row comes from.
        let watermark = writer::read_watermark(conn).map_err(|e| match e {
            CoreError::Sqlite(e) => schema::map_cancel(e, cancel),
            other => other,
        })?;
        if let Some(hook) = self.inner.after_watermark.lock().as_ref() {
            hook();
        }
        let overlay = match pending {
            Some(snapshot) => {
                let sequence = watermark.filter(|w| w.generation == snapshot.generation).map_or(0, |w| w.sequence);
                let mut links = conn.prepare(
                    "SELECT p.path,n.name,n.allocated,n.raw_alloc FROM nodes n JOIN dirs p ON p.id=n.parent
                     WHERE n.dev=?1 AND n.inode=?2 AND n.kind<>1",
                )?;
                let read = crate::pending::PendingRead::new(
                    snapshot,
                    sequence,
                    self.inner.traversal.clone(),
                    |identity| {
                        let rows = links
                            .query_map([schema::to_sql_u64(identity.dev), schema::to_sql_u64(identity.ino)], |row| {
                                let dir: Vec<u8> = row.get(0)?;
                                let name: Vec<u8> = row.get(1)?;
                                Ok(crate::pending::DurableLink {
                                    path: crate::paths::join(&dir, &name),
                                    allocated: row.get::<_, i64>(2)?.max(0) as u64,
                                    raw_allocated: row.get::<_, i64>(3)?.max(0) as u64,
                                })
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>();
                        rows.map_err(|e| schema::map_cancel(e, cancel))
                    },
                    cancel,
                )?;
                Some(read).filter(|r| !r.is_empty())
            }
            None => None,
        };
        let reader = Reader::new(conn, overlay.as_ref(), true, cancel);
        let projection = Projection::new(&reader, &self.inner.root, removals, cancel)?;
        let tree = build_tree(&reader, &self.inner.root, path, &projection, ViewSource::Stage, revision, cancel)?;
        if let Some(overlay) = &overlay {
            overlay.validate()?;
        }
        Ok(tree)
    }
}
