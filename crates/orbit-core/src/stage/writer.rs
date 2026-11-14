//! The single SQLite writer of a mapping stage. Ported from `ListingStage`
//! (`WALListingStage.swift`).
//!
//! Every public mutation is one `BEGIN IMMEDIATE` transaction (or part of a caller-owned group,
//! see [`Writer::complete_fresh_leaf_listings`]) and, when the records came from the mailbox,
//! stores the mailbox watermark (`orbit_async_progress`) in the same transaction. Statements are
//! `prepare_cached` and fully reset before `COMMIT`, so the writer's own reads never hold back
//! the WAL autocheckpoint.
//!
//! Invariants (see `TECHNICAL-NOTES.md`):
//! - A listing mints its epoch in its first batch transaction (or in its completion transaction
//!   when empty). Rows of older epochs are removed by `trim` once the listing completes.
//! - Completions carry the `dirty.revision` and full-refresh generation the listing started
//!   against; a mismatch rejects the completion so the directory stays pending.
//! - A directory replaced by another identity (or a file, or a mount) is pruned before the new
//!   listing may write under its path. Pruning deletes at most 128 rows per transaction,
//!   deepest parent paths first, so surviving rows keep their path across interruption.
//! - Fresh single-link files are charged at insert when no other row has their identity; every
//!   other case goes through `dirty_links` and [`Writer::reconcile_links_chunk`], which picks
//!   the owner by [`crate::hardlinks::precedes`].

use crate::hardlinks as link_order;
use crate::records::RecordBatch;
use crate::schema::{self, from_sql_u64, identity_from_sql, to_sql_size, to_sql_u64};
use crate::{CoreError, Result};
use orbit_platform::{Cancellation, EntryKind, EntryRecord, FileIdentity};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Records committed in the first transaction of a listing (and the fresh-folder threshold).
pub const FIRST_BATCH: usize = 128;
/// Records per later transaction of a wide listing.
pub const WRITE_CHUNK: usize = 1_024;
/// A listing also commits when this much time passed since its previous commit.
pub const COMMIT_INTERVAL: Duration = Duration::from_millis(50);
/// Soft time budget of one grouped fresh-leaf transaction (checked between folders).
pub const FRESH_GROUP_BUDGET: Duration = Duration::from_millis(5);
/// Rows per trim, prune, link and refresh-marking chunk.
pub const CHUNK: i64 = 128;

const KIND_DIR: i64 = EntryKind::Directory as i64;

/// The highest mailbox sequence whose records are durable, for one mailbox generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Watermark {
    pub generation: uuid::Uuid,
    pub sequence: u64,
}

/// A registered (traversable) directory and the identity it must still have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedDirectory {
    pub path: Vec<u8>,
    pub identity: FileIdentity,
}

/// The listing a completion was started against, plus its issues.
#[derive(Clone, Copy, Debug)]
pub struct Completion<'a> {
    pub revision: i64,
    pub generation: i64,
    pub issue_count: u32,
    pub issues: &'a [String],
}

/// One finished, small, file-only folder for [`Writer::complete_fresh_leaf_listings`].
#[derive(Clone, Copy, Debug)]
pub struct FreshLeaf<'a> {
    pub path: &'a [u8],
    pub identity: FileIdentity,
    pub records: &'a RecordBatch,
    pub expected_revision: i64,
    pub expected_generation: i64,
    pub issue_count: u32,
    pub issues: &'a [String],
    pub watermark: Watermark,
}

impl FreshLeaf<'_> {
    /// Estimated payload, bounded per group (Swift `FreshLeafListing.byteCount`).
    pub fn byte_count(&self) -> usize {
        self.path.len()
            + 128
            + self.records.len() * std::mem::size_of::<crate::records::RecordMeta>()
            + self.records.iter().map(|r| r.name.len()).sum::<usize>()
            + self.issues.iter().take(100).map(String::len).sum::<usize>()
    }
}

/// Per-listing writer state carried across batches of one directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListingCursor {
    /// The listing's epoch once its first batch committed.
    pub epoch: Option<i64>,
    /// The directory had never been listed and had no rows: names are known to be new.
    pub fresh: bool,
    /// Rows the directory had when this listing minted its epoch (relistings only).
    pub existing: Option<i64>,
    /// Of those rows, how many this listing has seen so far.
    pub seen: i64,
    /// Seen rows that matched the listing exactly and were left untouched, so they still carry
    /// an older epoch. They are stamped only if the listing ends with stale rows to trim.
    pub unstamped: Vec<i64>,
}

/// A child row as [`Writer::upsert`] compares it with a listed entry.
struct ExistingRow {
    id: i64,
    kind: i64,
    dev: i64,
    inode: i64,
    size: i64,
    allocated: i64,
    raw_alloc: i64,
    accessible: bool,
    is_mount: bool,
    links: i64,
}

/// What [`Writer::upsert`] did with one record.
struct Upserted {
    id: i64,
    /// The name had a row before this listing.
    existed: bool,
    /// The existing row matched exactly and was not written.
    untouched: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendOutcome {
    Committed,
    /// A completion's revision/generation/freshness no longer matched; nothing was written.
    Rejected,
    /// A child path is registered with another identity (or turned into a file or mount); a
    /// prune marker was committed and the listing must be read again later.
    PruneRequired,
}

/// `(path, dev, is_mount_point) -> allowed`.
pub type TraversalFn = dyn Fn(&[u8], u64, bool) -> bool + Send + Sync;

/// Decides whether discovery may descend into a directory (volume boundaries, firmlinks).
/// Defaults to "same device as the root and not a mount point".
#[derive(Clone, Default)]
pub enum Traversal {
    #[default]
    SameDevice,
    /// `(path, dev, is_mount_point) -> allowed`; supplied by the discovery scope.
    Custom(Arc<TraversalFn>),
}

impl std::fmt::Debug for Traversal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SameDevice => "SameDevice",
            Self::Custom(_) => "Custom",
        })
    }
}

impl Traversal {
    #[inline]
    pub fn allows(&self, path: &[u8], dev: u64, is_mount: bool, root_dev: u64) -> bool {
        match self {
            Self::SameDevice => !is_mount && dev == root_dev,
            Self::Custom(allows) => allows(path, dev, is_mount),
        }
    }
}

/// Test and diagnostics hook, called at named commit points with the writer connection.
pub type CommitHook = Box<dyn FnMut(&str, &Connection) + Send>;
/// Monotonic clock (injected by tests).
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Default)]
struct Deltas {
    logical: i64,
    allocated: i64,
    files: i64,
}

enum Link {
    Rebuild,
    Candidate(i64),
}

const PROBE_IDENTITY: &str = "SELECT EXISTS(SELECT 1 FROM nodes WHERE dev=?1 AND inode=?2 AND kind<>1)";
const INSERT_NODE: &str = "INSERT INTO nodes(parent,name,search_name,kind,accessible,is_mount,size,allocated,\
    file_count,raw_alloc,dev,inode,links,epoch) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)";

/// Statements of the compact insert path, held for one batch and dropped before `COMMIT`.
struct FreshStatements<'c> {
    probe: rusqlite::CachedStatement<'c>,
    insert: rusqlite::CachedStatement<'c>,
}

pub struct Writer {
    conn: Connection,
    path: PathBuf,
    root: Vec<u8>,
    root_identity: FileIdentity,
    traversal: Traversal,
    queue_indexes_ready: bool,
    full_refresh_cursor: Option<(i64, Vec<u8>)>,
    hook: Option<CommitHook>,
    clock: Clock,
    fold: String,
    child: Vec<u8>,
    /// An open transaction group ([`Writer::begin_group`]): transactions become savepoints of it.
    group: bool,
    /// Ancestor-total deltas deferred while a group is open: `(directory id, logical, allocated,
    /// files)`, applied once per distinct ancestor by [`Writer::flush_ancestors`].
    deferred: std::cell::RefCell<Vec<(i64, i64, i64, i64)>>,
    /// Length of `deferred` when the open savepoint began; a rollback truncates back to it.
    deferred_marks: std::cell::RefCell<Vec<usize>>,
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer").field("path", &self.path).finish_non_exhaustive()
    }
}

fn sqlite_invalid(message: &str) -> CoreError {
    CoreError::Invalid(message.to_string())
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(failure, _)
        if failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
            || failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY)
}

pub(crate) fn join_into(out: &mut Vec<u8>, dir: &[u8], name: &[u8]) {
    out.clear();
    out.extend_from_slice(dir);
    if dir.last() != Some(&b'/') {
        out.push(b'/');
    }
    out.extend_from_slice(name);
}

/// `[dir/, dir0)`: every strict descendant path of `dir` in byte order.
pub(crate) fn descendant_range(dir: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut low = dir.to_vec();
    if low.last() != Some(&b'/') {
        low.push(b'/');
    }
    let mut high = low.clone();
    *high.last_mut().expect("non-empty") = b'0';
    (low, high)
}

fn split_parent(path: &[u8]) -> Option<(&[u8], &[u8])> {
    let parent = crate::paths::parent(path)?;
    Some((parent, crate::paths::file_name(path)))
}

impl Writer {
    /// Opens (creating if empty) a stage database. Fails for other formats.
    pub fn open(path: &Path, root: &[u8], root_identity: FileIdentity, traversal: Traversal) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_millis(schema::WRITER_BUSY_MS))?;
        conn.set_prepared_statement_cache_capacity(96);
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != 0 && version != schema::VERSION {
            return Err(sqlite_invalid("unsupported inventory format"));
        }
        if version == 0 {
            let existing: Option<i64> = conn
                .query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name='nodes'", [], |row| row.get(0))
                .optional()?;
            if existing.is_some() {
                return Err(sqlite_invalid("unversioned inventory"));
            }
        }
        conn.execute_batch(schema::WRITER_PRAGMAS)?;
        let mut writer = Self {
            conn,
            path: path.to_path_buf(),
            root: root.to_vec(),
            root_identity,
            traversal,
            queue_indexes_ready: false,
            full_refresh_cursor: None,
            hook: None,
            clock: Arc::new(Instant::now),
            fold: String::new(),
            child: Vec::new(),
            group: false,
            deferred: std::cell::RefCell::new(Vec::new()),
            deferred_marks: std::cell::RefCell::new(Vec::new()),
        };
        writer.transaction(|w| {
            w.conn.execute_batch(schema::INVENTORY_DDL)?;
            // A new stage builds its two size indexes once, when it is sealed: maintained row by
            // row, every insert and every ancestor total update moved index entries, and on a
            // whole-drive map that made writing about 9 s slower. Mapping does not read them (the
            // live inventory answers views and search); a stage reopened later gets them back
            // from `INVENTORY_DDL` above.
            if version == 0 {
                w.conn.execute_batch(schema::DEFERRED_INDEX_DROP)?;
            }
            w.conn.execute_batch(schema::STAGE_DDL)?;
            w.conn.execute_batch(&format!(
                "PRAGMA application_id={}; PRAGMA user_version={}",
                schema::APPLICATION_ID,
                schema::VERSION
            ))?;
            Ok(())
        })?;
        Ok(writer)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn root(&self) -> &[u8] {
        &self.root
    }

    pub fn root_identity(&self) -> FileIdentity {
        self.root_identity
    }

    pub fn traversal(&self) -> &Traversal {
        &self.traversal
    }

    /// The writer connection (diagnostics, probes and tests only).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn set_commit_hook(&mut self, hook: Option<CommitHook>) {
        self.hook = hook;
    }

    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    pub(crate) fn now(&self) -> Instant {
        (self.clock)()
    }

    fn emit(&mut self, event: &str) {
        if let Some(hook) = self.hook.as_mut() {
            hook(event, &self.conn);
        }
    }

    fn begin(&self) -> Result<()> {
        self.conn.execute_batch(if self.group { "SAVEPOINT orbit_step" } else { "BEGIN IMMEDIATE" })?;
        if self.group {
            let mark = self.deferred.borrow().len();
            self.deferred_marks.borrow_mut().push(mark);
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        if self.group {
            self.conn.execute_batch("RELEASE orbit_step")?;
            self.deferred_marks.borrow_mut().pop();
            return Ok(());
        }
        self.emit("commit");
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    fn rollback(&self) {
        if self.group {
            // Undoes only this step (rows and deferred totals); the group's earlier steps stay.
            let _ = self.conn.execute_batch("ROLLBACK TO orbit_step; RELEASE orbit_step");
            if let Some(mark) = self.deferred_marks.borrow_mut().pop() {
                self.deferred.borrow_mut().truncate(mark);
            }
        } else if !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }

    /// Opens a transaction group: until [`Writer::commit_group`], every transaction of this
    /// writer is a savepoint inside one `BEGIN IMMEDIATE`, so many small listings share one
    /// durable commit (one WAL sync, ancestor and index pages written once). Nothing in the group
    /// is durable before that commit; callers must not acknowledge its mailbox batches earlier.
    pub fn begin_group(&mut self) -> Result<()> {
        debug_assert!(!self.group && self.conn.is_autocommit());
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        self.group = true;
        Ok(())
    }

    pub fn in_group(&self) -> bool {
        self.group
    }

    /// Commits the open group durably (`synchronous=FULL`), with its deferred ancestor totals.
    pub fn commit_group(&mut self) -> Result<()> {
        if let Err(error) = self.flush_ancestors() {
            self.rollback_group();
            return Err(error);
        }
        self.group = false;
        self.emit("commit");
        if let Err(error) = self.conn.execute_batch("COMMIT") {
            self.rollback();
            return Err(error.into());
        }
        Ok(())
    }

    /// Discards the open group (everything since [`Writer::begin_group`]).
    pub fn rollback_group(&mut self) {
        self.group = false;
        self.deferred.borrow_mut().clear();
        self.deferred_marks.borrow_mut().clear();
        self.rollback();
    }

    /// Applies deferred ancestor deltas: each folder's delta is carried up its parent chain and
    /// every distinct ancestor is updated once. In a group of thousands of folders under deep
    /// trees (`node_modules`), the shared ancestors were otherwise rewritten, with their
    /// size-index entries, once per folder. Deltas add up in any order.
    fn flush_ancestors(&self) -> Result<()> {
        if self.deferred.borrow().is_empty() {
            return Ok(());
        }
        // Inside a savepoint, a rollback would undo the applied deltas after they left the log.
        if !self.deferred_marks.borrow().is_empty() {
            return Err(sqlite_invalid("ancestor totals flushed inside a savepoint"));
        }
        let log = std::mem::take(&mut *self.deferred.borrow_mut());
        let mut start: std::collections::HashMap<i64, (i64, i64, i64)> = std::collections::HashMap::new();
        for (id, logical, allocated, files) in log {
            let entry = start.entry(id).or_default();
            *entry = (entry.0 + logical, entry.1 + allocated, entry.2 + files);
        }
        // Parents are looked up once per flush (row ids can be reused after deletions).
        let mut parents: std::collections::HashMap<i64, Option<i64>> = std::collections::HashMap::new();
        let mut totals: std::collections::HashMap<i64, (i64, i64, i64)> = std::collections::HashMap::new();
        let mut parent_of = self.conn.prepare_cached("SELECT parent FROM nodes WHERE id=?1")?;
        for (id, delta) in start {
            if delta == (0, 0, 0) {
                continue;
            }
            let mut current = Some(id);
            while let Some(node) = current {
                let entry = totals.entry(node).or_default();
                *entry = (entry.0 + delta.0, entry.1 + delta.1, entry.2 + delta.2);
                current = match parents.get(&node) {
                    Some(parent) => *parent,
                    None => {
                        let parent: Option<i64> = parent_of
                            .query_row([node], |row| row.get(0))
                            .optional()?
                            .ok_or_else(|| sqlite_invalid("missing ancestor"))?;
                        parents.insert(node, parent);
                        parent
                    }
                };
            }
        }
        let mut update = self.conn.prepare_cached(
            "UPDATE nodes SET size=size+?1,allocated=allocated+?2,file_count=file_count+?3 WHERE id=?4",
        )?;
        for (id, (logical, allocated, files)) in totals {
            if (logical, allocated, files) != (0, 0, 0) && update.execute(params![logical, allocated, files, id])? == 0
            {
                return Err(sqlite_invalid("missing ancestor"));
            }
        }
        Ok(())
    }

    /// Runs `body` in `BEGIN IMMEDIATE … COMMIT`, rolling back on error.
    fn transaction<T>(&mut self, body: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.begin()?;
        match body(self) {
            Ok(value) => match self.commit() {
                Ok(()) => Ok(value),
                Err(error) => {
                    self.rollback();
                    Err(error)
                }
            },
            Err(error) => {
                self.rollback();
                Err(error)
            }
        }
    }

    // --- Scan-start checkpoint, expectation and seal ------------------------------------------

    /// Stores the scan-start checkpoint (first open) or verifies it matches (resume).
    pub fn preserve_scan_start(
        &mut self,
        identity: &str,
        event_id: u64,
        expected: Option<&crate::lease::SnapshotExpectation>,
        generation: Option<&str>,
    ) -> Result<()> {
        let root = self.root.clone();
        self.transaction(|w| {
            let other: Option<i64> =
                w.conn.query_row("SELECT 1 FROM scan WHERE root<>?1 LIMIT 1", [&root], |row| row.get(0)).optional()?;
            if other.is_some() {
                return Err(sqlite_invalid("stage belongs to another root"));
            }
            let encoded = expected.map_or_else(|| "unspecified".to_string(), |e| e.encode());
            w.conn.execute(
                "INSERT OR IGNORE INTO scan(root,identity,event_id,expected_snapshot,generation) VALUES(?1,?2,?3,?4,NULL)",
                params![root, identity, to_sql_u64(event_id), encoded],
            )?;
            let (stored_identity, stored_event, stored_expected): (String, i64, String) = w.conn.query_row(
                "SELECT identity,event_id,expected_snapshot FROM scan WHERE root=?1",
                [&root],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if stored_identity != identity
                || from_sql_u64(stored_event) != event_id
                || expected.is_some_and(|e| e.encode() != stored_expected)
            {
                return Err(sqlite_invalid("scan-start checkpoint mismatch"));
            }
            if let Some(generation) = generation {
                w.conn.execute("UPDATE scan SET generation=?2 WHERE root=?1 AND generation IS NULL", params![
                    root, generation
                ])?;
                let stored: Option<String> =
                    w.conn.query_row("SELECT generation FROM scan WHERE root=?1", [&root], |row| row.get(0))?;
                if stored.as_deref() != Some(generation) {
                    return Err(CoreError::SnapshotChanged);
                }
            }
            Ok(())
        })
    }

    /// The scan-start checkpoint `(identity, event_id)`, if this stage was started before.
    pub fn scan_start(&self) -> Result<Option<(String, u64)>> {
        Ok(self
            .conn
            .query_row("SELECT identity,event_id FROM scan WHERE root=?1", [&self.root], |row| {
                Ok((row.get::<_, String>(0)?, from_sql_u64(row.get(1)?)))
            })
            .optional()?)
    }

    pub fn scan_expectation(&self) -> Result<Option<crate::lease::SnapshotExpectation>> {
        let encoded: Option<String> = self
            .conn
            .query_row("SELECT expected_snapshot FROM scan WHERE root=?1", [&self.root], |row| row.get(0))
            .optional()?;
        match encoded.as_deref() {
            None | Some("unspecified") => Ok(None),
            Some(text) => crate::lease::SnapshotExpectation::decode(text)
                .map(Some)
                .ok_or_else(|| sqlite_invalid("invalid scan expectation")),
        }
    }

    pub fn scan_generation(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT generation FROM scan WHERE root=?1", [&self.root], |row| row.get::<_, Option<String>>(0))
            .optional()?
            .flatten())
    }

    pub fn rebase_expectation(
        &mut self,
        from: &crate::lease::SnapshotExpectation,
        to: &crate::lease::SnapshotExpectation,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE scan SET expected_snapshot=?1 WHERE root=?2 AND expected_snapshot=?3",
            params![to.encode(), self.root, from.encode()],
        )?;
        if changed != 1 {
            return Err(CoreError::SnapshotChanged);
        }
        Ok(())
    }

    /// Moves the scan-start event floor down to `event_id` (reuse of an older saved map).
    pub fn lower_checkpoint(&mut self, identity: &str, event_id: u64) -> Result<()> {
        let Some((previous_identity, previous_event)) = self.scan_start()? else {
            return Err(sqlite_invalid("incompatible reuse checkpoint"));
        };
        if previous_identity != identity || event_id > previous_event {
            return Err(sqlite_invalid("incompatible reuse checkpoint"));
        }
        if event_id == previous_event {
            return Ok(());
        }
        let root = self.root.clone();
        self.transaction(|w| {
            w.conn.execute("UPDATE scan SET event_id=?1 WHERE root=?2", params![to_sql_u64(event_id), root])?;
            w.conn.execute("UPDATE metadata SET event_id=?1 WHERE singleton=1", [to_sql_u64(event_id)])?;
            Ok(())
        })
    }

    /// Creates the root node and its directory row if this is a new stage.
    /// Turns this copy of the published saved map `snapshot_id` into the start of a new stage
    /// of a rescan: no scan, seal, snapshot or progress of the old one (a new id follows), no
    /// leftover bookkeeping, and a new full-refresh generation so every directory is listed
    /// again before anything is published. Rows, owners, issues and the removal cursor stay.
    /// Refuses copies that are not that map, have unfinished work or are mostly free pages.
    pub fn prepare_seed(&mut self, snapshot_id: &str) -> Result<()> {
        let info = schema::read_snapshot(&self.conn, &self.path)?;
        if info.id != snapshot_id || info.root_path != self.root {
            return Err(CoreError::SnapshotChanged);
        }
        let root = self.root.clone();
        if self.directory(&root)?.is_none_or(|d| d.identity != self.root_identity) {
            return Err(sqlite_invalid("the saved map belongs to another folder"));
        }
        if self.has_remaining_work()? {
            return Err(sqlite_invalid("the saved map has unfinished work"));
        }
        let free: i64 = self.conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        let pages: i64 = self.conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        if free * 4 > pages {
            return Err(sqlite_invalid("the saved map is mostly free space; a fresh scan compacts it"));
        }
        self.transaction(|w| {
            w.conn.execute_batch(
                "DELETE FROM scan; DELETE FROM sealed; DELETE FROM metadata; DELETE FROM snapshot_issues;
                 DELETE FROM orbit_async_progress; DELETE FROM dirty; DELETE FROM link_candidates;
                 DELETE FROM link_rebuild; DELETE FROM dirty_links; DELETE FROM trim; DELETE FROM prune;
                 DROP TABLE IF EXISTS replay_parents;",
            )?;
            w.mark_full_refresh()
        })
    }

    pub fn register_root(&mut self) -> Result<()> {
        let root = self.root.clone();
        let identity = self.root_identity;
        self.transaction(|w| {
            let existing: Option<(i64, i64)> = w
                .conn
                .query_row("SELECT dev,inode FROM dirs WHERE path=?1", [&root], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            if let Some((dev, inode)) = existing {
                if identity_from_sql(dev, inode) != identity {
                    return Err(sqlite_invalid("directory identity changed; subtree reconciliation required"));
                }
                return Ok(());
            }
            let name: &[u8] = if root == b"/" { b"/" } else { crate::paths::file_name(&root) };
            w.conn.execute(
                "INSERT INTO nodes(parent,name,search_name,kind,accessible,is_mount,size,allocated,file_count,raw_alloc,dev,inode,links,epoch)
                 VALUES(NULL,?1,?2,?3,1,0,0,0,0,0,?4,?5,1,0)",
                params![name, crate::fold::fold(name), KIND_DIR, to_sql_u64(identity.dev), to_sql_u64(identity.ino)],
            )?;
            let id = w.conn.last_insert_rowid();
            w.conn.execute("INSERT INTO dirs(path,id,dev,inode,pending) VALUES(?1,?2,?3,?4,1)", params![
                root,
                id,
                to_sql_u64(identity.dev),
                to_sql_u64(identity.ino)
            ])?;
            Ok(())
        })
    }

    /// Private partial snapshot metadata for readers (inserted once; final seal replaces it).
    pub fn preview_snapshot(&mut self, identity: &str, event_id: u64) -> Result<schema::SnapshotInfo> {
        let existing: Option<(String, f64)> = self
            .conn
            .query_row("SELECT snapshot_id,scanned_at FROM metadata WHERE singleton=1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let (id, scanned_at) = match existing {
            Some(found) => found,
            None => {
                let id = uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase();
                let now = unix_now();
                self.conn.execute(
                    "INSERT INTO metadata VALUES(1,?1,?2,?3,0,0,?4,?5)",
                    params![id, self.root, now, to_sql_u64(event_id), identity],
                )?;
                (id, now)
            }
        };
        Ok(schema::SnapshotInfo {
            id,
            root_path: self.root.clone(),
            database: self.path.clone(),
            scanned_at,
            elapsed: 0.0,
            skipped: 0,
            issues: Vec::new(),
            event_id,
            event_identity: identity.to_string(),
        })
    }

    /// Stage work left before the stage may be sealed.
    pub fn has_remaining_work(&self) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM dirs WHERE pending=1 UNION ALL SELECT 1 FROM dirty_links UNION ALL SELECT 1 FROM prune
                 UNION ALL SELECT 1 FROM trim UNION ALL SELECT 1 FROM full_refresh WHERE pending=1 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Seals the finished stage: final metadata, issues and the `sealed` marker, atomically.
    pub fn seal(&mut self, identity: &str, event_id: u64, elapsed: f64) -> Result<schema::SnapshotInfo> {
        let root = self.root.clone();
        let path = self.path.clone();
        // Building the deferred indexes sorts every node; the writer's in-memory temp store
        // would hold those sort runs in RAM (about 500 MB more on a 5-million-file map).
        self.conn.execute_batch("PRAGMA temp_store=FILE; PRAGMA cache_size=-32768;")?;
        let sealed = self.seal_inner(identity, event_id, elapsed, root, path);
        self.conn.execute_batch("PRAGMA temp_store=MEMORY; PRAGMA cache_size=-262144;")?;
        sealed
    }

    fn seal_inner(
        &mut self,
        identity: &str,
        event_id: u64,
        elapsed: f64,
        root: Vec<u8>,
        path: PathBuf,
    ) -> Result<schema::SnapshotInfo> {
        self.transaction(|w| {
            if w.has_remaining_work()? {
                return Err(sqlite_invalid("stage has pending work"));
            }
            match w.scan_start()? {
                Some((stored, stored_event)) if stored == identity && stored_event == event_id => {}
                _ => return Err(sqlite_invalid("original checkpoint changed")),
            }
            let mut issues: Vec<String> = {
                let mut statement = w.conn.prepare("SELECT message FROM stage_issues ORDER BY path LIMIT 100")?;
                statement.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            if issues.len() < 100 {
                let mut statement = w.conn.prepare("SELECT messages FROM listing_issues ORDER BY parent")?;
                let mut rows = statement.query([])?;
                while issues.len() < 100 {
                    let Some(row) = rows.next()? else { break };
                    let bytes: Vec<u8> = row.get(0)?;
                    let remaining = 100 - issues.len();
                    issues.extend(schema::decode_messages(&bytes).into_iter().take(remaining));
                }
            }
            let skipped: i64 = w.conn.query_row(
                "SELECT (SELECT COUNT(*) FROM stage_issues)+(SELECT COALESCE(SUM(count),0) FROM listing_issues)",
                [],
                |row| row.get(0),
            )?;
            let existing: Option<String> = w
                .conn
                .query_row("SELECT snapshot_id FROM metadata WHERE singleton=1", [], |row| row.get(0))
                .optional()?;
            let id = existing.unwrap_or_else(|| uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase());
            let now = unix_now();
            w.conn.execute(
                "INSERT OR REPLACE INTO metadata VALUES(1,?1,?2,?3,?4,?5,?6,?7)",
                params![id, root, now, elapsed, skipped, to_sql_u64(event_id), identity],
            )?;
            w.conn.execute("DELETE FROM snapshot_issues", [])?;
            for (ord, message) in issues.iter().enumerate() {
                w.conn.execute("INSERT INTO snapshot_issues VALUES(?1,?2)", params![ord as i64, message])?;
            }
            // The size indexes a new stage deferred (see `open`); no-op when they exist.
            w.conn.execute_batch(schema::DEFERRED_INDEX_DDL)?;
            w.conn.execute("INSERT OR REPLACE INTO sealed(singleton,snapshot_id,published) VALUES(1,?1,0)", [&id])?;
            Ok(schema::SnapshotInfo {
                id,
                root_path: root,
                database: path,
                scanned_at: now,
                elapsed,
                skipped: skipped.max(0) as u64,
                issues,
                event_id,
                event_identity: identity.to_string(),
            })
        })
    }

    /// The sealed snapshot of a finished stage `(id, published)`.
    pub fn completed_snapshot(&self) -> Result<Option<(String, bool)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT metadata.snapshot_id,sealed.published FROM metadata JOIN sealed ON sealed.singleton=1
                   AND sealed.snapshot_id=metadata.snapshot_id WHERE root_path=?1
                   AND NOT EXISTS(SELECT 1 FROM dirs WHERE pending=1) AND NOT EXISTS(SELECT 1 FROM dirty_links)
                   AND NOT EXISTS(SELECT 1 FROM prune) AND NOT EXISTS(SELECT 1 FROM trim)
                   AND NOT EXISTS(SELECT 1 FROM full_refresh WHERE pending=1)",
                [&self.root],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()?)
    }

    pub fn mark_published(&mut self, id: &str) -> Result<()> {
        self.transaction(|w| {
            if w.conn.execute("UPDATE sealed SET published=1 WHERE singleton=1 AND snapshot_id=?1", [id])? != 1 {
                return Err(sqlite_invalid("sealed snapshot changed"));
            }
            Ok(())
        })
    }

    /// Folds the WAL into the main file and switches to rollback-journal mode so the database
    /// is a single self-contained file for cloning. The caller excludes other writes.
    pub fn checkpoint_for_clone(&mut self, timeout: Duration, cancel: &Cancellation) -> Result<()> {
        let deadline = Instant::now() + timeout;
        self.conn.busy_timeout(timeout.min(Duration::from_millis(100)))?;
        let result = (|| {
            loop {
                if cancel.is_cancelled() {
                    return Err(CoreError::Cancelled);
                }
                let (busy, _, _): (i64, i64, i64) =
                    self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?;
                if busy == 0 {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(sqlite_invalid("checkpoint failed: database busy"));
                }
            }
            if cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            let mode: String = self.conn.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
            if !mode.eq_ignore_ascii_case("delete") {
                return Err(sqlite_invalid("could not close WAL"));
            }
            Ok(())
        })();
        self.conn.busy_timeout(Duration::from_millis(schema::WRITER_BUSY_MS))?;
        result
    }

    pub fn resume_wal(&mut self) -> Result<()> {
        let mode: String = self.conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(sqlite_invalid("could not restore WAL"));
        }
        Ok(())
    }

    // --- Queue state ----------------------------------------------------------------------------

    /// Creates the partial queue indexes in one cancellable transaction (once per open).
    pub fn prepare_queue_indexes(&mut self, cancel: &Cancellation) -> Result<()> {
        if self.queue_indexes_ready {
            return Ok(());
        }
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let token = cancel.clone();
        self.conn.progress_handler(schema::PROGRESS_OPS, Some(move || token.is_cancelled()));
        let result = self.transaction(|w| {
            for (index, sql) in schema::QUEUE_INDEX_DDL.iter().enumerate() {
                w.conn.execute_batch(sql)?;
                if index == 0 {
                    w.emit("queue-index-first");
                }
            }
            if cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            Ok(())
        });
        self.conn.progress_handler(0, None::<fn() -> bool>);
        match result {
            Ok(()) => {
                self.queue_indexes_ready = true;
                Ok(())
            }
            Err(_) if cancel.is_cancelled() => Err(CoreError::Cancelled),
            Err(error) => Err(error),
        }
    }

    pub fn directory(&self, path: &[u8]) -> Result<Option<QueuedDirectory>> {
        Ok(self
            .conn
            .prepare_cached("SELECT dev,inode FROM dirs WHERE path=?1")?
            .query_row([path], |row| Ok(identity_from_sql(row.get(0)?, row.get(1)?)))
            .optional()?
            .map(|identity| QueuedDirectory { path: path.to_vec(), identity }))
    }

    pub fn dirty_revision(&self, parent: &[u8]) -> Result<i64> {
        Ok(self
            .conn
            .prepare_cached("SELECT revision FROM dirty WHERE parent=?1")?
            .query_row([parent], |row| row.get(0))
            .optional()?
            .unwrap_or(0))
    }

    pub fn is_dirty(&self, parent: &[u8]) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT pending FROM dirty WHERE parent=?1")?
            .query_row([parent], |row| row.get::<_, i64>(0))
            .optional()?
            .is_some_and(|p| p != 0))
    }

    /// `(generation, pending)` of the full-refresh marker.
    pub fn full_refresh_state(&self) -> Result<(i64, bool)> {
        Ok(self
            .conn
            .prepare_cached("SELECT generation,pending FROM full_refresh WHERE singleton=1")?
            .query_row([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)))?)
    }

    /// The directory never completed a listing, has no rows and is queued.
    pub fn can_complete_fresh_listing(&self, parent: &[u8]) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT d.epoch=0 AND d.pending=1 AND NOT EXISTS(SELECT 1 FROM nodes WHERE parent=d.id) FROM dirs d WHERE d.path=?1",
            )?
            .query_row([parent], |row| row.get::<_, bool>(0))
            .optional()?
            .unwrap_or(false))
    }

    pub fn has_pending_directories(&self) -> Result<bool> {
        Ok(self.conn.query_row("SELECT 1 FROM dirs WHERE pending=1 LIMIT 1", [], |_| Ok(())).optional()?.is_some())
    }

    pub fn has_trim(&self) -> Result<bool> {
        Ok(self.conn.query_row("SELECT 1 FROM trim LIMIT 1", [], |_| Ok(())).optional()?.is_some())
    }

    pub fn has_prune(&self) -> Result<bool> {
        Ok(self.conn.query_row("SELECT 1 FROM prune LIMIT 1", [], |_| Ok(())).optional()?.is_some())
    }

    /// Dirty listings, link, prune, trim or refresh work remains (removal replay readiness).
    pub fn has_replay_dirty_work(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM dirty WHERE pending=1 UNION ALL SELECT 1 FROM dirty_links UNION ALL SELECT 1 FROM prune
                 UNION ALL SELECT 1 FROM trim UNION ALL SELECT 1 FROM full_refresh WHERE pending=1 LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// No listing has been written yet (a removal-journal cursor may anchor at the present).
    pub fn is_fresh_for_removal_anchor(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM nodes WHERE parent IS NOT NULL UNION ALL SELECT 1 FROM dirs WHERE epoch<>0
                 UNION ALL SELECT 1 FROM dirty UNION ALL SELECT 1 FROM prune UNION ALL SELECT 1 FROM trim
                 UNION ALL SELECT 1 FROM sealed LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_none())
    }

    /// The next directory to list: dirty first, then the preferred folder's nearest queued
    /// ancestor or descendant, then path order.
    pub fn next_directory(&self, preferred: Option<&[u8]>) -> Result<Option<QueuedDirectory>> {
        let row = |row: &rusqlite::Row<'_>| {
            Ok(QueuedDirectory { path: row.get(0)?, identity: identity_from_sql(row.get(1)?, row.get(2)?) })
        };
        if let Some(found) = self
            .conn
            .prepare_cached(
                "SELECT d.path,d.dev,d.inode FROM dirty JOIN dirs d ON d.path=dirty.parent
                 WHERE dirty.pending=1 AND d.pending=1 ORDER BY dirty.parent LIMIT 1",
            )?
            .query_row([], row)
            .optional()?
        {
            return Ok(Some(found));
        }
        if let Some(preferred) = preferred
            && preferred != self.root.as_slice()
            && crate::paths::is_within(preferred, &self.root)
        {
            let mut chain = vec![preferred.to_vec()];
            let mut current = preferred;
            while let Some(parent) = crate::paths::parent(current) {
                if !crate::paths::is_within(parent, &self.root) {
                    break;
                }
                chain.push(parent.to_vec());
                current = parent;
            }
            let mut exact = self.conn.prepare_cached("SELECT path,dev,inode FROM dirs WHERE path=?1 AND pending=1")?;
            for path in chain.iter().rev() {
                if let Some(found) = exact.query_row([path], row).optional()? {
                    return Ok(Some(found));
                }
            }
            let (low, high) = descendant_range(preferred);
            if let Some(found) = self
                .conn
                .prepare_cached(
                    "SELECT path,dev,inode FROM dirs WHERE pending=1 AND path>=?1 AND path<?2 ORDER BY path LIMIT 1",
                )?
                .query_row([low, high], row)
                .optional()?
            {
                return Ok(Some(found));
            }
        }
        Ok(self
            .conn
            .prepare_cached("SELECT path,dev,inode FROM dirs WHERE pending=1 ORDER BY path LIMIT 1")?
            .query_row([], row)
            .optional()?)
    }

    // --- Listing writes -------------------------------------------------------------------------

    fn persist_watermark(&self, watermark: Option<Watermark>) -> Result<()> {
        if let Some(watermark) = watermark {
            self.conn
                .prepare_cached(
                    "INSERT INTO orbit_async_progress VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET
                     generation=excluded.generation,sequence=excluded.sequence",
                )?
                .execute(params![&watermark.generation.as_bytes()[..], to_sql_u64(watermark.sequence)])?;
        }
        Ok(())
    }

    /// The durable mailbox watermark.
    pub fn watermark(&self) -> Result<Option<Watermark>> {
        read_watermark(&self.conn)
    }

    /// Stores only the watermark (consumed batches that were skipped or rejected).
    pub fn acknowledge_async(&mut self, watermark: Watermark) -> Result<()> {
        self.transaction(|w| w.persist_watermark(Some(watermark)))
    }

    /// Appends one batch of a listing in its own transaction. The first call mints the epoch.
    pub fn append(
        &mut self,
        parent: &[u8],
        cursor: &mut ListingCursor,
        records: &[&RecordBatch],
        watermark: Option<Watermark>,
    ) -> Result<AppendOutcome> {
        self.append_batch(parent, cursor, records, None, watermark, true)
    }

    /// Commits a whole small listing of a never-listed folder (records, child queue, issues and
    /// completion) in one transaction. Returns `false` (nothing written) when it no longer
    /// qualifies; the caller then uses [`Writer::append`] + [`Writer::finish`].
    pub fn complete_fresh_listing(
        &mut self,
        parent: &[u8],
        records: &[&RecordBatch],
        completion: Completion<'_>,
        watermark: Option<Watermark>,
    ) -> Result<bool> {
        let count: usize = records.iter().map(|b| b.len()).sum();
        let bytes: usize = records
            .iter()
            .flat_map(|b| b.iter())
            .map(|r| std::mem::size_of::<crate::records::RecordMeta>() + r.name.len())
            .sum();
        if completion.revision != 0
            || count > WRITE_CHUNK
            || bytes > 256 * 1024
            || !self.can_complete_fresh_listing(parent)?
        {
            return Ok(false);
        }
        let mut cursor = ListingCursor::default();
        Ok(self.append_batch(parent, &mut cursor, records, Some(completion), watermark, true)?
            == AppendOutcome::Committed)
    }

    /// Commits up to 32 finished small leaf folders in one transaction, stopping between folders
    /// once [`FRESH_GROUP_BUDGET`] is spent. Returns how many leading folders committed (their
    /// last watermark is stored with them); `0` means nothing was written.
    pub fn complete_fresh_leaf_listings(&mut self, leaves: &[FreshLeaf<'_>], cancel: &Cancellation) -> Result<usize> {
        let Some(first) = leaves.first() else { return Ok(0) };
        let records: usize = leaves.iter().map(|l| l.records.len()).sum();
        let bytes: usize = leaves.iter().map(FreshLeaf::byte_count).sum();
        let mut paths = std::collections::HashSet::with_capacity(leaves.len());
        let valid = (2..=crate::mailbox::FRESH_LEAF_MAX_FOLDERS).contains(&leaves.len())
            && records <= crate::mailbox::FRESH_LEAF_MAX_RECORDS
            && bytes <= crate::mailbox::FRESH_LEAF_MAX_BYTES
            && leaves.iter().all(|l| paths.insert(l.path))
            && first.watermark.sequence > 0
            && leaves.iter().all(|l| {
                l.expected_revision == 0
                    && l.records.len() < crate::mailbox::FRESH_LEAF_MAX_FOLDER_RECORDS
                    && l.records.metas().iter().all(|m| m.kind != EntryKind::Directory)
                    && l.watermark.generation == first.watermark.generation
            })
            && leaves.windows(2).all(|w| w[0].watermark.sequence.checked_add(1) == Some(w[1].watermark.sequence));
        if !valid {
            return Ok(0);
        }
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let deadline = self.now() + FRESH_GROUP_BUDGET;
        self.begin()?;
        let result = (|| -> Result<Option<usize>> {
            let mut completed = 0;
            for leaf in leaves {
                if completed > 0 && self.now() >= deadline {
                    break;
                }
                if cancel.is_cancelled() {
                    return Err(CoreError::Cancelled);
                }
                if self.directory(leaf.path)?.is_none_or(|d| d.identity != leaf.identity) {
                    return Ok(None);
                }
                let completion = Completion {
                    revision: leaf.expected_revision,
                    generation: leaf.expected_generation,
                    issue_count: leaf.issue_count,
                    issues: leaf.issues,
                };
                let mut cursor = ListingCursor::default();
                if self.append_batch(leaf.path, &mut cursor, &[leaf.records], Some(completion), None, false)?
                    != AppendOutcome::Committed
                {
                    return Ok(None);
                }
                completed += 1;
            }
            self.persist_watermark(Some(leaves[completed - 1].watermark))?;
            self.emit("fresh-group-written");
            if cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            Ok(Some(completed))
        })();
        match result {
            Ok(Some(completed)) => {
                if let Err(error) = self.commit() {
                    self.rollback();
                    return Err(error);
                }
                self.emit("fresh-group-committed");
                Ok(completed)
            }
            Ok(None) => {
                self.rollback();
                Ok(0)
            }
            Err(error) => {
                self.rollback();
                Err(error)
            }
        }
    }

    fn traversable(&self, path: &[u8], record: &EntryRecord<'_>) -> bool {
        self.traversal.allows(path, record.id.dev, record.is_mount_point, self.root_identity.dev)
    }

    /// Checks registered child directories against the batch. Returns the first child whose
    /// registration conflicts (identity, kind or traversal changed).
    fn preflight(&mut self, parent: &[u8], records: &[&RecordBatch]) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        let (low, high) = descendant_range(parent);
        let any: Option<i64> = self
            .conn
            .prepare_cached("SELECT 1 FROM dirs WHERE path>=?1 AND path<?2 LIMIT 1")?
            .query_row([&low, &high], |row| row.get(0))
            .optional()?;
        if any.is_none() {
            return Ok(None);
        }
        let mut child = std::mem::take(&mut self.child);
        let result = (|| {
            let mut query = self.conn.prepare_cached("SELECT dev,inode FROM dirs WHERE path=?1")?;
            for record in records.iter().flat_map(|b| b.iter()) {
                join_into(&mut child, parent, record.name);
                let Some((dev, inode)) =
                    query.query_row([&child], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).optional()?
                else {
                    continue;
                };
                if !record.is_dir() || !self.traversable(&child, &record) || identity_from_sql(dev, inode) != record.id
                {
                    return Ok(Some((child.clone(), record.name.to_vec())));
                }
            }
            Ok(None)
        })();
        self.child = child;
        result
    }

    fn append_batch(
        &mut self,
        parent: &[u8],
        cursor: &mut ListingCursor,
        records: &[&RecordBatch],
        completion: Option<Completion<'_>>,
        watermark: Option<Watermark>,
        owns_transaction: bool,
    ) -> Result<AppendOutcome> {
        if owns_transaction {
            self.begin()?;
        }
        let result = self.append_inner(parent, cursor, records, completion, watermark, owns_transaction);
        match result {
            Ok(AppendOutcome::Committed) if owns_transaction => {
                if let Err(error) = self.commit() {
                    self.rollback();
                    return Err(error);
                }
                self.emit(if completion.is_some() { "fresh-committed" } else { "batch-committed" });
                Ok(AppendOutcome::Committed)
            }
            Ok(AppendOutcome::PruneRequired) if owns_transaction => {
                // Only the prune marker was written.
                self.commit()?;
                Ok(AppendOutcome::PruneRequired)
            }
            Ok(outcome) => {
                if owns_transaction {
                    self.rollback();
                }
                Ok(outcome)
            }
            Err(error) => {
                if owns_transaction {
                    self.rollback();
                }
                Err(error)
            }
        }
    }

    fn append_inner(
        &mut self,
        parent: &[u8],
        cursor: &mut ListingCursor,
        records: &[&RecordBatch],
        completion: Option<Completion<'_>>,
        watermark: Option<Watermark>,
        owns_transaction: bool,
    ) -> Result<AppendOutcome> {
        if let Some((child, name)) = self.preflight(parent, records)? {
            if owns_transaction {
                self.insert_prune(&child, parent, &name)?;
            }
            return Ok(AppendOutcome::PruneRequired);
        }
        if let Some(completion) = completion {
            let (generation, pending) = self.full_refresh_state()?;
            if self.dirty_revision(parent)? != completion.revision
                || generation != completion.generation
                || pending
                || !self.can_complete_fresh_listing(parent)?
            {
                return Ok(AppendOutcome::Rejected);
            }
        }
        let parent_id: i64 = match cursor.epoch {
            Some(_) => self
                .conn
                .prepare_cached("SELECT id FROM dirs WHERE path=?1")?
                .query_row([parent], |row| row.get(0))
                .optional()?
                .ok_or_else(|| sqlite_invalid("missing node parent"))?,
            None => {
                let (id, epoch): (i64, i64) = self
                    .conn
                    .prepare_cached("UPDATE dirs SET epoch=epoch+1 WHERE path=?1 RETURNING id,epoch")?
                    .query_row([parent], |row| Ok((row.get(0)?, row.get(1)?)))
                    .optional()?
                    .ok_or_else(|| sqlite_invalid("missing node parent"))?;
                let has_rows: bool = self
                    .conn
                    .prepare_cached("SELECT EXISTS(SELECT 1 FROM nodes WHERE parent=?1)")?
                    .query_row([id], |row| row.get(0))?;
                cursor.fresh = epoch == 1 && !has_rows;
                cursor.epoch = Some(epoch);
                if !cursor.fresh {
                    cursor.existing = Some(
                        self.conn
                            .prepare_cached("SELECT COUNT(*) FROM nodes WHERE parent=?1")?
                            .query_row([id], |row| row.get(0))?,
                    );
                }
                self.emit(if completion.is_some() { "fresh-epoch" } else { "batch-epoch" });
                id
            }
        };
        let epoch = cursor.epoch.expect("epoch minted above");
        let mut may_have_issues = self
            .conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM stage_issues)")?
            .query_row([], |row| row.get::<_, bool>(0))?;
        let mut deltas = Deltas::default();
        let mut child = std::mem::take(&mut self.child);
        let mut fold = std::mem::take(&mut self.fold);
        // Applied to the cursor only once the batch is written.
        let (mut seen, mut unstamped) = (0i64, Vec::new());
        let result = (|| -> Result<()> {
            // The compact insert path keeps its statements for the whole batch.
            let mut fresh = if cursor.fresh {
                Some(FreshStatements {
                    probe: self.conn.prepare_cached(PROBE_IDENTITY)?,
                    insert: self.conn.prepare_cached(INSERT_NODE)?,
                })
            } else {
                None
            };
            for record in records.iter().flat_map(|b| b.iter()) {
                let is_dir = record.is_dir();
                if is_dir || may_have_issues {
                    join_into(&mut child, parent, record.name);
                }
                let traversable = !is_dir || self.traversable(&child, &record);
                let inserted = match fresh.as_mut() {
                    Some(statements) => {
                        fold.clear();
                        crate::fold::fold_into(record.name, &mut fold);
                        self.insert_fresh(statements, parent_id, &record, &fold, traversable, epoch, &mut deltas)?
                    }
                    None => None,
                };
                let node_id = match inserted {
                    Some(id) => {
                        if !is_dir && record.nlink > 1 {
                            self.mark_link(record.id, Link::Candidate(id))?;
                        }
                        id
                    }
                    None => {
                        let upserted = self.upsert(parent_id, &record, &mut fold, traversable, epoch, &mut deltas)?;
                        seen += i64::from(upserted.existed);
                        if upserted.untouched {
                            unstamped.push(upserted.id);
                        }
                        upserted.id
                    }
                };
                if may_have_issues {
                    self.conn.prepare_cached("DELETE FROM stage_issues WHERE path=?1")?.execute([&child])?;
                }
                if is_dir {
                    if traversable {
                        self.register_directory(&child, node_id, record.id, cursor.fresh)?;
                    } else {
                        may_have_issues = true;
                        let message = format!("{}: nested volume skipped", String::from_utf8_lossy(&child));
                        self.conn
                            .prepare_cached("INSERT OR REPLACE INTO stage_issues(path,message) VALUES(?1,?2)")?
                            .execute(params![&child, message])?;
                    }
                }
            }
            Ok(())
        })();
        self.child = child;
        self.fold = fold;
        result?;
        cursor.seen += seen;
        cursor.unstamped.extend(unstamped);
        self.adjust_ancestors(parent_id, deltas.logical, deltas.allocated, deltas.files)?;
        if let Some(completion) = completion {
            self.record_listing_issues(parent, completion.issue_count, completion.issues)?;
            self.conn.prepare_cached("UPDATE dirs SET pending=0 WHERE path=?1")?.execute([parent])?;
        }
        self.persist_watermark(watermark)?;
        Ok(AppendOutcome::Committed)
    }

    fn identity_exists(&self, id: FileIdentity) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached(PROBE_IDENTITY)?
            .query_row([to_sql_u64(id.dev), to_sql_u64(id.ino)], |row| row.get(0))?)
    }

    fn mark_link(&self, id: FileIdentity, link: Link) -> Result<()> {
        let (dev, inode) = (to_sql_u64(id.dev), to_sql_u64(id.ino));
        self.conn.prepare_cached("INSERT OR IGNORE INTO dirty_links VALUES(?1,?2)")?.execute([dev, inode])?;
        match link {
            Link::Rebuild => {
                self.conn.prepare_cached("INSERT OR IGNORE INTO link_rebuild VALUES(?1,?2)")?.execute([dev, inode])?
            }
            Link::Candidate(node) => self
                .conn
                .prepare_cached("INSERT OR IGNORE INTO link_candidates VALUES(?1,?2,?3)")?
                .execute([dev, inode, node])?,
        };
        Ok(())
    }

    /// Compact insert for a name proven new (the folder had no rows when its listing started).
    /// `None` when the name already exists (a duplicate inside one listing); the caller falls
    /// back to [`Writer::upsert`]. Single-link files whose identity is already indexed elsewhere
    /// are marked for link reconciliation; multi-link candidates are marked by the caller.
    #[allow(clippy::too_many_arguments)]
    fn insert_fresh(
        &self,
        statements: &mut FreshStatements<'_>,
        parent_id: i64,
        record: &EntryRecord<'_>,
        fold: &str,
        traversable: bool,
        epoch: i64,
        deltas: &mut Deltas,
    ) -> Result<Option<i64>> {
        let is_dir = record.is_dir();
        let (dev, inode) = (to_sql_u64(record.id.dev), to_sql_u64(record.id.ino));
        // (Folding this probe into the INSERT as a subquery measured ~50% slower.)
        let mut charge = 0;
        let mut duplicate_identity = false;
        if !is_dir && record.nlink <= 1 {
            if statements.probe.query_row([dev, inode], |row| row.get::<_, bool>(0))? {
                duplicate_identity = true;
            } else {
                charge = to_sql_size(record.allocated);
            }
        }
        let inserted = statements.insert.execute(params![
            parent_id,
            record.name,
            fold,
            record.kind as i64,
            traversable,
            record.is_mount_point,
            if is_dir { 0 } else { to_sql_size(record.size) },
            charge,
            if is_dir { 0 } else { 1 },
            if is_dir { 0 } else { to_sql_size(record.allocated) },
            dev,
            inode,
            record.nlink as i64,
            epoch,
        ]);
        match inserted {
            Ok(_) => {}
            Err(error) if is_unique_violation(&error) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let id = self.conn.last_insert_rowid();
        if !is_dir {
            if duplicate_identity {
                self.mark_link(record.id, Link::Rebuild)?;
            }
            deltas.logical += to_sql_size(record.size);
            deltas.allocated += charge;
            deltas.files += 1;
        }
        Ok(Some(id))
    }

    /// Insert-or-update of one listed name (relistings, refreshes, duplicates).
    fn upsert(
        &self,
        parent_id: i64,
        record: &EntryRecord<'_>,
        fold: &mut String,
        traversable: bool,
        epoch: i64,
        deltas: &mut Deltas,
    ) -> Result<Upserted> {
        let found: Option<ExistingRow> = self
            .conn
            .prepare_cached(
                "SELECT id,kind,dev,inode,size,allocated,raw_alloc,accessible,is_mount,links FROM nodes
                 WHERE parent=?1 AND name=?2",
            )?
            .query_row(params![parent_id, record.name], |row| {
                Ok(ExistingRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    dev: row.get(2)?,
                    inode: row.get(3)?,
                    size: row.get(4)?,
                    allocated: row.get(5)?,
                    raw_alloc: row.get(6)?,
                    accessible: row.get(7)?,
                    is_mount: row.get(8)?,
                    links: row.get(9)?,
                })
            })
            .optional()?;
        let is_dir = record.is_dir();
        // A relisted entry identical to its row (a rescan of unchanged data) is left as it is:
        // no write, no index maintenance, no name folding. Its deltas and link marks are zero
        // anyway; the listing remembers it in case stale rows need trimming (`finish`).
        if let Some(row) = &found {
            let same_entry = row.kind == record.kind as i64
                && identity_from_sql(row.dev, row.inode) == record.id
                && row.accessible == traversable
                && row.is_mount == record.is_mount_point
                && row.links == record.nlink as i64;
            let same_size =
                is_dir || (row.size == to_sql_size(record.size) && row.raw_alloc == to_sql_size(record.allocated));
            if same_entry && same_size {
                return Ok(Upserted { id: row.id, existed: true, untouched: true });
            }
        }
        fold.clear();
        crate::fold::fold_into(record.name, fold);
        let fold = fold.as_str();
        let existing = found.map(|r| (r.id, r.kind, r.dev, r.inode, r.size, r.allocated, r.raw_alloc));
        let (mut old_size, mut old_charge, mut old_count, mut unchanged) = (0, 0, 0, false);
        if let Some((_, kind, dev, inode, size, allocated, raw)) = existing
            && kind != KIND_DIR
        {
            let old_identity = identity_from_sql(dev, inode);
            old_size = size;
            old_charge = allocated;
            old_count = 1;
            unchanged = !is_dir && old_identity == record.id;
            if !unchanged || raw != to_sql_size(record.allocated) {
                self.mark_link(old_identity, Link::Rebuild)?;
            }
        }
        let mut first_charge = 0;
        let mut candidate = false;
        if !is_dir {
            if existing.is_none() && record.nlink <= 1 {
                if self.identity_exists(record.id)? {
                    self.mark_link(record.id, Link::Rebuild)?;
                } else {
                    first_charge = to_sql_size(record.allocated);
                }
            } else if !unchanged {
                candidate = true;
            }
        }
        let (size, count) = if is_dir { (0, 0) } else { (to_sql_size(record.size), 1) };
        deltas.logical += size - old_size;
        deltas.files += count - old_count;
        deltas.allocated += (if unchanged { old_charge } else { first_charge }) - old_charge;
        let id = match existing {
            Some((id, kind, ..)) => {
                let keep_totals = is_dir && kind == KIND_DIR;
                let allocated = if unchanged { old_charge } else { 0 };
                self.conn
                    .prepare_cached(
                        "UPDATE nodes SET search_name=?2,kind=?3,accessible=?4,is_mount=?5,
                           size=CASE WHEN ?6 THEN size ELSE ?7 END, allocated=CASE WHEN ?6 THEN allocated ELSE ?8 END,
                           file_count=CASE WHEN ?6 THEN file_count ELSE ?9 END, raw_alloc=?10,dev=?11,inode=?12,links=?13,epoch=?14
                         WHERE id=?1",
                    )?
                    .execute(params![
                        id,
                        fold,
                        record.kind as i64,
                        traversable,
                        record.is_mount_point,
                        keep_totals,
                        size,
                        allocated,
                        count,
                        if is_dir { 0 } else { to_sql_size(record.allocated) },
                        to_sql_u64(record.id.dev),
                        to_sql_u64(record.id.ino),
                        record.nlink as i64,
                        epoch,
                    ])?;
                id
            }
            None => {
                self.conn.prepare_cached(INSERT_NODE)?.execute(params![
                    parent_id,
                    record.name,
                    fold,
                    record.kind as i64,
                    traversable,
                    record.is_mount_point,
                    size,
                    first_charge,
                    count,
                    if is_dir { 0 } else { to_sql_size(record.allocated) },
                    to_sql_u64(record.id.dev),
                    to_sql_u64(record.id.ino),
                    record.nlink as i64,
                    epoch,
                ])?;
                self.conn.last_insert_rowid()
            }
        };
        if candidate {
            self.mark_link(record.id, Link::Candidate(id))?;
        }
        Ok(Upserted { id, existed: existing.is_some(), untouched: false })
    }

    fn register_directory(&self, path: &[u8], node_id: i64, identity: FileIdentity, fresh: bool) -> Result<()> {
        let (dev, inode) = (to_sql_u64(identity.dev), to_sql_u64(identity.ino));
        if fresh {
            match self
                .conn
                .prepare_cached("INSERT INTO dirs(path,id,dev,inode,pending) VALUES(?1,?2,?3,?4,1)")?
                .execute(params![path, node_id, dev, inode])
            {
                Ok(_) => return Ok(()),
                Err(error) if is_unique_violation(&error) => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            self.conn
                .prepare_cached("INSERT OR IGNORE INTO dirs(path,id,dev,inode,pending) VALUES(?1,?2,?3,?4,1)")?
                .execute(params![path, node_id, dev, inode])?;
        }
        let stored: Option<(i64, i64, i64)> = self
            .conn
            .prepare_cached("SELECT id,dev,inode FROM dirs WHERE path=?1")?
            .query_row([path], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .optional()?;
        match stored {
            Some((id, d, i)) if id == node_id && identity_from_sql(d, i) == identity => Ok(()),
            _ => Err(sqlite_invalid("directory identity changed; subtree reconciliation required")),
        }
    }

    /// Adds deltas to a directory node and all its ancestors.
    fn adjust_ancestors(&self, dir_id: i64, logical: i64, allocated: i64, files: i64) -> Result<()> {
        if logical == 0 && allocated == 0 && files == 0 {
            return Ok(());
        }
        if self.group {
            self.deferred.borrow_mut().push((dir_id, logical, allocated, files));
            return Ok(());
        }
        let changed = self
            .conn
            .prepare_cached(
                "WITH RECURSIVE up(id) AS (SELECT ?4 UNION ALL SELECT n.parent FROM nodes n JOIN up ON n.id=up.id WHERE n.parent IS NOT NULL)
                 UPDATE nodes SET size=size+?1,allocated=allocated+?2,file_count=file_count+?3 WHERE id IN up",
            )?
            .execute(params![logical, allocated, files, dir_id])?;
        if changed == 0 {
            return Err(sqlite_invalid("missing ancestor"));
        }
        Ok(())
    }

    fn record_listing_issues(&self, parent: &[u8], count: u32, issues: &[String]) -> Result<()> {
        self.conn.prepare_cached("DELETE FROM listing_issues WHERE parent=?1")?.execute([parent])?;
        if count > 0 {
            self.conn
                .prepare_cached("INSERT INTO listing_issues(parent,count,messages) VALUES(?1,?2,?3)")?
                .execute(params![parent, count as i64, schema::encode_messages(issues)])?;
        }
        Ok(())
    }

    /// Completes a (non-fresh) listing: schedules bounded trimming of rows from older epochs.
    /// Returns `false` when the directory changed (revision) or a full refresh started since the
    /// listing began; with a watermark, the consumed batches are still acknowledged.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &mut self,
        parent: &[u8],
        cursor: &mut ListingCursor,
        expected_revision: i64,
        expected_generation: Option<i64>,
        issue_count: u32,
        issues: &[String],
        watermark: Option<Watermark>,
    ) -> Result<bool> {
        self.begin()?;
        let result = (|| -> Result<bool> {
            let (generation, pending) = self.full_refresh_state()?;
            if self.dirty_revision(parent)? != expected_revision
                || expected_generation.is_some_and(|g| g != generation || pending)
            {
                return Ok(false);
            }
            let epoch = match cursor.epoch {
                Some(epoch) => epoch,
                None => {
                    let (epoch, id): (i64, i64) = self
                        .conn
                        .prepare_cached("UPDATE dirs SET epoch=epoch+1 WHERE path=?1 RETURNING epoch,id")?
                        .query_row([parent], |row| Ok((row.get(0)?, row.get(1)?)))
                        .optional()?
                        .ok_or_else(|| sqlite_invalid("missing node parent"))?;
                    self.emit("empty-epoch");
                    cursor.epoch = Some(epoch);
                    // An empty listing saw none of the directory's rows.
                    cursor.existing = Some(
                        self.conn
                            .prepare_cached("SELECT COUNT(*) FROM nodes WHERE parent=?1")?
                            .query_row([id], |row| row.get(0))?,
                    );
                    epoch
                }
            };
            self.record_listing_issues(parent, issue_count, issues)?;
            // A relisting that saw every row it started with has nothing stale to trim.
            let complete = cursor.existing.is_some_and(|existing| existing <= cursor.seen);
            if !cursor.fresh && !complete {
                // Rows left untouched still carry an older epoch: stamp them so the trim below
                // removes only what this listing did not see (`epoch` is in no index).
                let mut stamp = self.conn.prepare_cached("UPDATE nodes SET epoch=?1 WHERE id=?2")?;
                for id in &cursor.unstamped {
                    stamp.execute(params![epoch, id])?;
                }
            }
            if cursor.fresh || complete {
                // Every row of a fresh listing carries its epoch, and a complete relisting saw
                // every row: nothing can be stale, so the listing is acknowledged here instead of
                // through a trim marker.
                self.conn
                    .prepare_cached("UPDATE dirty SET pending=0 WHERE parent=?1 AND revision=?2")?
                    .execute(params![parent, expected_revision])?;
                self.conn.prepare_cached("UPDATE dirs SET pending=0 WHERE path=?1")?.execute([parent])?;
            } else {
                self.conn
                    .prepare_cached(
                        "INSERT OR REPLACE INTO trim(parent,epoch,revision,full_generation) VALUES(?1,?2,?3,?4)",
                    )?
                    .execute(params![parent, epoch, expected_revision, expected_generation.unwrap_or(-1)])?;
            }
            Ok(true)
        })();
        match result {
            Ok(true) => {
                self.persist_watermark(watermark).inspect_err(|_| self.rollback())?;
                if let Err(error) = self.commit() {
                    self.rollback();
                    return Err(error);
                }
                self.emit("empty-committed");
                Ok(true)
            }
            Ok(false) => {
                if watermark.is_some() {
                    // The observations are consumed, but the dirty listing must be read again.
                    self.persist_watermark(watermark).inspect_err(|_| self.rollback())?;
                    self.commit().inspect_err(|_| self.rollback())?;
                } else {
                    self.rollback();
                }
                Ok(false)
            }
            Err(error) => {
                self.rollback();
                Err(error)
            }
        }
    }

    // --- Trim, prune, links and refresh -----------------------------------------------------------

    /// Removes up to 128 rows left over from an older epoch of a completed listing (stale
    /// directories become prune markers). Acknowledges the listing when nothing stale remains.
    pub fn trim_one_chunk(&mut self) -> Result<bool> {
        self.flush_ancestors()?;
        let marker: Option<(Vec<u8>, i64, i64, i64)> = self
            .conn
            .prepare_cached("SELECT parent,epoch,revision,full_generation FROM trim ORDER BY parent LIMIT 1")?
            .query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .optional()?;
        let Some((parent, epoch, revision, generation)) = marker else { return Ok(false) };
        let mut event = None;
        self.transaction(|w| {
            let (current_generation, pending) = w.full_refresh_state()?;
            if w.dirty_revision(&parent)? != revision || (generation >= 0 && (pending || current_generation != generation))
            {
                w.conn.prepare_cached("DELETE FROM trim WHERE parent=?1")?.execute([&parent])?;
                return Ok(());
            }
            let parent_id: Option<i64> =
                w.conn.prepare_cached("SELECT id FROM dirs WHERE path=?1")?.query_row([&parent], |row| row.get(0)).optional()?;
            let acknowledge = |w: &Writer| -> Result<()> {
                w.conn.prepare_cached("UPDATE dirty SET pending=0 WHERE parent=?1 AND revision=?2")?.execute(params![
                    &parent, revision
                ])?;
                w.conn.prepare_cached("UPDATE dirs SET pending=0 WHERE path=?1")?.execute([&parent])?;
                w.conn.prepare_cached("DELETE FROM trim WHERE parent=?1")?.execute([&parent])?;
                Ok(())
            };
            let Some(parent_id) = parent_id else {
                acknowledge(w)?;
                return Ok(());
            };
            let stale_dir: Option<Vec<u8>> = w
                .conn
                .prepare_cached("SELECT name FROM nodes WHERE parent=?1 AND epoch<>?2 AND kind=1 LIMIT 1")?
                .query_row([parent_id, epoch], |row| row.get(0))
                .optional()?;
            if let Some(name) = stale_dir {
                let path = crate::paths::join(&parent, &name);
                w.insert_prune(&path, &parent, &name)?;
                return Ok(());
            }
            let (size, allocated, files): (i64, i64, i64) = w
                .conn
                .prepare_cached(
                    "SELECT COALESCE(SUM(size),0),COALESCE(SUM(allocated),0),COUNT(*) FROM (SELECT size,allocated FROM nodes
                     WHERE parent=?1 AND epoch<>?2 AND kind<>1 ORDER BY name LIMIT 128)",
                )?
                .query_row([parent_id, epoch], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            if files == 0 {
                acknowledge(w)?;
                w.emit("trim-empty-written");
                event = Some("trim-empty-committed");
                return Ok(());
            }
            w.conn
                .prepare_cached(
                    "INSERT OR IGNORE INTO dirty_links SELECT dev,inode FROM nodes WHERE id IN (SELECT id FROM nodes
                     WHERE parent=?1 AND epoch<>?2 AND kind<>1 ORDER BY name LIMIT 128)",
                )?
                .execute([parent_id, epoch])?;
            w.conn.prepare_cached("INSERT OR IGNORE INTO link_rebuild SELECT dev,inode FROM dirty_links")?.execute([])?;
            w.adjust_ancestors(parent_id, -size, -allocated, -files)?;
            w.conn
                .prepare_cached(
                    "DELETE FROM nodes WHERE id IN (SELECT id FROM nodes WHERE parent=?1 AND epoch<>?2 AND kind<>1 ORDER BY name LIMIT 128)",
                )?
                .execute([parent_id, epoch])?;
            Ok(())
        })?;
        if let Some(event) = event {
            self.emit(event);
        }
        Ok(true)
    }

    fn insert_prune(&self, path: &[u8], parent: &[u8], name: &[u8]) -> Result<()> {
        self.conn
            .prepare_cached("INSERT OR IGNORE INTO prune(path,parent,name) VALUES(?1,?2,?3)")?
            .execute(params![path, parent, name])?;
        Ok(())
    }

    pub fn start_prune(&mut self, path: &[u8], parent: &[u8], name: &[u8]) -> Result<()> {
        self.transaction(|w| w.insert_prune(path, parent, name))
    }

    /// Removes up to 128 rows of a pruned subtree, deepest parent paths first; then the
    /// directory rows, issues and finally the pruned node itself (or marks it inaccessible when
    /// the directory was blocked).
    pub fn prune_one_chunk(&mut self) -> Result<bool> {
        self.flush_ancestors()?;
        let marker: Option<(Vec<u8>, Vec<u8>, Vec<u8>)> = self
            .conn
            .prepare_cached("SELECT path,parent,name FROM prune ORDER BY path LIMIT 1")?
            .query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .optional()?;
        let Some((path, parent, name)) = marker else { return Ok(false) };
        let (low, high) = descendant_range(&path);
        self.transaction(|w| {
            let rows: Vec<(i64, i64, i64, i64, i64, i64, i64)> = {
                let mut select = w.conn.prepare_cached(
                    "SELECT n.id,n.parent,n.kind,n.size,n.allocated,n.dev,n.inode FROM nodes n JOIN dirs p ON p.id=n.parent
                     WHERE p.path=?1 OR (p.path>=?2 AND p.path<?3) ORDER BY p.path DESC,n.name LIMIT 128",
                )?;
                select
                    .query_map(params![&path, &low, &high], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?
            };
            if !rows.is_empty() {
                let mut deltas: std::collections::HashMap<i64, (i64, i64, i64)> = std::collections::HashMap::new();
                for &(_, parent_id, kind, size, allocated, dev, inode) in &rows {
                    if kind == KIND_DIR {
                        continue;
                    }
                    let entry = deltas.entry(parent_id).or_default();
                    entry.0 += size;
                    entry.1 += allocated;
                    entry.2 += 1;
                    w.mark_link(identity_from_sql(dev, inode), Link::Rebuild)?;
                }
                // Totals first: the parents of deleted rows may be deleted in the same chunk.
                for (parent_id, (size, allocated, files)) in deltas {
                    w.adjust_ancestors(parent_id, -size, -allocated, -files)?;
                }
                let mut delete = w.conn.prepare_cached("DELETE FROM nodes WHERE id=?1")?;
                for (id, ..) in &rows {
                    delete.execute([id])?;
                }
                return Ok(());
            }
            let removed_dirs = w
                .conn
                .prepare_cached(
                    "DELETE FROM dirs WHERE path IN (SELECT path FROM dirs WHERE path=?1 OR (path>=?2 AND path<?3) LIMIT 128)",
                )?
                .execute(params![&path, &low, &high])?;
            if removed_dirs > 0 {
                return Ok(());
            }
            let issues = w
                .conn
                .prepare_cached("DELETE FROM stage_issues WHERE path IN (SELECT path FROM stage_issues WHERE path>=?1 AND path<?2 LIMIT 128)")?
                .execute([&low, &high])?;
            let listing = w
                .conn
                .prepare_cached(
                    "DELETE FROM listing_issues WHERE parent IN (SELECT parent FROM listing_issues WHERE parent=?1 OR (parent>=?2 AND parent<?3) LIMIT 128)",
                )?
                .execute(params![&path, &low, &high])?;
            let blocked = w
                .conn
                .prepare_cached(
                    "DELETE FROM blocked_directories WHERE path IN (SELECT path FROM blocked_directories WHERE path>=?1 AND path<?2 LIMIT 128)",
                )?
                .execute([&low, &high])?;
            if issues > 0 || listing > 0 || blocked > 0 {
                return Ok(());
            }
            let parent_id: Option<i64> =
                w.conn.prepare_cached("SELECT id FROM dirs WHERE path=?1")?.query_row([&parent], |row| row.get(0)).optional()?;
            let is_blocked = w
                .conn
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM blocked_directories WHERE path=?1)")?
                .query_row([&path], |row| row.get::<_, bool>(0))?;
            if let Some(parent_id) = parent_id {
                let node: Option<(i64, i64, i64, i64, i64)> = w
                    .conn
                    .prepare_cached("SELECT id,kind,size,allocated,file_count FROM nodes WHERE parent=?1 AND name=?2")?
                    .query_row(params![parent_id, &name], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
                    })
                    .optional()?;
                if let Some((id, kind, size, allocated, files)) = node {
                    // A directory's own totals are already zero once its rows are gone; a file
                    // (never expected here) keeps the totals consistent anyway.
                    w.adjust_ancestors(parent_id, -size, -allocated, -files)?;
                    if is_blocked && kind == KIND_DIR {
                        w.conn
                            .prepare_cached("UPDATE nodes SET accessible=0,size=0,allocated=0,file_count=0 WHERE id=?1")?
                            .execute([id])?;
                    } else {
                        if kind != KIND_DIR {
                            w.mark_link(
                                w.conn.query_row("SELECT dev,inode FROM nodes WHERE id=?1", [id], |row| {
                                    Ok(identity_from_sql(row.get(0)?, row.get(1)?))
                                })?,
                                Link::Rebuild,
                            )?;
                        }
                        w.conn.prepare_cached("DELETE FROM nodes WHERE id=?1")?.execute([id])?;
                    }
                }
            }
            if is_blocked {
                w.conn.prepare_cached("DELETE FROM blocked_directories WHERE path=?1")?.execute([&path])?;
            } else {
                w.conn.prepare_cached("DELETE FROM stage_issues WHERE path=?1")?.execute([&path])?;
            }
            w.conn.prepare_cached("DELETE FROM prune WHERE path=?1")?.execute([&path])?;
            Ok(())
        })?;
        Ok(true)
    }

    /// Recomputes allocation ownership for up to 128 dirty identities: the owner is the path
    /// that sorts first by [`crate::hardlinks::precedes`]; every other link is charged zero.
    pub fn reconcile_links_chunk(&mut self, cancel: Option<&Cancellation>) -> Result<bool> {
        let any =
            self.conn.prepare_cached("SELECT 1 FROM dirty_links LIMIT 1")?.query_row([], |_| Ok(())).optional()?;
        if any.is_none() {
            return Ok(false);
        }
        self.transaction(|w| {
            let identities: Vec<(i64, i64)> = w
                .conn
                .prepare_cached("SELECT dev,inode FROM dirty_links ORDER BY dev,inode LIMIT 128")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (dev, inode) in identities {
                w.reconcile_identity(dev, inode, cancel)?;
            }
            Ok(())
        })?;
        Ok(true)
    }

    fn reconcile_identity(&self, dev: i64, inode: i64, cancel: Option<&Cancellation>) -> Result<()> {
        struct Link {
            id: i64,
            parent: i64,
            path: Vec<u8>,
            allocated: i64,
            raw: i64,
        }
        let read = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Link> {
            let dir: Vec<u8> = row.get(1)?;
            let name: Vec<u8> = row.get(2)?;
            Ok(Link {
                id: row.get(0)?,
                parent: row.get(3)?,
                path: crate::paths::join(&dir, &name),
                allocated: row.get(4)?,
                raw: row.get(5)?,
            })
        };
        let owner: Option<Link> = self
            .conn
            .prepare_cached(
                "SELECT n.id,p.path,n.name,n.parent,n.allocated,n.raw_alloc FROM owners o JOIN nodes n ON n.id=o.node_id
                 AND n.dev=o.dev AND n.inode=o.inode AND n.kind<>1 JOIN dirs p ON p.id=n.parent WHERE o.dev=?1 AND o.inode=?2",
            )?
            .query_row([dev, inode], read)
            .optional()?;
        let rebuild = self
            .conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM link_rebuild WHERE dev=?1 AND inode=?2)")?
            .query_row([dev, inode], |row| row.get::<_, bool>(0))?;
        let candidates: Vec<Link> = self
            .conn
            .prepare_cached(
                "SELECT n.id,p.path,n.name,n.parent,n.allocated,n.raw_alloc FROM link_candidates c JOIN nodes n ON n.id=c.node_id
                 AND n.dev=c.dev AND n.inode=c.inode AND n.kind<>1 JOIN dirs p ON p.id=n.parent WHERE c.dev=?1 AND c.inode=?2",
            )?
            .query_map([dev, inode], read)?
            .collect::<rusqlite::Result<_>>()?;
        let full_scan = rebuild || owner.is_none() || candidates.is_empty();
        let links: Vec<Link> = if full_scan {
            self.conn
                .prepare_cached(
                    "SELECT n.id,p.path,n.name,n.parent,n.allocated,n.raw_alloc FROM nodes n JOIN dirs p ON p.id=n.parent
                     WHERE n.dev=?1 AND n.inode=?2 AND n.kind<>1",
                )?
                .query_map([dev, inode], read)?
                .collect::<rusqlite::Result<_>>()?
        } else {
            owner.into_iter().chain(candidates).collect()
        };
        let mut winner: Option<usize> = None;
        for (index, link) in links.iter().enumerate() {
            if index & 255 == 0 && cancel.is_some_and(Cancellation::is_cancelled) {
                return Err(CoreError::Cancelled);
            }
            if winner.is_none_or(|w| link_order::precedes(&link.path, &links[w].path)) {
                winner = Some(index);
            }
        }
        for (index, link) in links.iter().enumerate() {
            let desired = if Some(index) == winner { link.raw } else { 0 };
            if link.allocated != desired {
                self.conn.prepare_cached("UPDATE nodes SET allocated=?2 WHERE id=?1")?.execute([link.id, desired])?;
                self.adjust_ancestors(link.parent, 0, desired - link.allocated, 0)?;
            }
        }
        match winner {
            Some(index) => {
                self.conn.prepare_cached("INSERT OR REPLACE INTO owners VALUES(?1,?2,?3)")?.execute([
                    dev,
                    inode,
                    links[index].id,
                ])?;
            }
            None => {
                self.conn.prepare_cached("DELETE FROM owners WHERE dev=?1 AND inode=?2")?.execute([dev, inode])?;
            }
        }
        for sql in [
            "DELETE FROM link_candidates WHERE dev=?1 AND inode=?2",
            "DELETE FROM link_rebuild WHERE dev=?1 AND inode=?2",
            "DELETE FROM dirty_links WHERE dev=?1 AND inode=?2",
        ] {
            self.conn.prepare_cached(sql)?.execute([dev, inode])?;
        }
        Ok(())
    }

    /// Starts a new full-refresh generation (every directory must be listed again). Runs inside
    /// the caller's transaction when there is one.
    pub fn mark_full_refresh(&mut self) -> Result<()> {
        self.conn.execute("UPDATE full_refresh SET generation=generation+1,pending=1 WHERE singleton=1", [])?;
        Ok(())
    }

    /// Requeues up to 128 directories of the pending refresh generation, advancing a keyset
    /// cursor over the directory primary key. Returns `false` once nothing is pending.
    pub fn mark_full_refresh_chunk(&mut self) -> Result<bool> {
        if !self.full_refresh_state()?.1 {
            return Ok(false);
        }
        let mut last = None;
        let mut progressed = false;
        let cursor = self.full_refresh_cursor.clone();
        self.transaction(|w| {
            let (generation, pending) = w.full_refresh_state()?;
            if !pending {
                return Ok(());
            }
            progressed = true;
            let previous = match &cursor {
                Some((g, path)) if *g == generation => path.clone(),
                _ => vec![0u8],
            };
            // One keyset range per chunk: find the 128th unmarked path, then update the range
            // (a range scan instead of 128 primary-key descents).
            let marked: Option<Vec<u8>> = w
                .conn
                .prepare_cached(
                    "SELECT MAX(path) FROM (SELECT path FROM dirs WHERE path>?1 AND full_generation<?2 ORDER BY path LIMIT 128)",
                )?
                .query_row(params![previous, generation], |row| row.get(0))?;
            match marked {
                Some(path) => {
                    w.conn
                        .prepare_cached(
                            "UPDATE dirs SET pending=1,full_generation=?2 WHERE path>?1 AND path<=?3 AND full_generation<?2",
                        )?
                        .execute(params![previous, generation, path])?;
                    last = Some((generation, path));
                }
                None => {
                    w.conn
                        .prepare_cached("UPDATE full_refresh SET pending=0 WHERE singleton=1 AND generation=?1")?
                        .execute([generation])?;
                }
            }
            Ok(())
        })?;
        // Reopening scans the committed prefix once; later chunks seek past it.
        self.full_refresh_cursor = last;
        Ok(progressed)
    }

    /// Queues `parent` for relisting with a new revision (a change was reported inside it).
    pub fn mark_dirty(&mut self, parent: &[u8]) -> Result<()> {
        self.transaction(|w| {
            if w.directory(parent)?.is_none() {
                return Err(sqlite_invalid("unknown changed directory"));
            }
            w.bump_dirty(parent)
        })
    }

    fn bump_dirty(&self, parent: &[u8]) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO dirty(parent,revision,pending) VALUES(?1,1,1) ON CONFLICT(parent) DO UPDATE SET revision=revision+1,pending=1",
            )?
            .execute([parent])?;
        self.conn.prepare_cached("UPDATE dirs SET pending=1 WHERE path=?1")?.execute([parent])?;
        Ok(())
    }

    /// Swift `DurableCrawler.markChanged`: marks the nearest registered directory (and its
    /// parent) dirty; a path outside the root starts a full refresh.
    pub fn mark_changed(&mut self, directory: &[u8]) -> Result<()> {
        let Some(path) = self.nearest_registered(directory)? else {
            return self.transaction(|w| w.mark_full_refresh());
        };
        if path != self.root
            && let Some(parent) = crate::paths::parent(&path)
            && self.directory(parent)?.is_some()
        {
            self.mark_dirty(parent)?;
        }
        self.mark_dirty(&path)
    }

    /// Queues the listing of `directory` (or of its nearest registered ancestor) for repetition
    /// with a new revision, and nothing else: its subfolders keep their listings. Relisting
    /// reconciles its entries; a removed or replaced subfolder is pruned, a new one registered and
    /// listed in full. A path outside the root starts a full refresh.
    pub fn mark_listing_changed(&mut self, directory: &[u8]) -> Result<()> {
        match self.nearest_registered(directory)? {
            Some(path) => self.mark_dirty(&path),
            None => self.transaction(|w| w.mark_full_refresh()),
        }
    }

    /// `directory` or its nearest registered ancestor; `None` when it lies outside the root.
    fn nearest_registered(&self, directory: &[u8]) -> Result<Option<Vec<u8>>> {
        if !crate::paths::is_within(directory, &self.root) {
            return Ok(None);
        }
        let mut path = directory.to_vec();
        while self.directory(&path)?.is_none() {
            if path == self.root {
                return Err(sqlite_invalid("invalid changed directory"));
            }
            path = crate::paths::parent(&path).ok_or_else(|| sqlite_invalid("invalid changed directory"))?.to_vec();
        }
        Ok(Some(path))
    }

    /// Queues `directory` and every registered directory below it for relisting with new
    /// revisions (a change monitor asked for a recursive reconciliation; Swift replaced the
    /// whole subtree with a fresh scan). Every listing in the subtree is repeated, so this costs
    /// in proportion to the subtree; prefer [`Writer::mark_listing_changed`] when the changed
    /// folder is known. The root starts a full refresh instead.
    pub fn mark_subtree_changed(&mut self, directory: &[u8]) -> Result<()> {
        if directory == self.root.as_slice() || !crate::paths::is_within(directory, &self.root) {
            return self.transaction(|w| w.mark_full_refresh());
        }
        if self.directory(directory)?.is_none() {
            return self.mark_changed(directory);
        }
        let (low, high) = descendant_range(directory);
        self.transaction(|w| {
            w.bump_dirty(directory)?;
            w.conn
                .prepare_cached(
                    "INSERT INTO dirty(parent,revision,pending) SELECT path,1,1 FROM dirs WHERE path>=?1 AND path<?2
                     ON CONFLICT(parent) DO UPDATE SET revision=revision+1,pending=1",
                )?
                .execute(params![low, high])?;
            w.conn
                .prepare_cached("UPDATE dirs SET pending=1 WHERE path>=?1 AND path<?2")?
                .execute(params![low, high])?;
            Ok(())
        })
    }

    /// Finishes a repair of a saved-map working copy: a new snapshot id and scan time, the
    /// change-history position the repair covers and the removal-journal cursor, atomically.
    pub fn finish_repair(
        &mut self,
        event_identity: &str,
        event_id: u64,
        elapsed: f64,
        removal_cursor: Option<(&str, i64)>,
    ) -> Result<schema::SnapshotInfo> {
        let path = self.path.clone();
        self.transaction(|w| {
            if w.has_remaining_work()? {
                return Err(sqlite_invalid("the repair has pending work"));
            }
            let id = uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase();
            let changed = w.conn.execute(
                "UPDATE metadata SET snapshot_id=?1,scanned_at=?2,elapsed=?3,event_id=?4,event_identity=?5 WHERE singleton=1",
                params![id, unix_now(), elapsed.max(0.0), to_sql_u64(event_id), event_identity],
            )?;
            if changed != 1 {
                return Err(sqlite_invalid("the saved map has no metadata"));
            }
            if let Some((identity, sequence)) = removal_cursor {
                write_removal_cursor(&w.conn, identity, sequence)?;
            }
            schema::read_snapshot(&w.conn, &path)
        })
    }

    /// Starts counting, on this connection only, the inventory changes a repair makes: inserted
    /// and deleted items and any change of an item other than its listing epoch (name, kind,
    /// sizes, counts, identity, links). Queue bookkeeping is not counted. Read the count with
    /// [`Writer::inventory_changes`]; the temporary triggers never reach the database file.
    pub fn track_inventory_changes(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS orbit_inventory_changes(singleton INTEGER PRIMARY KEY, n INTEGER NOT NULL);
             INSERT OR REPLACE INTO temp.orbit_inventory_changes VALUES(1,0);
             CREATE TEMP TRIGGER IF NOT EXISTS orbit_node_inserted AFTER INSERT ON main.nodes BEGIN
               UPDATE orbit_inventory_changes SET n=n+1;
             END;
             CREATE TEMP TRIGGER IF NOT EXISTS orbit_node_deleted AFTER DELETE ON main.nodes BEGIN
               UPDATE orbit_inventory_changes SET n=n+1;
             END;
             CREATE TEMP TRIGGER IF NOT EXISTS orbit_node_changed AFTER UPDATE ON main.nodes
               WHEN OLD.parent IS NOT NEW.parent OR OLD.name IS NOT NEW.name OR OLD.search_name IS NOT NEW.search_name
                 OR OLD.kind IS NOT NEW.kind OR OLD.accessible IS NOT NEW.accessible OR OLD.is_mount IS NOT NEW.is_mount
                 OR OLD.size IS NOT NEW.size OR OLD.allocated IS NOT NEW.allocated
                 OR OLD.file_count IS NOT NEW.file_count OR OLD.raw_alloc IS NOT NEW.raw_alloc
                 OR OLD.dev IS NOT NEW.dev OR OLD.inode IS NOT NEW.inode OR OLD.links IS NOT NEW.links BEGIN
               UPDATE orbit_inventory_changes SET n=n+1;
             END;",
        )?;
        Ok(())
    }

    /// Inventory changes counted since [`Writer::track_inventory_changes`].
    pub fn inventory_changes(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT n FROM temp.orbit_inventory_changes WHERE singleton=1", [], |row| row.get(0))?)
    }

    /// A hash of everything the map reports as skipped (listing issues, skipped items, blocked
    /// folders), to tell whether a repair changed it.
    pub fn issue_fingerprint(&self) -> Result<u64> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for sql in [
            "SELECT parent,count,messages FROM listing_issues ORDER BY parent",
            "SELECT path,0,message FROM stage_issues ORDER BY path",
            "SELECT path,0,NULL FROM blocked_directories ORDER BY path",
        ] {
            let mut statement = self.conn.prepare_cached(sql)?;
            let mut rows = statement.query([])?;
            let mut count = 0u64;
            while let Some(row) = rows.next()? {
                count += 1;
                row.get_ref(0)?.as_bytes_or_null().unwrap_or_default().hash(&mut hasher);
                row.get::<_, i64>(1)?.hash(&mut hasher);
                row.get_ref(2)?.as_bytes_or_null().unwrap_or_default().hash(&mut hasher);
            }
            count.hash(&mut hasher);
        }
        Ok(hasher.finish())
    }

    /// A queued directory vanished or was replaced: prune it and relist its parent.
    pub fn invalidate_directory(&mut self, path: &[u8]) -> Result<()> {
        let (parent, name) = split_parent(path).ok_or_else(|| sqlite_invalid("invalid directory"))?;
        self.transaction(|w| {
            if w.directory(parent)?.is_none() {
                return Err(sqlite_invalid("missing parent for retry"));
            }
            w.insert_prune(path, parent, name)?;
            w.bump_dirty(parent)
        })
    }

    /// A queued directory cannot be read (permissions): prune its rows and keep it as an
    /// inaccessible, empty node with an issue.
    pub fn block_directory(&mut self, path: &[u8], message: &str) -> Result<()> {
        let (parent, name) = split_parent(path).ok_or_else(|| sqlite_invalid("invalid directory"))?;
        self.transaction(|w| {
            w.conn.prepare_cached("INSERT OR REPLACE INTO blocked_directories(path) VALUES(?1)")?.execute([path])?;
            w.conn
                .prepare_cached("INSERT OR REPLACE INTO stage_issues(path,message) VALUES(?1,?2)")?
                .execute(params![path, message])?;
            w.insert_prune(path, parent, name)
        })
    }

    // --- Removal-journal support ----------------------------------------------------------------

    /// The removal-journal cursor `(identity, sequence)` stored with the stage.
    pub fn removal_cursor(&self) -> Result<Option<(String, i64)>> {
        read_removal_cursor(&self.conn)
    }

    pub fn write_removal_cursor(&mut self, identity: &str, sequence: i64) -> Result<()> {
        self.transaction(|w| write_removal_cursor(&w.conn, identity, sequence))
    }

    /// Disk-backed set of parents a removal replay must reconcile.
    pub fn reset_replay_parents(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS replay_parents(path BLOB PRIMARY KEY) WITHOUT ROWID; DELETE FROM replay_parents",
        )?;
        Ok(())
    }

    pub fn add_replay_parents(&mut self, paths: &[Vec<u8>]) -> Result<()> {
        self.transaction(|w| {
            let mut insert = w.conn.prepare_cached("INSERT OR IGNORE INTO replay_parents VALUES(?1)")?;
            for path in paths {
                insert.execute([path])?;
            }
            Ok(())
        })
    }

    pub fn replay_parents(&self, after: Option<&[u8]>, limit: usize) -> Result<Vec<Vec<u8>>> {
        let after = after.unwrap_or(&[]);
        let mut statement =
            self.conn.prepare_cached("SELECT path FROM replay_parents WHERE path>?1 ORDER BY path LIMIT ?2")?;
        Ok(statement.query_map(params![after, limit as i64], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?)
    }

    // --- Diagnostics ----------------------------------------------------------------------------

    /// `(logical, allocated, files)` of a registered directory.
    pub fn folder_totals(&self, path: &[u8]) -> Result<Option<(i64, i64, i64)>> {
        self.flush_ancestors()?;
        Ok(self
            .conn
            .query_row(
                "SELECT n.size,n.allocated,n.file_count FROM dirs d JOIN nodes n ON n.id=d.id WHERE d.path=?1",
                [path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    }

    /// Sum over all file rows (ignores directory totals).
    pub fn file_totals(&self) -> Result<(i64, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(size),0),COALESCE(SUM(allocated),0),COUNT(*) FROM nodes WHERE kind<>1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    }

    /// Charged allocation of the file at `path` (`0` for directories).
    pub fn charge(&self, path: &[u8]) -> Result<Option<i64>> {
        let (parent, name) = split_parent(path).ok_or_else(|| sqlite_invalid("invalid charge path"))?;
        Ok(self
            .conn
            .query_row(
                "SELECT CASE WHEN n.kind=1 THEN 0 ELSE n.allocated END FROM nodes n JOIN dirs d ON d.id=n.parent
                 WHERE d.path=?1 AND n.name=?2",
                params![parent, name],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Number of rows directly inside `parent`.
    pub fn count(&self, parent: &[u8]) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE parent=(SELECT id FROM dirs WHERE path=?1)",
            [parent],
            |row| row.get(0),
        )?)
    }
}

pub(crate) fn unix_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub(crate) fn read_watermark(conn: &Connection) -> Result<Option<Watermark>> {
    let row: Option<(Vec<u8>, i64)> = conn
        .prepare_cached("SELECT generation,sequence FROM orbit_async_progress WHERE singleton=1")?
        .query_row([], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    match row {
        None => Ok(None),
        Some((generation, sequence)) => {
            let generation = uuid::Uuid::from_slice(&generation)
                .map_err(|_| CoreError::Invalid("The live mapping checkpoint is damaged.".into()))?;
            if sequence < 0 {
                return Err(CoreError::Invalid("The live mapping checkpoint is damaged.".into()));
            }
            Ok(Some(Watermark { generation, sequence: sequence as u64 }))
        }
    }
}

pub(crate) fn read_removal_cursor(conn: &Connection) -> Result<Option<(String, i64)>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='orbit_removal_cursor')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let (identity, sequence): (String, i64) = conn
        .query_row("SELECT identity,sequence FROM orbit_removal_cursor WHERE singleton=1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?
        .ok_or_else(|| CoreError::Invalid("The map's removal checkpoint is damaged.".into()))?;
    if uuid::Uuid::parse_str(&identity).is_err() || sequence < 0 {
        return Err(CoreError::Invalid("The map's removal checkpoint is damaged.".into()));
    }
    Ok(Some((identity, sequence)))
}

pub(crate) fn write_removal_cursor(conn: &Connection, identity: &str, sequence: i64) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS orbit_removal_cursor(singleton INTEGER PRIMARY KEY CHECK(singleton=1),identity TEXT NOT NULL,sequence INTEGER NOT NULL)",
        [],
    )?;
    conn.execute("INSERT OR REPLACE INTO orbit_removal_cursor VALUES(1,?1,?2)", params![identity, sequence])?;
    Ok(())
}

/// SQL of statements on `conn` that are mid-step and read-only (they would block the WAL
/// autocheckpoint at `COMMIT`). Diagnostics for tests.
pub fn busy_readers(conn: &Connection) -> Vec<String> {
    let mut readers = Vec::new();
    // SAFETY: the handle stays valid while `conn` is borrowed; sqlite3_next_stmt/stmt_busy/
    // stmt_readonly/sql only read statement state owned by this connection.
    unsafe {
        let db = conn.handle();
        let mut statement = rusqlite::ffi::sqlite3_next_stmt(db, std::ptr::null_mut());
        while !statement.is_null() {
            if rusqlite::ffi::sqlite3_stmt_busy(statement) != 0 && rusqlite::ffi::sqlite3_stmt_readonly(statement) != 0
            {
                let sql = rusqlite::ffi::sqlite3_sql(statement);
                if !sql.is_null() {
                    readers.push(std::ffi::CStr::from_ptr(sql).to_string_lossy().into_owned());
                }
            }
            statement = rusqlite::ffi::sqlite3_next_stmt(db, statement);
        }
    }
    readers
}
