//! The cache-wide removal journal: every Trash/delete intent is committed here *before* the
//! filesystem changes, so a crash at any point leaves enough information to reconcile saved maps
//! and mapping stages.
//!
//! Ported from `Sources/OrbitCore/RemovalJournal.swift`. Layout under the caller's durable
//! directory (`AppDirs::support`-derived lock directory):
//!
//! ```text
//! <parent>/.orbit-removals/          mode 0700, owned by the user, opened O_NOFOLLOW
//!     journal.lock                   the fence: exclusive flock for writers, shared for readers
//!     journal.sqlite (+ -wal/-shm)   WAL, synchronous=FULL, see `schema::DDL`
//!     <UUID>.operation               one per operation, exclusively flocked while its owner lives
//! ```
//!
//! Invariants:
//! - An operation row and all its items are inserted in one `BEGIN IMMEDIATE` transaction while
//!   the operation's lock file is already held, so a visible row always has a lock to probe.
//! - Liveness = the `.operation` file is still flocked. A crashed owner releases it, which turns
//!   the operation into an *abandoned* intent that recovery replays. A live operation is never
//!   treated as abandoned; probing never recreates a missing lock (fails closed).
//! - The journal is never compacted. `metadata.identity` (a UUID) changes only when the database
//!   is recreated, which invalidates every saved cursor (`JournalChanged` → full reconciliation).
//! - Readers capture `high_water = MAX(seq)` under the fence and never consume beyond it. A
//!   repaired snapshot may be published only under the fence while `high_water` still equals the
//!   cursor it carries (see [`Fence::high_water`]).
//! - Every file is re-validated (same inode, regular, 0600, one link, owned by us) around each use.

#![cfg(unix)]

pub mod canonical;
mod delete;
mod run;
pub mod schema;
pub(crate) mod sys;

#[cfg(test)]
pub(crate) mod tests;

pub use canonical::{CanonicalAliases, RootBinding, is_standard_path};
pub use delete::remove_no_follow;
pub use run::{
    FailureReason, NoFollowDelete, OVERLAY_LIMIT, PARENT_LIMIT, RemovalExecutor, RemovalFailure, RemovalOutcome,
    RemovalRun, TrashExecutor,
};

use crate::paths;
use crate::types::{CoreError, ScanRemoval};
use canonical::ParentIdentity;
use orbit_platform::posix::{join, path_from_bytes};
use orbit_platform::{Cancellation, FileIdentity, PlatformError};
use parking_lot::Mutex;
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter};
use std::cell::Cell;
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Name of the journal directory inside the caller's durable lock directory.
pub const DIRECTORY_NAME: &str = ".orbit-removals";
const FENCE_NAME: &[u8] = b"journal.lock";
const DB_NAME: &[u8] = b"journal.sqlite";
const OPERATION_SUFFIX: &str = ".operation";
/// Largest page any journal query accepts.
pub const MAX_PAGE: usize = 512;
/// Poll interval while another process holds the fence.
const LOCK_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// A journal file or directory is not what it must be (symlink, replaced, wrong owner/mode).
    #[error("the removal journal is unsafe: {0}")]
    Unsafe(String),
    #[error("invalid removal journal request: {0}")]
    Invalid(String),
    /// The database has a format this build does not understand. Nothing was changed.
    #[error("incompatible removal journal: {0}")]
    Incompatible(String),
    /// Read-only access found the fence held and does not wait.
    #[error("the removal journal is busy")]
    Busy,
    /// The journal identity, captured range or recovery root binding changed. Callers restart
    /// from a fresh cursor and treat the result as requiring full reconciliation.
    #[error("the removal journal changed")]
    JournalChanged,
    /// New operations were appended after a recovery plan was captured; publish was refused.
    #[error("the removal journal changed during reconciliation")]
    ChangedDuringReconciliation,
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Platform(#[from] PlatformError),
    #[error(transparent)]
    Io(#[from] io::Error),
    /// An error from a [`crate::recovery`] target (store, stage) while applying a plan.
    #[error(transparent)]
    Target(#[from] CoreError),
}

impl JournalError {
    pub fn is_cancelled(&self) -> bool {
        match self {
            Self::Cancelled | Self::Platform(PlatformError::Cancelled) => true,
            Self::Target(error) => error.is_cancelled(),
            _ => false,
        }
    }
}

impl From<JournalError> for CoreError {
    fn from(error: JournalError) -> Self {
        match error {
            JournalError::Cancelled => CoreError::Cancelled,
            JournalError::Sqlite(e) => CoreError::Sqlite(e),
            JournalError::Platform(e) => CoreError::Platform(e),
            JournalError::Io(e) => CoreError::Io(e),
            JournalError::Target(e) => e,
            JournalError::Busy => CoreError::InUse("the removal journal".into()),
            JournalError::JournalChanged | JournalError::ChangedDuringReconciliation => CoreError::SnapshotChanged,
            other => CoreError::Invalid(other.to_string()),
        }
    }
}

pub type JournalResult<T> = Result<T, JournalError>;

/// A position in one journal: everything up to and including `sequence` has been applied.
/// Stored with saved maps and stages (`orbit_removal_cursor`), compatible with Swift's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RemovalCursor {
    /// `metadata.identity` of the journal (a UUID string).
    pub identity: String,
    pub sequence: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Access {
    /// Creates the directory and files as needed, waits for the fence (exclusive).
    #[default]
    ReadWrite,
    /// Never creates anything, takes the fence shared, and fails with [`JournalError::Busy`]
    /// instead of waiting. Requires a current-format journal.
    ReadOnly,
}

#[derive(Clone, Debug, Default)]
pub struct JournalOptions {
    pub access: Access,
    pub aliases: CanonicalAliases,
}

/// Read-only fences serialize within the process: concurrent read-only SQLite opens can collide
/// while setting up WAL shared memory. Writers never take this.
static READ_FENCE: Mutex<()> = Mutex::new(());

pub struct Journal {
    directory: PathBuf,
    dir_fd: OwnedFd,
    dir_identity: FileIdentity,
    access: Access,
    aliases: CanonicalAliases,
    #[cfg(test)]
    pub(crate) hooks: Hooks,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal").field("directory", &self.directory).field("access", &self.access).finish()
    }
}

impl Journal {
    /// Opens (creating when read-write) `<parent>/.orbit-removals`.
    pub fn open(parent: &Path, options: JournalOptions) -> JournalResult<Self> {
        let unsafe_err = |what: &str| JournalError::Unsafe(what.to_string());
        let write = options.access == Access::ReadWrite;
        if write {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
        }
        let (parent_fd, _) = sys::open_dir(parent).map_err(|_| unsafe_err("journal parent"))?;
        let name = DIRECTORY_NAME.as_bytes();
        let mut created = false;
        if write {
            match sys::mkdir_at(parent_fd.as_fd(), name, sys::DIR_MODE) {
                Ok(()) => created = true,
                Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {}
                Err(_) => return Err(unsafe_err("journal directory")),
            }
            sys::fsync(parent_fd.as_fd()).map_err(|_| unsafe_err("sync journal parent"))?;
        }
        let dir_fd = sys::open_at(parent_fd.as_fd(), name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
            .map_err(|_| unsafe_err("open journal directory"))?;
        if created {
            // The umask may have narrowed the mode; the journal requires exactly 0700.
            sys::fchmod(dir_fd.as_fd(), sys::DIR_MODE).map_err(|_| unsafe_err("journal directory mode"))?;
        }
        let info = sys::fstat(dir_fd.as_fd()).map_err(|_| unsafe_err("journal directory identity"))?;
        if !info.is_dir() || !info.is_private(sys::DIR_MODE) {
            return Err(unsafe_err("journal directory identity"));
        }
        let journal = Self {
            directory: parent.join(DIRECTORY_NAME),
            dir_fd,
            dir_identity: info.identity,
            access: options.access,
            aliases: options.aliases,
            #[cfg(test)]
            hooks: Hooks::default(),
        };
        journal.validate_directory()?;
        Ok(journal)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn aliases(&self) -> &CanonicalAliases {
        &self.aliases
    }

    fn validate_directory(&self) -> JournalResult<()> {
        let replaced = || JournalError::Unsafe("journal directory replaced".into());
        let held = sys::fstat(self.dir_fd.as_fd()).map_err(|_| replaced())?;
        let actual = sys::lstat(&self.directory).map_err(|_| replaced())?;
        if !actual.is_dir()
            || !actual.is_private(sys::DIR_MODE)
            || actual.identity != self.dir_identity
            || held.identity != self.dir_identity
        {
            return Err(replaced());
        }
        Ok(())
    }

    /// Opens `name` in the journal directory with full ownership/identity checks.
    fn checked_file(&self, name: &[u8], flags: libc::c_int, create: bool) -> JournalResult<(OwnedFd, FileIdentity)> {
        self.validate_directory()?;
        let label = String::from_utf8_lossy(name).into_owned();
        let write = self.access == Access::ReadWrite;
        let create = create && write;
        let flags = if write { flags } else { flags & !libc::O_CREAT & !libc::O_EXCL };
        let dir = self.dir_fd.as_fd();
        let (fd, created) = if create && flags & libc::O_EXCL == 0 {
            match sys::open_at(dir, name, flags | libc::O_CREAT | libc::O_EXCL, sys::FILE_MODE) {
                Ok(fd) => (Ok(fd), true),
                Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {
                    (sys::open_at(dir, name, flags & !libc::O_CREAT, sys::FILE_MODE), false)
                }
                Err(e) => (Err(e), false),
            }
        } else {
            let fd = sys::open_at(dir, name, flags, sys::FILE_MODE);
            let created = create && fd.is_ok();
            (fd, created)
        };
        let fd = fd.map_err(|_| JournalError::Unsafe(format!("open {label}")))?;
        if created {
            sys::fchmod(fd.as_fd(), sys::FILE_MODE).map_err(|_| JournalError::Unsafe(format!("mode {label}")))?;
        }
        let unsafe_file = || JournalError::Unsafe(format!("unsafe {label}"));
        let held = sys::fstat(fd.as_fd()).map_err(|_| unsafe_file())?;
        let actual = sys::stat_at(dir, name).map_err(|_| unsafe_file())?;
        if !held.is_file() || !held.is_private(sys::FILE_MODE) || held.identity != actual.identity {
            return Err(unsafe_file());
        }
        if created {
            sys::fsync(dir).map_err(|_| JournalError::Unsafe("sync journal directory".into()))?;
        }
        Ok((fd, held.identity))
    }

    fn validate_file(&self, name: &[u8], fd: &OwnedFd, expected: FileIdentity) -> JournalResult<()> {
        self.validate_directory()?;
        let replaced = || JournalError::Unsafe("journal file replaced".into());
        let held = sys::fstat(fd.as_fd()).map_err(|_| replaced())?;
        let actual = sys::stat_at(self.dir_fd.as_fd(), name).map_err(|_| replaced())?;
        if !actual.is_file()
            || !actual.is_private(sys::FILE_MODE)
            || held.identity != expected
            || actual.identity != expected
        {
            return Err(replaced());
        }
        Ok(())
    }

    fn check_sidecars(&self) -> JournalResult<()> {
        for suffix in [&b"-wal"[..], b"-shm", b"-journal"] {
            let mut name = DB_NAME.to_vec();
            name.extend_from_slice(suffix);
            match sys::stat_at(self.dir_fd.as_fd(), &name) {
                Ok(info) if info.is_file() && info.is_private(sys::FILE_MODE) => {}
                Ok(_) => return Err(JournalError::Unsafe("journal sidecar identity".into())),
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {}
                Err(_) => return Err(JournalError::Unsafe("journal sidecar".into())),
            }
        }
        Ok(())
    }

    fn wait_lock(&self, fd: &OwnedFd, cancel: &Cancellation, shared: bool) -> JournalResult<()> {
        let path = self.directory.join("journal.lock");
        while !sys::try_lock(fd.as_fd(), shared, &path).map_err(|_| JournalError::Unsafe("journal lock".into()))? {
            if cancel.is_cancelled() {
                return Err(JournalError::Cancelled);
            }
            if self.access == Access::ReadOnly {
                return Err(JournalError::Busy);
            }
            #[cfg(test)]
            Hooks::run(&self.hooks.fence_busy);
            std::thread::sleep(LOCK_POLL);
        }
        if cancel.is_cancelled() {
            return Err(JournalError::Cancelled);
        }
        Ok(())
    }

    /// Runs `body` while holding the fence. Waiting for the fence honours `cancel`.
    pub fn with_fence<T>(
        &self,
        cancel: &Cancellation,
        body: impl FnOnce(&Fence<'_>) -> JournalResult<T>,
    ) -> JournalResult<T> {
        let _serial = (self.access == Access::ReadOnly).then(|| READ_FENCE.lock());
        let (fd, identity) = self.checked_file(FENCE_NAME, libc::O_CREAT | libc::O_RDWR, true)?;
        self.wait_lock(&fd, cancel, self.access == Access::ReadOnly)?;
        self.validate_file(FENCE_NAME, &fd, identity)?;
        let fence = Fence::open(self, fd, identity, cancel.clone())?;
        let result = body(&fence)?;
        fence.validate()?;
        Ok(result)
    }

    /// Records an intent to remove `items` (all strictly inside `root`) and returns the live
    /// operation. Nothing may be mutated on disk before this returns.
    pub fn begin(
        &self,
        root: &[u8],
        root_identity: FileIdentity,
        items: &[ScanRemoval],
        cancel: &Cancellation,
    ) -> JournalResult<Operation<'_>> {
        if !is_standard_path(root) {
            return Err(JournalError::Invalid("removal root".into()));
        }
        if items.is_empty() {
            return Err(JournalError::Invalid("empty removal".into()));
        }
        let mut prefix = root.to_vec();
        if prefix.last() != Some(&b'/') {
            prefix.push(b'/');
        }
        let mut parents: HashMap<Vec<u8>, ParentIdentity> = HashMap::new();
        let mut seen = std::collections::HashSet::with_capacity(items.len());
        for item in items {
            if cancel.is_cancelled() {
                return Err(JournalError::Cancelled);
            }
            let path = item.path.as_slice();
            if path.len() <= prefix.len() || !path.starts_with(&prefix) || !is_standard_path(path) {
                return Err(JournalError::Invalid("removal path".into()));
            }
            if !seen.insert(path) {
                return Err(JournalError::Invalid("duplicate removal path".into()));
            }
            let parent = paths::parent(path).unwrap_or(b"/");
            if !parents.contains_key(parent) {
                parents.insert(parent.to_vec(), canonical::parent_identity(parent, &self.aliases));
            }
        }
        let name = format!("{}{OPERATION_SUFFIX}", uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase());
        let (fd, identity) = self.checked_file(name.as_bytes(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, true)?;
        self.wait_lock(&fd, cancel, false)?;
        let cursor = self.with_fence(cancel, |fence| {
            let sequence = fence.insert(root, root_identity, &name, items, &parents)?;
            Ok(RemovalCursor { identity: fence.identity.clone(), sequence })
        })?;
        Ok(Operation { journal: self, name, fd, identity, cursor, parents, completed: Mutex::new(false) })
    }
}

/// One recorded removal, held by its owner. Dropping it without [`Operation::complete`] (or the
/// owning process dying) turns it into an abandoned intent that recovery replays.
pub struct Operation<'j> {
    journal: &'j Journal,
    name: String,
    fd: OwnedFd,
    identity: FileIdentity,
    cursor: RemovalCursor,
    parents: HashMap<Vec<u8>, ParentIdentity>,
    completed: Mutex<bool>,
}

impl std::fmt::Debug for Operation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Operation").field("name", &self.name).field("cursor", &self.cursor).finish()
    }
}

impl Operation<'_> {
    /// The journal position right after this operation's insertion.
    pub fn cursor(&self) -> &RemovalCursor {
        &self.cursor
    }

    pub fn sequence(&self) -> i64 {
        self.cursor.sequence
    }

    /// Identity the parent of `path` had when the intent was committed (`None` if unresolved).
    pub fn parent_identity(&self, path: &[u8]) -> Option<FileIdentity> {
        self.parents.get(paths::parent(path).unwrap_or(b"/"))?.identity
    }

    /// True only if `path`'s parent still has the identity *and* canonical spelling it had at
    /// intent time. An unresolved parent never authorizes removal.
    pub fn parent_is_unchanged(&self, path: &[u8]) -> bool {
        let parent = paths::parent(path).unwrap_or(b"/");
        let Some(saved) = self.parents.get(parent) else { return false };
        if saved.identity.is_none() || saved.canonical.is_none() {
            return false;
        }
        canonical::parent_identity(parent, &self.journal.aliases) == *saved
    }

    /// Marks the operation complete. Call only after the last attempted filesystem mutation, even
    /// if some attempts failed.
    pub fn complete(&self, cancel: &Cancellation) -> JournalResult<()> {
        let mut completed = self.completed.lock();
        if *completed {
            return Err(JournalError::Invalid("operation already completed".into()));
        }
        self.journal.validate_file(self.name.as_bytes(), &self.fd, self.identity)?;
        self.journal.with_fence(cancel, |fence| fence.finish(self.cursor.sequence))?;
        *completed = true;
        Ok(())
    }
}

/// One `operations` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationEntry {
    pub sequence: i64,
    pub root: Vec<u8>,
    /// [`schema::UNKNOWN_IDENTITY`] for migrated Swift rows whose identity was unreadable.
    pub root_identity: FileIdentity,
    pub completed: bool,
}

/// How a journal item relates to a recovery root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Relation {
    /// Strictly inside the root.
    Inside,
    /// The root itself or one of its ancestors was removed.
    CoversRoot,
    /// The item's location could not be resolved at intent time (or the root cannot be
    /// resolved now); it may affect any root.
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intersection {
    /// The path recorded in the journal.
    pub original_path: Vec<u8>,
    /// The item as seen from the recovery root (projected through canonical aliases when the
    /// root reaches it under another spelling).
    pub removal: ScanRemoval,
    pub relation: Relation,
    /// Set when the recorded path *also* relates to the root lexically, under a different path
    /// than `removal.path`.
    pub original_relation: Option<Relation>,
}

/// Exclusive (or shared, read-only) access to the journal database.
pub struct Fence<'j> {
    conn: Connection,
    journal: &'j Journal,
    lock_fd: OwnedFd,
    lock_identity: FileIdentity,
    db_fd: OwnedFd,
    db_identity: FileIdentity,
    identity: String,
    high_water: Cell<i64>,
    cancel: Cancellation,
}

impl std::fmt::Debug for Fence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fence").field("identity", &self.identity).field("high_water", &self.high_water.get()).finish()
    }
}

impl<'j> Fence<'j> {
    fn open(
        journal: &'j Journal,
        lock_fd: OwnedFd,
        lock_identity: FileIdentity,
        cancel: Cancellation,
    ) -> JournalResult<Self> {
        let write = journal.access == Access::ReadWrite;
        let (db_fd, db_identity) = journal.checked_file(DB_NAME, libc::O_CREAT | libc::O_RDWR, true)?;
        journal.check_sidecars()?;
        let path = journal.directory.join("journal.sqlite");
        let flags = OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | if write { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
        let conn = Connection::open_with_flags(&path, flags)?;
        let mut fence = Self {
            conn,
            journal,
            lock_fd,
            lock_identity,
            db_fd,
            db_identity,
            identity: String::new(),
            high_water: Cell::new(0),
            cancel,
        };
        fence.validate()?;
        let layout = schema::detect(&fence.conn)?;
        if write {
            fence.conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        } else if !matches!(layout, schema::Layout::Current) {
            return Err(JournalError::Incompatible("journal needs initialization".into()));
        }
        match layout {
            schema::Layout::Empty => {
                let identity = uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase();
                schema::create(&fence.conn, &identity)?;
            }
            schema::Layout::Legacy(_) => schema::migrate_legacy(&fence.conn)?,
            schema::Layout::Current => schema::verify_current(&fence.conn)?,
        }
        let mut stmt = fence.conn.prepare("SELECT identity FROM metadata WHERE singleton=1")?;
        let rows: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        drop(stmt);
        let [identity] =
            <[String; 1]>::try_from(rows).map_err(|_| JournalError::Invalid("missing journal identity".into()))?;
        if uuid::Uuid::parse_str(&identity).is_err() {
            return Err(JournalError::Invalid("invalid journal identity".into()));
        }
        fence.identity = identity;
        fence.high_water.set(fence.conn.query_row("SELECT COALESCE(MAX(seq),0) FROM operations", [], |r| r.get(0))?);
        Ok(fence)
    }

    /// The journal's identity (a UUID). Changes only when the journal is recreated.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Highest committed sequence when the fence was taken (or after this fence's own insert).
    /// A repaired snapshot carrying cursor `c` may be published only while
    /// `identity() == c.identity && high_water() == c.sequence`.
    pub fn high_water(&self) -> i64 {
        self.high_water.get()
    }

    /// Lowest valid cursor. Always 0: the journal is never compacted.
    pub fn floor(&self) -> i64 {
        0
    }

    /// True when `cursor` covers exactly this fence's history.
    pub fn is_at(&self, cursor: &RemovalCursor) -> bool {
        self.identity == cursor.identity && self.high_water() == cursor.sequence
    }

    /// Re-verifies the fence and database files. Call immediately before a snapshot rename.
    pub fn validate(&self) -> JournalResult<()> {
        self.journal.validate_file(FENCE_NAME, &self.lock_fd, self.lock_identity)?;
        self.journal.validate_file(DB_NAME, &self.db_fd, self.db_identity)?;
        self.journal.check_sidecars()
    }

    fn check(&self) -> JournalResult<()> {
        if self.cancel.is_cancelled() { Err(JournalError::Cancelled) } else { Ok(()) }
    }

    fn insert(
        &self,
        root: &[u8],
        root_identity: FileIdentity,
        name: &str,
        items: &[ScanRemoval],
        parents: &HashMap<Vec<u8>, ParentIdentity>,
    ) -> JournalResult<i64> {
        self.check()?;
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            self.conn.execute(
                "INSERT INTO operations(root, root_dev, root_ino, lock_name) VALUES(?1,?2,?3,?4)",
                params![root, schema::to_sql(root_identity.dev), schema::to_sql(root_identity.ino), name],
            )?;
            let seq = self.conn.last_insert_rowid();
            let mut stmt = self.conn.prepare(
                "INSERT INTO items(op, path, dev, ino, canonical, parent_dev, parent_ino) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            )?;
            for item in items {
                self.check()?;
                let parent = parents.get(paths::parent(&item.path).unwrap_or(b"/"));
                let (canonical, parent_identity) = match parent {
                    Some(ParentIdentity { canonical: Some(c), identity: Some(id) }) => {
                        (Some(join(c, paths::file_name(&item.path))), Some(*id))
                    }
                    _ => (None, None),
                };
                stmt.execute(params![
                    seq,
                    item.path,
                    schema::to_sql(item.identity.dev),
                    schema::to_sql(item.identity.ino),
                    canonical,
                    parent_identity.map(|p| schema::to_sql(p.dev)),
                    parent_identity.map(|p| schema::to_sql(p.ino)),
                ])?;
            }
            drop(stmt);
            #[cfg(test)]
            Hooks::run(&self.journal.hooks.before_pending_commit);
            self.validate()?;
            self.check()?;
            self.conn.execute_batch("COMMIT")?;
            Ok(seq)
        })();
        match result {
            Ok(seq) => {
                self.high_water.set(seq);
                Ok(seq)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn finish(&self, sequence: i64) -> JournalResult<()> {
        self.check()?;
        self.validate()?;
        let changed = self.conn.execute("UPDATE operations SET complete=1 WHERE seq=?1 AND complete=0", [sequence])?;
        if changed != 1 {
            return Err(JournalError::Invalid("unknown operation".into()));
        }
        Ok(())
    }

    fn page_bounds(&self, after: i64, limit: usize, what: &str) -> JournalResult<()> {
        if after < self.floor() || after > self.high_water() || limit == 0 || limit > MAX_PAGE {
            return Err(JournalError::Invalid(what.into()));
        }
        Ok(())
    }

    fn read_entries(&self, sql: &str, after: i64, limit: usize) -> JournalResult<Vec<OperationEntry>> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(params![after, self.high_water(), limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            self.check()?;
            out.push(OperationEntry {
                sequence: row.get(0)?,
                root: row.get(1)?,
                root_identity: FileIdentity { dev: schema::from_sql(row.get(2)?), ino: schema::from_sql(row.get(3)?) },
                completed: row.get::<_, i64>(4)? != 0,
            });
        }
        Ok(out)
    }

    /// Operations with `after < seq <= high_water`, in sequence order.
    pub fn operations(&self, after: i64, limit: usize) -> JournalResult<Vec<OperationEntry>> {
        self.check()?;
        self.page_bounds(after, limit, "operation page")?;
        self.read_entries(
            "SELECT seq, root, root_dev, root_ino, complete FROM operations WHERE seq>?1 AND seq<=?2 ORDER BY seq LIMIT ?3",
            after,
            limit,
        )
    }

    /// Incomplete operations with `after < seq <= high_water`, in sequence order.
    pub fn pending_operations(&self, after: i64, limit: usize) -> JournalResult<Vec<OperationEntry>> {
        self.check()?;
        self.page_bounds(after, limit, "pending operation page")?;
        self.read_entries(
            "SELECT seq, root, root_dev, root_ino, complete FROM operations
             WHERE complete=0 AND seq>?1 AND seq<=?2 ORDER BY seq LIMIT ?3",
            after,
            limit,
        )
    }

    /// Whether the operation's owner still holds its lock. A missing or replaced lock file fails
    /// closed with [`JournalError::Unsafe`] and is never recreated.
    pub fn is_live(&self, sequence: i64) -> JournalResult<bool> {
        self.check()?;
        let name: String = self
            .conn
            .query_row("SELECT lock_name FROM operations WHERE seq=?1", [sequence], |r| r.get(0))
            .optional()?
            .ok_or_else(|| JournalError::Invalid("unknown operation".into()))?;
        let valid = name
            .strip_suffix(OPERATION_SUFFIX)
            .is_some_and(|stem| stem.len() == 36 && uuid::Uuid::parse_str(stem).is_ok());
        if !valid {
            return Err(JournalError::Unsafe("invalid operation lock".into()));
        }
        let (fd, identity) = self.journal.checked_file(name.as_bytes(), libc::O_RDWR, false)?;
        let path = self.journal.directory.join(&name);
        if !sys::try_lock(fd.as_fd(), false, &path).map_err(|_| JournalError::Unsafe("operation lock".into()))? {
            return Ok(true);
        }
        self.journal.validate_file(name.as_bytes(), &fd, identity)?;
        Ok(false)
    }

    /// Identity + canonical spelling recovery binds `root` to; `None` when it does not resolve to
    /// a directory.
    pub fn root_binding(&self, root: &[u8]) -> Option<RootBinding> {
        canonical::root_binding(root, &self.journal.aliases)
    }

    /// Items of operation `sequence` that relate to `root`, ordered by recorded path, starting
    /// after `after_path`.
    pub fn intersections(
        &self,
        sequence: i64,
        root: &[u8],
        after_path: Option<&[u8]>,
        limit: usize,
    ) -> JournalResult<Vec<Intersection>> {
        self.check()?;
        if limit == 0 || limit > MAX_PAGE || !is_standard_path(root) {
            return Err(JournalError::Invalid("item page".into()));
        }
        let canonical_root = canonical::canonical_directory(root, &self.journal.aliases);
        let raw = PathRange::new(root);
        let canonical = canonical_root.as_deref().map(PathRange::new);
        let mut sql = String::from("SELECT path, dev, ino, canonical FROM items WHERE op=?");
        let mut values = vec![Value::Integer(sequence)];
        if let Some(after) = after_path {
            sql.push_str(" AND path>?");
            values.push(Value::Blob(after.to_vec()));
        }
        if let Some(c) = &canonical {
            let marks = |n: usize| vec!["?"; n].join(",");
            sql.push_str(&format!(
                " AND (canonical IS NULL OR (canonical>=? AND canonical<?) OR canonical IN ({}) \
                 OR (path>=? AND path<?) OR path IN ({}))",
                marks(c.ancestors.len()),
                marks(raw.ancestors.len())
            ));
            for range in [c, &raw] {
                values.push(Value::Blob(range.prefix.clone()));
                values.push(Value::Blob(range.upper.clone()));
                values.extend(range.ancestors.iter().map(|a| Value::Blob(a.clone())));
            }
        }
        sql.push_str(" ORDER BY path LIMIT ?");
        values.push(Value::Integer(limit as i64));
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(values))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            self.check()?;
            let original: Vec<u8> = row.get(0)?;
            let identity = FileIdentity { dev: schema::from_sql(row.get(1)?), ino: schema::from_sql(row.get(2)?) };
            let resolved: Option<Vec<u8>> = row.get(3)?;
            let raw_relation = raw.relation(&original);
            let (Some(canonical_root), Some(range), Some(resolved)) = (&canonical_root, &canonical, resolved) else {
                out.push(Intersection {
                    removal: ScanRemoval { path: original.clone(), identity },
                    original_path: original,
                    relation: Relation::Unresolved,
                    original_relation: raw_relation,
                });
                continue;
            };
            if resolved.starts_with(&range.prefix) && resolved.len() > canonical_root.len() {
                let suffix = &resolved[canonical_root.len()..];
                let mut projected = root.to_vec();
                if projected.last() != Some(&b'/') && suffix.first() != Some(&b'/') {
                    projected.push(b'/');
                }
                projected.extend_from_slice(suffix);
                let additional = if projected == original { None } else { raw_relation };
                out.push(Intersection {
                    original_path: original,
                    removal: ScanRemoval { path: projected, identity },
                    relation: Relation::Inside,
                    original_relation: additional,
                });
            } else if range.ancestors.contains(&resolved) {
                out.push(Intersection {
                    removal: ScanRemoval { path: original.clone(), identity },
                    original_path: original,
                    relation: Relation::CoversRoot,
                    original_relation: raw_relation.filter(|r| *r == Relation::Inside),
                });
            } else if let Some(relation) = raw_relation {
                out.push(Intersection {
                    removal: ScanRemoval { path: original.clone(), identity },
                    original_path: original,
                    relation,
                    original_relation: None,
                });
            } else {
                return Err(JournalError::Invalid("invalid intersection".into()));
            }
        }
        Ok(out)
    }
}

/// Byte range of everything strictly inside `root`, plus `root` and its ancestors.
struct PathRange {
    prefix: Vec<u8>,
    /// Exclusive upper bound: `prefix` with its trailing `/` replaced by `0` (the next byte).
    upper: Vec<u8>,
    ancestors: Vec<Vec<u8>>,
}

impl PathRange {
    fn new(root: &[u8]) -> Self {
        let mut prefix = root.to_vec();
        if prefix.last() != Some(&b'/') {
            prefix.push(b'/');
        }
        let mut upper = prefix.clone();
        *upper.last_mut().expect("non-empty") = b'0';
        let mut ancestors = Vec::new();
        let mut current = root;
        loop {
            ancestors.push(current.to_vec());
            match paths::parent(current) {
                Some(parent) if current != b"/" => current = parent,
                _ => break,
            }
        }
        Self { prefix, upper, ancestors }
    }

    fn relation(&self, path: &[u8]) -> Option<Relation> {
        if path.starts_with(&self.prefix) {
            Some(Relation::Inside)
        } else if self.ancestors.iter().any(|a| a == path) {
            Some(Relation::CoversRoot)
        } else {
            None
        }
    }
}

/// Current identity of `path` without following a final symlink (`lstat`).
pub fn current_identity(path: &[u8]) -> Option<FileIdentity> {
    sys::lstat(path_from_bytes(path)).ok().map(|m| m.identity)
}

/// Test-only hook points, mirroring the Swift `ORBIT_TESTING` statics but per journal.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Hooks {
    pub before_pending_commit: Mutex<Option<Box<dyn FnMut() + Send>>>,
    pub after_recovery_probe: Mutex<Option<Box<dyn FnMut() + Send>>>,
    pub fence_busy: Mutex<Option<Box<dyn FnMut() + Send>>>,
}

#[cfg(test)]
impl Hooks {
    /// Runs a hook without holding its slot, so the hook may re-enter the journal.
    pub(crate) fn run(slot: &Mutex<Option<Box<dyn FnMut() + Send>>>) {
        let hook = slot.lock().take();
        if let Some(mut hook) = hook {
            hook();
            let mut guard = slot.lock();
            if guard.is_none() {
                *guard = Some(hook);
            }
        }
    }

    pub(crate) fn set(slot: &Mutex<Option<Box<dyn FnMut() + Send>>>, hook: impl FnMut() + Send + 'static) {
        *slot.lock() = Some(Box::new(hook));
    }
}
