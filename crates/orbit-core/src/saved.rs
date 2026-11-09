//! Saved maps for the app: coverage of a requested folder, bounded views, search with a
//! prepared accelerator, incremental refresh after filesystem changes, and removal-journal
//! recovery. Ported from `MappingCoverage.swift`, the saved half of `DiskIndexStore.swift`
//! (search, `prepareSearch`, `update`) and the refresh/repair loop of `DiskModel`.
//!
//! ```text
//! SavedMaps::new(platform, paths)
//!   .coverage(requested)            -> Option<Coverage>  existing saved map or stage that covers
//!                                                        the folder, identity-checked
//!   .snapshot(root) / .view(..)     -> SnapshotInfo / FolderTree
//!   .search(snapshot, query, ..)    -> SearchResults      (prepared NameIndex + SQL tail)
//!   .prepare_search                 -> build the accelerator (callers choose when)
//!   .refresh(snapshot, dirs, ckpt)  -> SnapshotInfo       re-lists changed subtrees in a private
//!                                                        working copy, folds in removal-journal
//!                                                        history, publishes under the fence
//!   .recover(snapshot)              -> SavedRecovery      journal-only repair (RecoveryTarget)
//! ```
//!
//! Refresh follows Swift v0.9.15: changed directories (plus the journal's affected parents)
//! are planned with [`crate::refresh::plan_refresh`], each planned folder is re-read
//! recursively, the working copy gets a new snapshot id with the batch's change-history
//! position and the journal cursor, and replaces the saved map inside the journal fence only
//! while no newer removal was recorded (otherwise it starts over). Instead of Swift's
//! delete-and-reinsert of each subtree, the working copy is driven by the stage writer: planned
//! subtrees are marked dirty and re-listed, so unchanged listings keep their rows.

use crate::journal::{Fence, Journal, JournalError, JournalOptions, RemovalCursor};
use crate::lease::RootMutationLease;
use crate::query::SearchResults;
use crate::recovery::{RecoveryTarget, SavedRecovery, SystemClock, reconcile_saved_map};
use crate::refresh::{FileKind, IndexedNode, PathStat, RefreshFacts, plan_refresh};
use crate::schema::{self, SnapshotInfo};
use crate::search::{BUILDER_BYTE_LIMIT, PreparedIndex, PreparedIndexes, PreparedKey};
use crate::session::{mapping_exclusions, mapping_scope, stage_config};
use crate::stage::{Stage, StepOutcome};
use crate::store::{IndexPaths, Reader, Store};
use crate::view::FolderTree;
use crate::{CoreError, Result, ScanRemoval};
use orbit_platform::{Cancellation, ChangeCheckpoint, FileIdentity, Platform};
use rusqlite::OptionalExtension;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Publication attempts before a refresh gives up on a journal that keeps moving.
const REFRESH_ATTEMPTS: usize = 8;
/// A refresh that changed nothing still publishes when the saved map's change-history position
/// is older than this, so a restart never replays more than this much history.
const CHECKPOINT_REFRESH_INTERVAL: Duration = Duration::from_secs(600);
/// Head start the first folder read gets over search preparation (Swift: 250 ms).
pub const SEARCH_WARMUP_DELAY: Duration = Duration::from_millis(250);

/// Identifies a prepared search index: the database file and, for saved maps, its stamps, so a
/// replaced or rewritten file never serves an old index. Staged keys omit the stamps (the file
/// keeps growing) and rely on [`PreparedIndex::generation`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchKey {
    pub database: PathBuf,
    pub stamps: Option<(u64, u64, i64, (i64, i64))>,
}

impl PreparedKey for SearchKey {
    fn same_database(&self, other: &Self) -> bool {
        self.database == other.database
    }
}

impl SearchKey {
    /// The key of a saved map file as it is on disk now.
    pub fn saved(database: &std::path::Path) -> Option<Self> {
        let info = orbit_platform::posix::lstat(database).ok()?;
        info.is_file().then(|| Self {
            database: database.to_path_buf(),
            stamps: Some((info.identity.dev, info.identity.ino, info.size, info.mtime)),
        })
    }

    pub fn staged(database: &std::path::Path) -> Self {
        Self { database: database.to_path_buf(), stamps: None }
    }
}

/// Process-wide prepared search indexes (Swift `DiskIndexStore.searchIndexes`).
pub fn search_indexes() -> &'static PreparedIndexes<SearchKey> {
    static INDEXES: std::sync::OnceLock<PreparedIndexes<SearchKey>> = std::sync::OnceLock::new();
    INDEXES.get_or_init(PreparedIndexes::default)
}

/// Where an existing map of a requested folder comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageSource {
    /// A resumable mapping stage of an ancestor (or of the folder itself).
    Stage,
    /// A saved map of an ancestor (or of the folder itself).
    Saved,
}

/// An existing map that already covers a requested folder (Swift `MappingCoverage`).
#[derive(Clone, Debug, PartialEq)]
pub struct Coverage {
    pub root: Vec<u8>,
    pub root_identity: FileIdentity,
    pub requested_identity: FileIdentity,
    pub source: CoverageSource,
    /// The saved map, for [`CoverageSource::Saved`].
    pub snapshot: Option<SnapshotInfo>,
}

/// Saved-map services for one cache.
pub struct SavedMaps<P: Platform> {
    platform: Arc<P>,
    paths: IndexPaths,
    store: Store,
}

impl<P: Platform> std::fmt::Debug for SavedMaps<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SavedMaps").field("paths", &self.paths).finish_non_exhaustive()
    }
}

impl<P: Platform> SavedMaps<P> {
    pub fn new(platform: Arc<P>, paths: IndexPaths) -> Self {
        let store = Store::new(paths.clone());
        Self { platform, paths, store }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn paths(&self) -> &IndexPaths {
        &self.paths
    }

    /// The valid saved map whose root is exactly `root`.
    pub fn snapshot(&self, root: &[u8]) -> Option<SnapshotInfo> {
        self.store.saved_snapshot(root)
    }

    pub fn view(
        &self,
        snapshot: &SnapshotInfo,
        path: &[u8],
        removals: &[ScanRemoval],
        cancel: Option<&Cancellation>,
    ) -> Result<FolderTree> {
        self.store.folder(snapshot, path, removals, cancel)
    }

    /// Opens the removal journal (read-write: created when missing).
    pub fn journal(&self) -> Result<Journal> {
        Ok(Journal::open(
            &self.paths.journal,
            JournalOptions { access: crate::journal::Access::ReadWrite, aliases: self.platform.path_aliases() },
        )?)
    }

    // --- Search ---------------------------------------------------------------------------------

    pub fn is_search_prepared(&self, snapshot: &SnapshotInfo) -> bool {
        SearchKey::saved(&snapshot.database).is_some_and(|key| search_indexes().lookup(&key).is_some())
    }

    /// Builds the search accelerator for a saved map (Swift `prepareSearch`). Returns false when
    /// the map has no searchable rows.
    pub fn prepare_search(&self, snapshot: &SnapshotInfo, cancel: &Cancellation) -> Result<bool> {
        let key = SearchKey::saved(&snapshot.database).ok_or(CoreError::SnapshotChanged)?;
        if search_indexes().lookup(&key).is_some() {
            return Ok(true);
        }
        let _permit = search_indexes().begin(key.clone(), || schema::check(Some(cancel)))?;
        if search_indexes().lookup(&key).is_some() {
            return Ok(true);
        }
        let conn = self.store.open(snapshot, Some(cancel))?;
        conn.execute_batch("BEGIN")?;
        let index = crate::query::build_index(&conn, BUILDER_BYTE_LIMIT, Some(cancel))?;
        drop(conn);
        if SearchKey::saved(&snapshot.database).as_ref() != Some(&key) {
            return Err(CoreError::SnapshotChanged);
        }
        if index.is_empty() && index.is_complete() {
            return Ok(false);
        }
        search_indexes().insert(key, PreparedIndex { index, generation: None });
        Ok(true)
    }

    /// Up to `limit` matches for `query` in the saved map, largest first, plus the exact count of
    /// visible matches after `removals`.
    pub fn search(
        &self,
        snapshot: &SnapshotInfo,
        query: &str,
        removals: &[ScanRemoval],
        limit: usize,
        cancel: Option<&Cancellation>,
    ) -> Result<SearchResults> {
        let needle = crate::fold::fold_query(query);
        if needle.is_empty() {
            return Ok(SearchResults::default());
        }
        let key = SearchKey::saved(&snapshot.database);
        let conn = self.store.open(snapshot, cancel)?;
        conn.execute_batch("BEGIN")?;
        let prepared = key.as_ref().and_then(|k| search_indexes().lookup(k));
        // The index must describe the file this transaction reads.
        let prepared = prepared.filter(|_| SearchKey::saved(&snapshot.database) == key);
        crate::query::search_inventory(
            &conn,
            &snapshot.root_path,
            &needle,
            limit,
            prepared.as_deref(),
            removals,
            false,
            cancel,
        )
    }

    // --- Coverage -------------------------------------------------------------------------------

    /// The nearest existing map (saved or staged) whose root contains `requested` and reaches it
    /// as the same directory, walking up from the folder itself (Swift `MappingCoverage.find`).
    pub fn coverage(&self, requested: &[u8], cancel: &Cancellation) -> Result<Option<Coverage>> {
        let Some(requested) = crate::paths::standardize(requested) else { return Ok(None) };
        let platform = &*self.platform;
        let Ok((target_dir, target_identity)) = platform.open_root(&crate::discovery::native_path(&requested)) else {
            return Ok(None);
        };
        let target_mount = platform.mount_name(&target_dir).unwrap_or_default();
        drop(target_dir);
        let mut root = requested.clone();
        loop {
            schema::check(Some(cancel))?;
            let Ok((root_dir, root_identity)) = platform.open_root(&crate::discovery::native_path(&root)) else {
                return Ok(None);
            };
            drop(root_dir);
            let scope = mapping_scope(platform, &root, root_identity);
            let eligible =
                scope.allows_identity(&requested, target_identity) && scope.allows_mount(&requested, &target_mount);
            let mut found = None;
            if eligible {
                if let Some(saved) = self.saved_candidate(&root, root_identity, &requested, target_identity, cancel) {
                    found = Some((CoverageSource::Saved, Some(saved)));
                } else if self.staged_candidate(&root, root_identity, &requested, target_identity, cancel) {
                    found = Some((CoverageSource::Stage, None));
                }
            }
            schema::check(Some(cancel))?;
            if let Some((source, snapshot)) = found
                && can_reach(platform, &root, &requested, root_identity, Some(target_identity), cancel)?
            {
                return Ok(Some(Coverage {
                    root,
                    root_identity,
                    requested_identity: target_identity,
                    source,
                    snapshot,
                }));
            }
            match crate::paths::parent(&root) {
                Some(parent) => root = parent.to_vec(),
                None => return Ok(None),
            }
        }
    }

    fn saved_candidate(
        &self,
        root: &[u8],
        root_identity: FileIdentity,
        requested: &[u8],
        target: FileIdentity,
        cancel: &Cancellation,
    ) -> Option<SnapshotInfo> {
        let saved = self.store.saved_snapshot(root)?;
        let root_node = self.store.node(&saved, root, Some(cancel)).ok()??;
        let node = self.store.node(&saved, requested, Some(cancel)).ok()??;
        (root_node.meta.flags.is_dir
            && root_node.meta.identity == Some(root_identity)
            && node.meta.flags.is_dir
            && node.meta.identity == Some(target))
        .then_some(saved)
    }

    fn staged_candidate(
        &self,
        root: &[u8],
        root_identity: FileIdentity,
        requested: &[u8],
        target: FileIdentity,
        cancel: &Cancellation,
    ) -> bool {
        let database = crate::lease::stage_database_path(&self.paths.staging, root, root_identity);
        let Ok(info) = orbit_platform::posix::lstat(&database) else { return false };
        if !info.is_file() || !info.is_private(0o600) || info.size <= 0 {
            return false;
        }
        let Ok(conn) = schema::open_reader(&database, Some(cancel), false) else { return false };
        let _ = conn.busy_timeout(Duration::from_millis(50));
        let check = || -> rusqlite::Result<bool> {
            let application: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
            let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if application != schema::APPLICATION_ID || version != schema::VERSION {
                return Ok(false);
            }
            conn.execute_batch("BEGIN")?;
            type StageRoot = (i64, i64, Option<String>, Vec<u8>, bool);
            let state: Option<StageRoot> = conn
                .query_row(
                    "SELECT d.dev,d.inode,s.generation,m.root_path,EXISTS(SELECT 1 FROM sealed WHERE singleton=1)
                     FROM scan s JOIN dirs d ON d.path=s.root JOIN metadata m ON m.singleton=1 WHERE s.root=?1",
                    [root],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            let Some((dev, inode, generation, stored_root, sealed)) = state else { return Ok(false) };
            if schema::identity_from_sql(dev, inode) != root_identity
                || stored_root != root
                || generation != crate::lease::read_invalidation_generation(&self.paths.locks, root)
            {
                return Ok(false);
            }
            let node: Option<(i64, i64, i64)> = conn
                .query_row(
                    "SELECT n.kind,n.dev,n.inode FROM dirs p JOIN nodes n ON n.id=p.id WHERE p.path=?1",
                    [requested],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            Ok(match node {
                Some((kind, dev, inode)) => kind == 1 && schema::identity_from_sql(dev, inode) == target,
                None => !sealed,
            })
        };
        let result = check().unwrap_or(false);
        let _ = conn.execute_batch("ROLLBACK");
        result
    }

    // --- Refresh and recovery ------------------------------------------------------------------

    fn root_identity(&self, snapshot: &SnapshotInfo) -> Result<FileIdentity> {
        let node = self.store.node(snapshot, &snapshot.root_path, None)?.ok_or(CoreError::SnapshotChanged)?;
        let identity = node.meta.identity.ok_or(CoreError::SnapshotChanged)?;
        let (_, current) = self.platform.open_root(&crate::discovery::native_path(&snapshot.root_path))?;
        if current != identity {
            return Err(orbit_platform::PlatformError::IdentityChanged {
                path: crate::discovery::native_path(&snapshot.root_path),
            }
            .into());
        }
        Ok(identity)
    }

    fn repair_stage(
        &self,
        copy: &SnapshotInfo,
        root_identity: FileIdentity,
        cancel: &Cancellation,
    ) -> Result<Stage<P>> {
        let root = &copy.root_path;
        let scope = mapping_scope(&*self.platform, root, root_identity);
        let exclusions = mapping_exclusions(&*self.platform, &self.paths, &scope, &[]);
        let config = stage_config(&scope, &exclusions);
        Stage::open_repair(self.platform.clone(), self.paths.clone(), copy, root_identity, cancel.clone(), config)
    }

    /// Re-lists `directories` (paths reported by a change monitor; reconciled recursively) and
    /// everything the removal journal recorded since the map's cursor, then replaces the saved
    /// map. `checkpoint` is the change-history position the batch brings the map to. Fails with
    /// [`CoreError::InUse`] while a removal touching the map is still running (retry later).
    pub fn refresh(
        &self,
        snapshot: &SnapshotInfo,
        directories: &[Vec<u8>],
        checkpoint: Option<&ChangeCheckpoint>,
        cancel: &Cancellation,
    ) -> Result<SnapshotInfo> {
        let started = Instant::now();
        let journal = self.journal()?;
        let root = snapshot.root_path.clone();
        let root_identity = self.root_identity(snapshot)?;
        let (event_identity, event_id) = match checkpoint {
            Some(c) => (c.identity.clone(), c.event_id),
            None => (snapshot.event_identity.clone(), snapshot.event_id),
        };
        for _ in 0..REFRESH_ATTEMPTS {
            let stored = self
                .store
                .removal_cursor(snapshot, Some(cancel))?
                .map(|(identity, sequence)| RemovalCursor { identity, sequence });
            let plan = journal.recover_or_reset(&root, stored.as_ref(), cancel, &SystemClock::default())?;
            if plan.has_live_operation {
                return Err(CoreError::InUse("a removal in this folder is still running".into()));
            }
            let mut changed: Vec<Vec<u8>> = directories.to_vec();
            changed.extend(plan.affected_parents.iter().cloned());
            if plan.requires_full_refresh || plan.overlay_overflow {
                changed = vec![root.clone()];
            }
            let copy = self.store.recovery_working_copy(&*self.platform, snapshot, cancel)?;
            let result = (|| -> Result<SnapshotInfo> {
                let minimal = {
                    let conn = schema::open_reader(&copy.database, Some(cancel), true)?;
                    let mut facts = InventoryFacts {
                        reader: Reader::new(&conn, None, false, Some(cancel)),
                        platform: &*self.platform,
                    };
                    plan_refresh(changed.iter().map(Vec::as_slice), &root, &mut facts, cancel)?
                };
                let mut stage = self.repair_stage(&copy, root_identity, cancel)?;
                let issues = stage.writer_ref().issue_fingerprint()?;
                stage.writer().track_inventory_changes()?;
                for path in &minimal {
                    stage.writer().mark_subtree_changed(path)?;
                }
                drain(&mut stage)?;
                // Re-listing found nothing new (e.g. a file rewritten with the same size): keep the
                // saved map instead of publishing an identical one (a durable rewrite, and a new
                // search accelerator and view reload downstream). Only the change-history position
                // would advance; it lags until the next real change (replaying it is harmless),
                // but never for longer than CHECKPOINT_REFRESH_INTERVAL.
                let checkpoint_due = (event_identity.as_str(), event_id)
                    != (snapshot.event_identity.as_str(), snapshot.event_id)
                    && crate::stage::writer::unix_now() - snapshot.scanned_at
                        >= CHECKPOINT_REFRESH_INTERVAL.as_secs_f64();
                if stage.writer_ref().inventory_changes()? == 0
                    && stage.writer_ref().issue_fingerprint()? == issues
                    && stored.as_ref() == Some(&plan.cursor)
                    && !checkpoint_due
                {
                    return Ok(snapshot.clone());
                }
                let elapsed = started.elapsed().as_secs_f64();
                let cursor = (plan.cursor.identity.as_str(), plan.cursor.sequence);
                let info = stage.writer().finish_repair(&event_identity, event_id, elapsed, Some(cursor))?;
                stage.writer().checkpoint_for_clone(Duration::from_secs(5), cancel)?;
                drop(stage);
                let prepared = SnapshotInfo { database: copy.database.clone(), ..info };
                let mut fence = |rename: &mut dyn FnMut() -> Result<()>| -> Result<()> {
                    journal
                        .with_fence(cancel, |f| {
                            if !f.is_at(&plan.cursor) {
                                return Err(JournalError::ChangedDuringReconciliation);
                            }
                            f.validate()?;
                            rename().map_err(JournalError::Target)
                        })
                        .map_err(CoreError::from)
                };
                self.store.publish_recovery_copy(&prepared, snapshot, &mut fence, cancel)
            })();
            self.store.discard_recovery_copy(&copy);
            match result {
                Err(CoreError::ChangedDuringReconciliation) => continue,
                other => return other,
            }
        }
        Err(CoreError::ChangedDuringReconciliation)
    }

    /// Applies removal-journal history to the saved map (removals recorded by this or an earlier
    /// app, including Swift journals) and publishes the repair under the fence.
    /// Returns the recovery result and, when a repair was published, the new saved map.
    pub fn recover(
        &self,
        snapshot: &SnapshotInfo,
        cancel: &Cancellation,
    ) -> Result<(SavedRecovery, Option<SnapshotInfo>)> {
        let journal = self.journal()?;
        let root_identity = self.root_identity(snapshot)?;
        let copy = self.store.recovery_working_copy(&*self.platform, snapshot, cancel)?;
        // Lease before fence, like every saved-map writer.
        let lease = match RootMutationLease::acquire(&self.paths.locks, &snapshot.root_path, cancel) {
            Ok(lease) => lease,
            Err(error) => {
                self.store.discard_recovery_copy(&copy);
                return Err(error);
            }
        };
        let stage = match self.repair_stage(&copy, root_identity, cancel) {
            Ok(stage) => stage,
            Err(error) => {
                self.store.discard_recovery_copy(&copy);
                return Err(error);
            }
        };
        let mut target = SavedRepair {
            maps: self,
            original: snapshot.clone(),
            copy: copy.clone(),
            stage: Some(stage),
            lease: &lease,
            cancel: cancel.clone(),
            published: None,
        };
        let result = reconcile_saved_map(&journal, &mut target, cancel, &SystemClock::default());
        drop(target.stage.take());
        let published = target.published.take();
        self.store.discard_recovery_copy(&copy);
        Ok((result?, published))
    }
}

/// Runs the local pass of a repair stage until nothing is left.
fn drain<P: Platform>(stage: &mut Stage<P>) -> Result<()> {
    loop {
        if let StepOutcome::Done = stage.step(None)? {
            return Ok(());
        }
    }
}

/// [`RefreshFacts`] over an inventory and the live filesystem.
struct InventoryFacts<'a, P> {
    reader: Reader<'a>,
    platform: &'a P,
}

impl<P: Platform> RefreshFacts for InventoryFacts<'_, P> {
    fn indexed(&mut self, path: &[u8]) -> Result<Option<IndexedNode>> {
        Ok(self
            .reader
            .node_by_path(path)?
            .map(|row| IndexedNode { is_dir: row.meta.flags.is_dir, accessible: row.meta.flags.accessible }))
    }

    fn lstat(&mut self, path: &[u8]) -> Option<PathStat> {
        let info = orbit_platform::posix::lstat(&crate::discovery::native_path(path)).ok()?;
        let kind = if info.is_dir() {
            FileKind::Directory
        } else if info.mode & libc::S_IFMT as u32 == libc::S_IFLNK as u32 {
            FileKind::Symlink
        } else {
            FileKind::Other
        };
        Some(PathStat { kind, dev: info.identity.dev })
    }

    fn mount_path(&mut self, path: &[u8]) -> Result<Vec<u8>> {
        let (dir, _) = self.platform.open_root(&crate::discovery::native_path(path))?;
        Ok(self.platform.mount_name(&dir).map(String::into_bytes).unwrap_or_default())
    }

    fn canonicalize(&mut self, path: &[u8]) -> Vec<u8> {
        std::fs::canonicalize(crate::discovery::native_path(path))
            .map(|p| crate::discovery::path_bytes(&p))
            .unwrap_or_else(|_| path.to_vec())
    }
}

/// A saved map's private working copy as a [`RecoveryTarget`].
struct SavedRepair<'a, P: Platform> {
    maps: &'a SavedMaps<P>,
    original: SnapshotInfo,
    copy: SnapshotInfo,
    stage: Option<Stage<P>>,
    lease: &'a RootMutationLease,
    cancel: Cancellation,
    published: Option<SnapshotInfo>,
}

impl<P: Platform> SavedRepair<'_, P> {
    fn stage(&mut self) -> Result<&mut Stage<P>> {
        self.stage.as_mut().ok_or_else(|| CoreError::Invalid("the repair is finished".into()))
    }
}

impl<P: Platform> RecoveryTarget for SavedRepair<'_, P> {
    fn root(&self) -> &[u8] {
        &self.original.root_path
    }

    fn removal_cursor(&mut self) -> Result<Option<RemovalCursor>> {
        Ok(self
            .maps
            .store
            .removal_cursor(&self.original, Some(&self.cancel))?
            .map(|(identity, sequence)| RemovalCursor { identity, sequence }))
    }

    fn indexed_identities(&mut self, paths: &[Vec<u8>]) -> Result<Vec<Option<FileIdentity>>> {
        let stage = self.stage()?;
        let reader = Reader::new(stage.writer_ref().connection(), None, true, None);
        paths.iter().map(|p| Ok(reader.node_by_path(p)?.and_then(|row| row.meta.identity))).collect()
    }

    fn apply_removals(&mut self, removals: &[ScanRemoval], _cancel: &Cancellation) -> Result<()> {
        // The items are gone from disk: re-listing their parents drops exactly those rows.
        let stage = self.stage()?;
        for removal in removals {
            let indexed = Reader::new(stage.writer_ref().connection(), None, true, None)
                .node_by_path(&removal.path)?
                .and_then(|row| row.meta.identity);
            if indexed == Some(removal.identity)
                && let Some(parent) = crate::paths::parent(&removal.path)
            {
                stage.writer().mark_changed(parent)?;
            }
        }
        Ok(())
    }

    fn reconcile_directories(&mut self, directories: &[Vec<u8>], _cancel: &Cancellation) -> Result<()> {
        let stage = self.stage()?;
        for directory in directories {
            stage.writer().mark_changed(directory)?;
        }
        drain(stage)
    }

    fn require_full_refresh(&mut self, _cursor: &RemovalCursor) -> Result<()> {
        // Nothing private is kept; the caller re-maps the root.
        self.stage = None;
        Ok(())
    }

    fn publish(&mut self, cursor: &RemovalCursor, _fence: &Fence<'_>) -> Result<()> {
        let mut stage = self.stage.take().ok_or_else(|| CoreError::Invalid("the repair is finished".into()))?;
        drain(&mut stage)?;
        let (identity, event) = (self.original.event_identity.clone(), self.original.event_id);
        let info = stage.writer().finish_repair(
            &identity,
            event,
            self.original.elapsed,
            Some((cursor.identity.as_str(), cursor.sequence)),
        )?;
        stage.writer().checkpoint_for_clone(Duration::from_secs(5), &self.cancel)?;
        drop(stage);
        let prepared = SnapshotInfo { database: self.copy.database.clone(), ..info };
        // Already inside the journal fence and holding the lease: rename directly.
        let mut fence = |rename: &mut dyn FnMut() -> Result<()>| rename();
        let published = self.maps.store.publish_recovery_copy_locked(
            &prepared,
            &self.original,
            self.lease,
            &mut fence,
            &self.cancel,
        )?;
        self.published = Some(published);
        Ok(())
    }
}

/// Opens `target` component by component from `root` without following symlinks, checking
/// each step against the mapping scope (Swift `MappingCoverage.canReach`).
pub fn can_reach<P: Platform>(
    platform: &P,
    root: &[u8],
    target: &[u8],
    root_identity: FileIdentity,
    target_identity: Option<FileIdentity>,
    cancel: &Cancellation,
) -> Result<bool> {
    schema::check(Some(cancel))?;
    let Some(components) = crate::paths::relative_components(target, root) else { return Ok(false) };
    let Ok((mut dir, identity)) = platform.open_root(&crate::discovery::native_path(root)) else { return Ok(false) };
    if identity != root_identity {
        return Ok(false);
    }
    let scope = mapping_scope(platform, root, root_identity);
    let mut path = root.to_vec();
    let mut current = identity;
    for name in components {
        schema::check(Some(cancel))?;
        path = crate::paths::join(&path, name);
        let Ok(child) = platform.child_identity(&dir, name) else { return Ok(false) };
        let Ok(next) = platform.open_child(&dir, name, child) else { return Ok(false) };
        let mount = platform.mount_name(&next).unwrap_or_default();
        if !scope.allows_identity(&path, child) || !scope.allows_mount(&path, &mount) {
            return Ok(false);
        }
        dir = next;
        current = child;
    }
    if target_identity.is_some_and(|t| t != current) {
        return Ok(false);
    }
    let again = |p: &[u8]| platform.open_root(&crate::discovery::native_path(p)).ok().map(|(_, id)| id);
    Ok(again(root) == Some(root_identity) && again(target) == Some(current))
}
