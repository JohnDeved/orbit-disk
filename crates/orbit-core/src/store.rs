//! Saved maps: discovery, bounded folder views, lookups, invalidation and the private working
//! copy used by removal recovery. Ported from the read half of `DiskIndexStore.swift`.
//!
//! A saved map is a published stage database at `<cache>/<sha256(root)>.sqlite`. Readers open
//! it read-only per call; every view of one folder comes from a single read transaction.
//!
//! ```text
//! Store::new(paths)
//!   .saved_snapshot(root)                      -> Option<SnapshotInfo>
//!   .folder(&snapshot, path, removals, cancel) -> FolderTree   (all direct children, plus up to
//!                                                VIEW_DESCENDANT_BUDGET descendants over
//!                                                VIEW_DEPTH levels, 96 per folder, largest first)
//!   .node / .ancestors                         -> IndexedNode(s)
//!   .invalidate(root)                          -> bumps the durable invalidation generation and
//!                                                deletes the saved map
//!   .recovery_working_copy / publish_recovery_copy / discard_recovery_copy
//! ```

use crate::lease::{RootMutationLease, SnapshotExpectation};
use crate::pending::PendingRead;
use crate::schema::{self, SnapshotInfo, identity_from_sql};
use crate::types::ScanState;
use crate::view::{
    FolderTree, FolderTreeBuilder, NodeFlags, NodeIx, NodeMeta, VIEW_DEPTH, VIEW_DESCENDANT_BUDGET, ViewSource,
};
use crate::{CoreError, Result, ScanRemoval};
use orbit_platform::{Cancellation, EntryKind, Platform};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Descendants loaded per folder below the direct children (Swift: `min(96, budget)`).
pub const VIEW_FOLDER_LIMIT: usize = 96;

/// Where saved maps, locks and stages live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexPaths {
    /// Saved maps (`<cache>/<sha256(root)>.sqlite`); purgeable.
    pub cache: PathBuf,
    /// Parent of `.orbit-locks` (root mutation leases and invalidation markers).
    pub locks: PathBuf,
    /// Parent of `.orbit-stages` (mapping stages). Usually equal to `locks`.
    pub staging: PathBuf,
    /// Parent of the removal journal (`.orbit-removals`); see [`orbit_platform::AppDirs`].
    pub journal: PathBuf,
}

impl IndexPaths {
    /// The engine's directories for this user (Swift `IndexLocations`): saved maps in the cache
    /// directory, locks and stages under the support directory, the removal journal where the
    /// Swift app kept it. Paths are canonicalized so that discovery exclusions and journal
    /// records match however the folders are reached.
    pub fn for_platform<P: Platform>(platform: &P) -> Self {
        Self::from_dirs(&platform.app_dirs(), &platform.path_aliases())
    }

    pub fn from_dirs(dirs: &orbit_platform::AppDirs, aliases: &orbit_platform::PathAliases) -> Self {
        let support = canonical_location(&dirs.support, aliases);
        Self {
            cache: canonical_location(&dirs.cache, aliases),
            locks: support.clone(),
            staging: support,
            journal: canonical_location(&dirs.removal_journal, aliases),
        }
    }

    /// Everything below `base` (tests and probes): `cache/`, and `support/` for the rest.
    pub fn under(base: &Path) -> Self {
        let support = base.join("support");
        Self { cache: base.join("cache"), locks: support.clone(), staging: support.clone(), journal: support }
    }

    /// Directories mapping must never enter (Swift `ResumableStage` exclusions): the cache, the
    /// lock, stage and journal directories and their hidden subdirectories.
    pub fn exclusion_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![
            self.cache.clone(),
            self.locks.clone(),
            self.staging.clone(),
            self.journal.clone(),
            self.locks.join(".orbit-locks"),
            self.staging.join(".orbit-stages"),
            self.journal.join(crate::journal::DIRECTORY_NAME),
        ];
        roots.sort();
        roots.dedup();
        roots
    }
}

/// Swift `IndexLocations.canonicalCache`: resolves the longest existing ancestor (the folder
/// itself may not exist yet), re-appends the rest, and prefers the firmlinked spelling of a
/// Data-volume path when both name the same folder.
pub fn canonical_location(path: &Path, aliases: &orbit_platform::PathAliases) -> PathBuf {
    let bytes = crate::discovery::path_bytes(path);
    let Some(input) = crate::paths::standardize(&bytes) else { return path.to_path_buf() };
    if input == b"/" {
        return path.to_path_buf();
    }
    let mut current = input.as_slice();
    let mut suffix: Vec<&[u8]> = Vec::new();
    while std::fs::symlink_metadata(crate::discovery::native_path(current)).is_err() && current != b"/" {
        suffix.push(crate::paths::file_name(current));
        current = crate::paths::parent(current).unwrap_or(b"/");
    }
    let mut resolved = std::fs::canonicalize(crate::discovery::native_path(current))
        .map(|p| crate::discovery::path_bytes(&p))
        .unwrap_or_else(|_| current.to_vec());
    for component in suffix.iter().rev() {
        resolved = crate::paths::join(&resolved, component);
    }
    if let Some(data) = &aliases.data_volume
        && crate::paths::is_within(&resolved, data)
        && resolved.len() > data.len()
    {
        let native = &resolved[data.len()..];
        if same_through_ancestor(&resolved, native) {
            return crate::discovery::native_path(native);
        }
    }
    crate::discovery::native_path(&resolved)
}

fn same_through_ancestor(first: &[u8], second: &[u8]) -> bool {
    let identity = |p: &[u8]| std::fs::metadata(crate::discovery::native_path(p)).ok().map(|m| file_identity(&m));
    let (mut a, mut b) = (first, second);
    while b != b"/" {
        if let (Some(x), Some(y)) = (identity(a), identity(b))
            && x == y
        {
            return true;
        }
        a = crate::paths::parent(a).unwrap_or(b"/");
        b = crate::paths::parent(b).unwrap_or(b"/");
    }
    false
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// Deletes the cache directories of earlier builds (`AppDirs::obsolete_caches`; on macOS the
/// Swift `Indexes` folder, whose format-5 maps this build cannot read). Skips anything that is
/// not a real directory owned by this user, and anything that contains or lies inside a
/// directory holding durable data (`support`, the removal journal, the current cache). Returns
/// what was removed.
pub fn discard_obsolete_caches(dirs: &orbit_platform::AppDirs) -> Vec<PathBuf> {
    let durable = [&dirs.support, &dirs.removal_journal, &dirs.cache, &dirs.settings];
    let mut removed = Vec::new();
    for obsolete in &dirs.obsolete_caches {
        let obsolete_bytes = crate::discovery::path_bytes(obsolete);
        let overlaps = durable.iter().any(|keep| {
            let keep = crate::discovery::path_bytes(keep);
            crate::paths::is_within(&keep, &obsolete_bytes) || crate::paths::is_within(&obsolete_bytes, &keep)
        });
        if overlaps || obsolete_bytes.len() < 2 {
            continue;
        }
        #[cfg(unix)]
        {
            let Ok(Some(info)) = orbit_platform::posix::optional(orbit_platform::posix::lstat(obsolete)) else {
                continue;
            };
            if !info.is_dir() || info.uid != orbit_platform::posix::euid() {
                continue;
            }
        }
        if std::fs::remove_dir_all(obsolete).is_ok() {
            removed.push(obsolete.clone());
        }
    }
    removed
}

/// Swift `DiskIndexStore.databaseURL(for:)`.
pub fn saved_map_path(cache: &Path, root: &[u8]) -> PathBuf {
    cache.join(format!("{}.sqlite", schema::root_digest(root)))
}

/// Reads the snapshot of a saved map if it is a valid inventory of `root` with a root node.
pub(crate) fn read_saved_snapshot(file: &Path, root: &[u8], wait_for_locks: bool) -> Option<SnapshotInfo> {
    std::fs::symlink_metadata(file).ok()?;
    let conn = schema::open_reader(file, None, wait_for_locks).ok()?;
    let snapshot = schema::read_snapshot(&conn, file).ok()?;
    if snapshot.root_path != root {
        return None;
    }
    let reader = Reader::new(&conn, None, false, None);
    let node = reader.node_by_path(root).ok()??;
    node.meta.flags.is_dir.then_some(snapshot)
}

/// One indexed item with its full path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedNode {
    pub path: Vec<u8>,
    pub name: Vec<u8>,
    pub meta: NodeMeta,
}

/// A row read from `nodes` (or synthesized from a pending batch, then `id == 0`).
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub id: i64,
    pub name: Vec<u8>,
    pub meta: NodeMeta,
}

/// Row columns for stage views (with queue state) and saved maps (always complete).
pub(crate) fn columns(stage: bool) -> &'static str {
    if stage {
        "n.id,n.name,n.kind,n.accessible,n.is_mount,n.size,n.allocated,n.file_count,n.raw_alloc,n.dev,n.inode,
         (SELECT pending FROM dirs WHERE id=n.id),(SELECT epoch FROM dirs WHERE id=n.id),(SELECT pending FROM dirs WHERE id=n.parent)"
    } else {
        "n.id,n.name,n.kind,n.accessible,n.is_mount,n.size,n.allocated,n.file_count,n.raw_alloc,n.dev,n.inode,NULL,NULL,NULL"
    }
}

pub(crate) fn read_row(row: &rusqlite::Row<'_>, stage: bool) -> rusqlite::Result<Row> {
    let kind: i64 = row.get(2)?;
    let is_dir = kind == EntryKind::Directory as i64;
    let (size, allocated, files, raw): (i64, i64, i64, i64) = (row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?);
    if size < 0 || allocated < 0 || files < 0 || raw < 0 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(5, size.min(allocated).min(files).min(raw)));
    }
    let state = if !stage {
        ScanState::Complete
    } else if is_dir {
        match (row.get::<_, Option<i64>>(11)?, row.get::<_, Option<i64>>(12)?) {
            (Some(1), Some(epoch)) if epoch > 0 => ScanState::Scanning,
            (Some(1), _) => ScanState::Pending,
            _ => ScanState::Complete,
        }
    } else if row.get::<_, Option<i64>>(13)? == Some(1) {
        ScanState::Pending
    } else {
        ScanState::Complete
    };
    Ok(Row {
        id: row.get(0)?,
        name: row.get(1)?,
        meta: NodeMeta {
            flags: NodeFlags {
                is_dir,
                is_symlink: kind == EntryKind::Symlink as i64,
                is_mount_point: row.get::<_, i64>(4)? != 0,
                accessible: row.get::<_, i64>(3)? != 0,
            },
            size: size as u64,
            allocated: allocated as u64,
            raw_allocated: raw as u64,
            file_count: files as u64,
            identity: Some(identity_from_sql(row.get(9)?, row.get(10)?)),
            state,
        },
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Delta {
    pub size: i64,
    pub allocated: i64,
    pub count: i64,
}

pub(crate) fn apply_delta(meta: &mut NodeMeta, delta: Delta, sign: i64) {
    let add = |value: u64, delta: i64| (value as i64).saturating_add(sign * delta).max(0) as u64;
    meta.size = add(meta.size, delta.size);
    meta.allocated = add(meta.allocated, delta.allocated);
    meta.file_count = add(meta.file_count, delta.count);
}

/// Reads rows from one connection, optionally merging unacknowledged mailbox batches.
pub(crate) struct Reader<'a> {
    conn: &'a Connection,
    overlay: Option<&'a PendingRead<'a>>,
    stage: bool,
    cancel: Option<&'a Cancellation>,
}

impl<'a> Reader<'a> {
    pub fn new(
        conn: &'a Connection,
        overlay: Option<&'a PendingRead<'a>>,
        stage: bool,
        cancel: Option<&'a Cancellation>,
    ) -> Self {
        Self { conn, overlay, stage, cancel }
    }

    fn map<T>(&self, result: rusqlite::Result<T>) -> Result<T> {
        result.map_err(|e| schema::map_cancel(e, self.cancel))
    }

    /// The durable row at `path` (directory rows by path, others by parent path + name).
    pub fn durable_by_path(&self, path: &[u8]) -> Result<Option<Row>> {
        let (parent, name) = match crate::paths::parent(path) {
            Some(parent) => (parent, crate::paths::file_name(path)),
            None => (&b""[..], &b""[..]),
        };
        let columns = columns(self.stage);
        let sql = format!(
            "SELECT {columns} FROM nodes n WHERE n.id=COALESCE((SELECT id FROM dirs WHERE path=?1),
             (SELECT id FROM nodes WHERE parent=(SELECT id FROM dirs WHERE path=?2) AND name=?3))"
        );
        let stage = self.stage;
        let result = self
            .conn
            .prepare_cached(&sql)
            .and_then(|mut s| s.query_row(params![path, parent, name], |row| read_row(row, stage)).optional());
        self.map(result)
    }

    /// The row at `path`, with pending batches applied.
    pub fn node_by_path(&self, path: &[u8]) -> Result<Option<Row>> {
        let durable = self.durable_by_path(path)?;
        Ok(match self.overlay {
            Some(overlay) => overlay.node(path, durable),
            None => durable,
        })
    }

    /// Children of the folder at `path` (durable id when known), largest first, at most `limit`.
    pub fn children(&self, id: i64, path: &[u8], limit: Option<usize>) -> Result<Vec<Row>> {
        schema::check(self.cancel)?;
        let changes_order = self.overlay.is_some_and(|o| o.changes_children_order(path));
        let durable_limit = if changes_order { None } else { limit };
        let durable = if id > 0 {
            let columns = columns(self.stage);
            let sql = format!("SELECT {columns} FROM nodes n WHERE n.parent=?1 ORDER BY n.size DESC,n.name LIMIT ?2");
            let stage = self.stage;
            let limit = durable_limit.map_or(-1, |l| l as i64);
            let result = self.conn.prepare_cached(&sql).and_then(|mut s| {
                s.query_map(params![id, limit], |row| read_row(row, stage))?.collect::<rusqlite::Result<Vec<_>>>()
            });
            self.map(result)?
        } else {
            Vec::new()
        };
        match self.overlay {
            Some(overlay) => overlay.children(path, durable, limit, self.cancel),
            None => Ok(durable),
        }
    }

    pub fn count_children(&self, id: i64) -> Result<u64> {
        if id <= 0 {
            return Ok(0);
        }
        let result = self
            .conn
            .prepare_cached("SELECT COUNT(*) FROM nodes WHERE parent=?1")
            .and_then(|mut s| s.query_row([id], |row| row.get::<_, i64>(0)));
        Ok(self.map(result)? as u64)
    }
}

/// Identity-bound removals hidden from a view, with the totals they subtract from ancestors.
#[derive(Debug, Default)]
pub(crate) struct Projection {
    hidden: HashSet<Vec<u8>>,
    deltas: HashMap<Vec<u8>, Delta>,
}

impl Projection {
    /// Hides each removal whose path still has the removed identity. Ported from
    /// `DiskRemovalProjection` (TODO(integration): share with `crate::projection`).
    pub fn new(
        reader: &Reader<'_>,
        root: &[u8],
        removals: &[ScanRemoval],
        cancel: Option<&Cancellation>,
    ) -> Result<Self> {
        let mut projection = Self::default();
        let mut sources = Vec::new();
        for (index, removal) in removals.iter().enumerate() {
            if index & 255 == 0 {
                schema::check(cancel)?;
            }
            if !crate::paths::is_within(&removal.path, root) || projection.hidden.contains(&removal.path) {
                continue;
            }
            if let Some(row) = reader.node_by_path(&removal.path)?
                && row.meta.identity == Some(removal.identity)
            {
                projection.hidden.insert(removal.path.clone());
                sources.push((removal.path.clone(), row.meta));
            }
        }
        for (path, meta) in sources {
            let mut parents = Vec::new();
            let mut current = path.as_slice();
            let mut covered = false;
            while current != root {
                let Some(parent) = crate::paths::parent(current) else { break };
                if projection.hidden.contains(parent) {
                    covered = true;
                    break;
                }
                parents.push(parent.to_vec());
                current = parent;
            }
            if covered {
                continue;
            }
            for parent in parents {
                let delta = projection.deltas.entry(parent).or_default();
                delta.size += meta.size as i64;
                delta.allocated += meta.allocated as i64;
                delta.count += meta.file_count as i64;
            }
        }
        Ok(projection)
    }

    /// No removal applies to this inventory.
    pub fn is_empty(&self) -> bool {
        self.hidden.is_empty()
    }

    /// `path` or one of its ancestors is hidden.
    pub fn hides(&self, path: &[u8], root: &[u8]) -> bool {
        if self.hidden.is_empty() {
            return false;
        }
        let mut current = path;
        loop {
            if self.hidden.contains(current) {
                return true;
            }
            if current == root {
                return false;
            }
            match crate::paths::parent(current) {
                Some(parent) => current = parent,
                None => return false,
            }
        }
    }

    pub fn apply(&self, path: &[u8], meta: &mut NodeMeta) {
        if let Some(delta) = self.deltas.get(path) {
            apply_delta(meta, *delta, -1);
        }
    }
}

/// Builds the bounded view of `folder` from one read transaction.
pub(crate) fn build_tree(
    reader: &Reader<'_>,
    root: &[u8],
    folder: &[u8],
    projection: &Projection,
    source: ViewSource,
    revision: u64,
    cancel: Option<&Cancellation>,
) -> Result<FolderTree> {
    let missing = || CoreError::Invalid("This folder is no longer in the index.".into());
    if !crate::paths::is_within(folder, root) || projection.hides(folder, root) {
        return Err(missing());
    }
    let mut folder_row = reader.node_by_path(folder)?.ok_or_else(missing)?;
    if !folder_row.meta.flags.is_dir {
        return Err(missing());
    }
    projection.apply(folder, &mut folder_row.meta);
    let mut ancestors = Vec::new();
    let mut current = folder;
    while current != root {
        let parent = crate::paths::parent(current).ok_or_else(missing)?;
        let mut row = reader.node_by_path(parent)?.ok_or_else(missing)?;
        projection.apply(parent, &mut row.meta);
        ancestors.push((parent, row));
        current = parent;
    }
    let folder_name: &[u8] = if folder == root { &folder_row.name } else { crate::paths::file_name(folder) };
    let is_listing = folder_row.meta.state != ScanState::Complete;
    let mut builder = FolderTreeBuilder::new(Arc::from(root), folder.to_vec(), folder_name, folder_row.meta);
    for (path, row) in ancestors.iter().rev() {
        let name: &[u8] = if *path == root { &row.name } else { crate::paths::file_name(path) };
        builder.push_ancestor(name, row.meta);
    }
    let direct = reader.children(folder_row.id, folder, None)?;
    let mut budget = VIEW_DESCENDANT_BUDGET;
    let mut path = folder.to_vec();
    let mut loaded = 0;
    for child in direct {
        loaded += 1;
        if loaded & 255 == 0 {
            schema::check(cancel)?;
        }
        add_child(reader, &mut builder, FolderTree::FOLDER, &mut path, child, 1, &mut budget, projection, root)?;
    }
    builder.set_total_children(FolderTree::FOLDER, loaded as u32);
    schema::check(cancel)?;
    Ok(builder.finish(revision, is_listing, source))
}

#[allow(clippy::too_many_arguments)]
fn add_child(
    reader: &Reader<'_>,
    builder: &mut FolderTreeBuilder,
    parent: NodeIx,
    path: &mut Vec<u8>,
    mut row: Row,
    depth: u32,
    budget: &mut usize,
    projection: &Projection,
    root: &[u8],
) -> Result<()> {
    let length = path.len();
    if path.last() != Some(&b'/') {
        path.push(b'/');
    }
    path.extend_from_slice(&row.name);
    let result = (|| {
        if projection.hides(path, root) {
            return Ok(());
        }
        projection.apply(path, &mut row.meta);
        let ix = builder.push_child(parent, &row.name, row.meta);
        if !row.meta.flags.is_dir || depth >= VIEW_DEPTH || *budget == 0 {
            if row.meta.flags.is_dir {
                builder.set_total_children(ix, reader.count_children(row.id)? as u32);
            }
            return Ok(());
        }
        let limit = VIEW_FOLDER_LIMIT.min(*budget);
        let children = reader.children(row.id, path, Some(limit))?;
        *budget -= children.len();
        let total = if children.len() == limit { reader.count_children(row.id)? as usize } else { children.len() };
        builder.set_total_children(ix, total.max(children.len()) as u32);
        for child in children {
            add_child(reader, builder, ix, path, child, depth + 1, budget, projection, root)?;
        }
        Ok(())
    })();
    path.truncate(length);
    result
}

fn to_indexed(path: &[u8], root: &[u8], row: Row) -> IndexedNode {
    let name = if path == root { row.name } else { crate::paths::file_name(path).to_vec() };
    IndexedNode { path: path.to_vec(), name, meta: row.meta }
}

/// Saved-map reader and mutator for one cache.
#[derive(Clone, Debug)]
pub struct Store {
    paths: IndexPaths,
}

impl Store {
    pub fn new(paths: IndexPaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &IndexPaths {
        &self.paths
    }

    pub fn database_path(&self, root: &[u8]) -> PathBuf {
        saved_map_path(&self.paths.cache, root)
    }

    /// The saved map for `root`, if a valid one exists.
    pub fn saved_snapshot(&self, root: &[u8]) -> Option<SnapshotInfo> {
        read_saved_snapshot(&self.database_path(root), root, true)
    }

    pub(crate) fn open(&self, snapshot: &SnapshotInfo, cancel: Option<&Cancellation>) -> Result<Connection> {
        schema::check(cancel)?;
        let conn = schema::open_reader(&snapshot.database, cancel, true)?;
        let current = schema::read_snapshot(&conn, &snapshot.database)?;
        if current.id != snapshot.id || current.root_path != snapshot.root_path {
            return Err(CoreError::SnapshotChanged);
        }
        Ok(conn)
    }

    /// The folder view: the folder, its ancestors, all direct children and a bounded preview.
    pub fn folder(
        &self,
        snapshot: &SnapshotInfo,
        path: &[u8],
        removals: &[ScanRemoval],
        cancel: Option<&Cancellation>,
    ) -> Result<FolderTree> {
        let conn = self.open(snapshot, cancel)?;
        conn.execute_batch("BEGIN")?;
        let reader = Reader::new(&conn, None, false, cancel);
        let projection = Projection::new(&reader, &snapshot.root_path, removals, cancel)?;
        build_tree(&reader, &snapshot.root_path, path, &projection, ViewSource::Saved, 0, cancel)
    }

    pub fn node(
        &self,
        snapshot: &SnapshotInfo,
        path: &[u8],
        cancel: Option<&Cancellation>,
    ) -> Result<Option<IndexedNode>> {
        let conn = self.open(snapshot, cancel)?;
        let reader = Reader::new(&conn, None, false, cancel);
        Ok(reader.node_by_path(path)?.map(|row| to_indexed(path, &snapshot.root_path, row)))
    }

    /// Indexed nodes from the root down to `path` (empty when `path` is outside the root).
    pub fn ancestors(
        &self,
        snapshot: &SnapshotInfo,
        path: &[u8],
        cancel: Option<&Cancellation>,
    ) -> Result<Vec<IndexedNode>> {
        let conn = self.open(snapshot, cancel)?;
        conn.execute_batch("BEGIN")?;
        let root = snapshot.root_path.as_slice();
        if !crate::paths::is_within(path, root) {
            return Ok(Vec::new());
        }
        let reader = Reader::new(&conn, None, false, cancel);
        let mut result = Vec::new();
        let mut current = path;
        while let Some(row) = reader.node_by_path(current)? {
            result.push(to_indexed(current, root, row));
            if current == root {
                break;
            }
            match crate::paths::parent(current) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        result.reverse();
        Ok(result)
    }

    /// The removal-journal cursor stored in a saved map.
    pub fn removal_cursor(
        &self,
        snapshot: &SnapshotInfo,
        cancel: Option<&Cancellation>,
    ) -> Result<Option<(String, i64)>> {
        let conn = self.open(snapshot, cancel)?;
        crate::stage::writer::read_removal_cursor(&conn)
    }

    /// Captures which saved map a new stage will replace (under the root mutation lease).
    pub fn capture_expectation(&self, root: &[u8], cancel: &Cancellation) -> Result<SnapshotExpectation> {
        crate::lease::create_private_dir(&self.paths.cache)?;
        crate::lease::create_private_dir(&self.paths.locks)?;
        let _lease = RootMutationLease::acquire(&self.paths.locks, root, cancel)?;
        SnapshotExpectation::capture(&self.paths.cache, root)
    }

    /// Makes every existing stage for `root` unpublishable and deletes the saved map.
    pub fn invalidate(&self, root: &[u8], cancel: &Cancellation) -> Result<()> {
        crate::lease::create_private_dir(&self.paths.locks)?;
        let lease = RootMutationLease::acquire(&self.paths.locks, root, cancel)?;
        lease.validate()?;
        lease.invalidate_generation()?;
        match std::fs::symlink_metadata(&self.paths.cache) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(info) if !info.is_dir() => {
                return Err(CoreError::Invalid("The index directory is not a safe folder.".into()));
            }
            Ok(_) => {}
        }
        match std::fs::remove_file(self.database_path(root)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    /// Clones the saved map into a hidden private file for an incremental repair. The original
    /// stays published until [`Store::publish_recovery_copy`].
    pub fn recovery_working_copy<P: Platform>(
        &self,
        platform: &P,
        snapshot: &SnapshotInfo,
        cancel: &Cancellation,
    ) -> Result<SnapshotInfo> {
        if snapshot.database != self.database_path(&snapshot.root_path) {
            return Err(CoreError::SnapshotChanged);
        }
        let lease = RootMutationLease::acquire(&self.paths.locks, &snapshot.root_path, cancel)?;
        drop(self.open(snapshot, Some(cancel))?);
        let temporary = self.paths.cache.join(format!(".{}.sqlite", uuid::Uuid::new_v4().hyphenated()));
        platform.clone_file(&snapshot.database, &temporary)?;
        let copy = SnapshotInfo { database: temporary.clone(), ..snapshot.clone() };
        let checked = (|| {
            drop(self.open(&copy, Some(cancel))?);
            lease.validate()
        })();
        match checked {
            Ok(()) => Ok(copy),
            Err(error) => {
                self.discard_recovery_copy(&copy);
                Err(error)
            }
        }
    }

    /// Atomically replaces `original` with the repaired `copy`. `fence` runs the rename inside
    /// the removal journal's fence (it must call the closure it is given exactly once).
    pub fn publish_recovery_copy(
        &self,
        copy: &SnapshotInfo,
        original: &SnapshotInfo,
        fence: crate::publish::Fence<'_>,
        cancel: &Cancellation,
    ) -> Result<SnapshotInfo> {
        let lease = RootMutationLease::acquire(&self.paths.locks, &original.root_path, cancel)?;
        self.publish_recovery_copy_locked(copy, original, &lease, fence, cancel)
    }

    /// [`Store::publish_recovery_copy`] for a caller that already holds the root's mutation
    /// lease (taken before the journal fence, the order every saved-map writer uses).
    pub fn publish_recovery_copy_locked(
        &self,
        copy: &SnapshotInfo,
        original: &SnapshotInfo,
        lease: &RootMutationLease,
        fence: crate::publish::Fence<'_>,
        cancel: &Cancellation,
    ) -> Result<SnapshotInfo> {
        let valid_name = copy
            .database
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.') && n.ends_with(".sqlite"));
        if original.database != self.database_path(&original.root_path)
            || copy.root_path != original.root_path
            || copy.database.parent() != Some(self.paths.cache.as_path())
            || !valid_name
        {
            return Err(CoreError::Invalid("The recovery copy is invalid.".into()));
        }
        lease.validate()?;
        drop(self.open(original, Some(cancel))?);
        let conn = schema::open_reader(&copy.database, Some(cancel), true)?;
        let prepared = schema::read_snapshot(&conn, &copy.database)?;
        drop(conn);
        if prepared.root_path != original.root_path {
            return Err(CoreError::Invalid("The recovery copy is invalid.".into()));
        }
        let mut rename = || -> Result<()> {
            lease.validate()?;
            if cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            std::fs::rename(&copy.database, &original.database)?;
            Ok(())
        };
        fence(&mut rename)?;
        Ok(SnapshotInfo { database: original.database.clone(), ..prepared })
    }

    pub fn discard_recovery_copy(&self, copy: &SnapshotInfo) {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let mut path = copy.database.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }
}
