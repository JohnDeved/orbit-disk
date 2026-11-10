//! Inventory format 6: the SQLite schema shared by mapping stages and saved maps.
//!
//! Ported from `InventorySchema.swift` and the stage DDL in `WALListingStage.swift`, then
//! redesigned for write speed (old caches are discarded, not migrated):
//!
//! - Names are raw bytes (`BLOB`); `search_name` is [`crate::fold::fold`] of the name.
//! - `dev`/`inode` store the `u64` bit pattern in an `INTEGER` (see [`to_sql_u64`]).
//! - Full paths exist only for traversable directories, in `dirs`, which also carries the
//!   per-directory queue state (`pending`, listing `epoch`, `full_generation`). Swift spread this
//!   over `directories`, `directory_paths` and `epochs`; one row per directory now replaces three.
//! - Directory totals live directly in `nodes` (no totals trigger); ancestors are updated by id.
//! - `nodes.id` is a plain rowid (no `AUTOINCREMENT`, so no `sqlite_sequence` write per row).
//! - Fresh single-link files do not get an `owners` row; a probe of `file_identities` proves the
//!   identity unique instead (see `stage::writer`).
//!
//! Stage-only tables stay in the published file. At publication they are empty or tiny
//! (`owners` holds only multi-link identities, `scan`/`full_refresh` one row each), so dropping
//! them would cost a rewrite for no measurable size gain.

use crate::{CoreError, Result};
use orbit_platform::{Cancellation, FileIdentity};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};

/// `PRAGMA application_id` of every Orbit inventory (`'ORBT'`).
pub const APPLICATION_ID: i64 = 0x4F52_4254;
/// `PRAGMA user_version`. Format 6 is the first Rust format; Swift formats are rejected.
pub const VERSION: i64 = 6;

/// Tables and indexes every inventory (stage or saved map) contains.
pub const INVENTORY_DDL: &str = "
CREATE TABLE IF NOT EXISTS metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  snapshot_id TEXT NOT NULL, root_path BLOB NOT NULL, scanned_at REAL NOT NULL, elapsed REAL NOT NULL,
  skipped INTEGER NOT NULL, event_id INTEGER NOT NULL, event_identity TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS snapshot_issues(ord INTEGER PRIMARY KEY, message TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS nodes(id INTEGER PRIMARY KEY, parent INTEGER, name BLOB NOT NULL,
  search_name TEXT NOT NULL, kind INTEGER NOT NULL, accessible INTEGER NOT NULL, is_mount INTEGER NOT NULL,
  size INTEGER NOT NULL, allocated INTEGER NOT NULL, file_count INTEGER NOT NULL, raw_alloc INTEGER NOT NULL,
  dev INTEGER NOT NULL, inode INTEGER NOT NULL, links INTEGER NOT NULL, epoch INTEGER NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS node_names ON nodes(parent,name);
CREATE INDEX IF NOT EXISTS children_by_size ON nodes(parent,size DESC,name);
CREATE INDEX IF NOT EXISTS nodes_by_size ON nodes(size DESC,id,search_name,parent);
CREATE INDEX IF NOT EXISTS file_identities ON nodes(dev,inode) WHERE kind<>1;
CREATE TABLE IF NOT EXISTS dirs(path BLOB PRIMARY KEY, id INTEGER NOT NULL UNIQUE, dev INTEGER NOT NULL,
  inode INTEGER NOT NULL, pending INTEGER NOT NULL, epoch INTEGER NOT NULL DEFAULT 0,
  full_generation INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;
CREATE TRIGGER IF NOT EXISTS node_dir_delete AFTER DELETE ON nodes WHEN OLD.kind=1 BEGIN
  DELETE FROM dirs WHERE id=OLD.id;
END;
CREATE TABLE IF NOT EXISTS orbit_search_generation(singleton INTEGER PRIMARY KEY CHECK(singleton=1), value INTEGER NOT NULL);
INSERT OR IGNORE INTO orbit_search_generation VALUES(1,0);
CREATE TRIGGER IF NOT EXISTS node_search_generation AFTER UPDATE OF search_name ON nodes
  WHEN OLD.search_name<>NEW.search_name BEGIN
  UPDATE orbit_search_generation SET value=value+1 WHERE singleton=1;
END;
";

/// Stage bookkeeping: queue, listing epochs, trim/prune work, hard-link reconciliation, the
/// scan-start checkpoint, the seal marker and the mailbox watermark.
pub const STAGE_DDL: &str = "
CREATE TABLE IF NOT EXISTS scan(root BLOB PRIMARY KEY, identity TEXT NOT NULL, event_id INTEGER NOT NULL,
  expected_snapshot TEXT NOT NULL, generation TEXT) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS sealed(singleton INTEGER PRIMARY KEY CHECK(singleton=1), snapshot_id TEXT NOT NULL,
  published INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS stage_issues(path BLOB PRIMARY KEY, message TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS listing_issues(parent BLOB PRIMARY KEY, count INTEGER NOT NULL, messages BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS blocked_directories(path BLOB PRIMARY KEY) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS dirty_links(dev INTEGER NOT NULL, inode INTEGER NOT NULL, PRIMARY KEY(dev,inode)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS link_candidates(dev INTEGER NOT NULL, inode INTEGER NOT NULL, node_id INTEGER NOT NULL,
  PRIMARY KEY(dev,inode,node_id)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS link_rebuild(dev INTEGER NOT NULL, inode INTEGER NOT NULL, PRIMARY KEY(dev,inode)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS owners(dev INTEGER NOT NULL, inode INTEGER NOT NULL, node_id INTEGER NOT NULL,
  PRIMARY KEY(dev,inode)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS dirty(parent BLOB PRIMARY KEY, revision INTEGER NOT NULL, pending INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS full_refresh(singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation INTEGER NOT NULL,
  pending INTEGER NOT NULL);
INSERT OR IGNORE INTO full_refresh VALUES(1,0,0);
CREATE TABLE IF NOT EXISTS prune(path BLOB PRIMARY KEY, parent BLOB NOT NULL, name BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS trim(parent BLOB PRIMARY KEY, epoch INTEGER NOT NULL, revision INTEGER NOT NULL,
  full_generation INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS orbit_async_progress(singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  generation BLOB NOT NULL, sequence INTEGER NOT NULL);
";

/// Partial queue indexes, created lazily inside a cancellable transaction (`prepare_queue_indexes`).
pub const QUEUE_INDEX_DDL: [&str; 2] = [
    "CREATE INDEX IF NOT EXISTS pending_dirs ON dirs(path) WHERE pending=1",
    "CREATE INDEX IF NOT EXISTS pending_dirty ON dirty(parent) WHERE pending=1",
];

/// Tables that must exist in a format-6 stage (used to validate files found on disk).
pub const REQUIRED_STAGE_TABLES: [&str; 5] = ["scan", "nodes", "dirs", "metadata", "full_refresh"];

/// Writer pragmas: WAL, FULL synchronization, 512 MiB autocheckpoint and a 256 MiB page cache.
/// A large map's indexes outgrow a small cache (every insert then rereads a page), and frequent
/// checkpoints rewrite the same index pages into the database again and again; both dominated
/// writing a 5-million-file home folder. The cache only grows as pages are used. Statement
/// journals stay in memory: each folder of a transaction group is a savepoint, and a
/// file-backed sub-journal would write every modified page twice.
pub const WRITER_PRAGMAS: &str = "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; \
    PRAGMA cache_size=-262144; PRAGMA wal_autocheckpoint=131072; PRAGMA temp_store=MEMORY;";

/// Busy timeout of the stage writer.
pub const WRITER_BUSY_MS: u64 = 5_000;
/// Busy timeout of readers that may wait for a lock.
pub const READER_BUSY_MS: u64 = 2_000;
/// Readers poll their cancellation every this many SQLite VM operations.
pub const PROGRESS_OPS: i32 = 1_000;

/// Stores a `u64` bit pattern in an SQLite `INTEGER`.
#[inline]
pub fn to_sql_u64(value: u64) -> i64 {
    value as i64
}

/// Reads back a `u64` stored with [`to_sql_u64`].
#[inline]
pub fn from_sql_u64(value: i64) -> u64 {
    value as u64
}

/// Sizes are clamped to `i64::MAX` before they enter SQLite (platform layers already clamp `< 0`).
#[inline]
pub fn to_sql_size(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

#[inline]
pub fn identity_from_sql(dev: i64, inode: i64) -> FileIdentity {
    FileIdentity { dev: from_sql_u64(dev), ino: from_sql_u64(inode) }
}

/// Saved-map file name for a root: lowercase SHA-256 hex of the root path bytes.
pub fn root_digest(root: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(root))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

/// Metadata of a sealed (or preview) inventory. Swift: `IndexSnapshot`.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotInfo {
    pub id: String,
    pub root_path: Vec<u8>,
    pub database: PathBuf,
    /// Seconds since the Unix epoch.
    pub scanned_at: f64,
    pub elapsed: f64,
    pub skipped: u64,
    /// At most 100 issue messages.
    pub issues: Vec<String>,
    pub event_id: u64,
    pub event_identity: String,
}

/// Opens an inventory for reading. Readers never write; `cancel` interrupts long statements
/// through SQLite's progress handler (they then fail with `CoreError::Cancelled`).
pub fn open_reader(path: &Path, cancel: Option<&Cancellation>, wait_for_locks: bool) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(std::time::Duration::from_millis(if wait_for_locks { READER_BUSY_MS } else { 0 }))?;
    if let Some(cancel) = cancel {
        let cancel = cancel.clone();
        conn.progress_handler(PROGRESS_OPS, Some(move || cancel.is_cancelled()));
    }
    Ok(conn)
}

/// Maps an SQLite interruption caused by a cancelled progress handler to `CoreError::Cancelled`.
pub fn map_cancel(error: rusqlite::Error, cancel: Option<&Cancellation>) -> CoreError {
    if cancel.is_some_and(Cancellation::is_cancelled) {
        return CoreError::Cancelled;
    }
    if let rusqlite::Error::SqliteFailure(failure, _) = &error
        && failure.code == rusqlite::ErrorCode::OperationInterrupted
    {
        return CoreError::Cancelled;
    }
    CoreError::Sqlite(error)
}

pub(crate) fn check(cancel: Option<&Cancellation>) -> Result<()> {
    if cancel.is_some_and(Cancellation::is_cancelled) { Err(CoreError::Cancelled) } else { Ok(()) }
}

/// Validates the format of an opened inventory and returns its metadata.
pub fn read_snapshot(conn: &Connection, database: &Path) -> Result<SnapshotInfo> {
    let application: i64 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if application != APPLICATION_ID || version != VERSION {
        return Err(CoreError::Invalid("This disk index has an unsupported format.".into()));
    }
    let row = conn
        .query_row(
            "SELECT snapshot_id,root_path,scanned_at,elapsed,skipped,event_id,event_identity FROM metadata WHERE singleton=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;
    let damaged = || CoreError::Invalid("This disk index is incomplete or damaged.".into());
    let (id, root_path, scanned_at, elapsed, skipped, event_id, event_identity) = row.ok_or_else(damaged)?;
    if id.is_empty()
        || root_path.first() != Some(&b'/')
        || !scanned_at.is_finite()
        || !elapsed.is_finite()
        || elapsed < 0.0
        || skipped < 0
    {
        return Err(damaged());
    }
    let mut statement = conn.prepare("SELECT message FROM snapshot_issues ORDER BY ord LIMIT 100")?;
    let issues = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(SnapshotInfo {
        id,
        root_path,
        database: database.to_path_buf(),
        scanned_at,
        elapsed,
        skipped: skipped as u64,
        issues,
        event_id: from_sql_u64(event_id),
        event_identity,
    })
}

/// Rejects files that are not empty and not a format-6 stage (Swift `StageLease.validateSchema`).
pub fn validate_stage_file(path: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    let incompatible = || CoreError::Invalid("the mapping stage has an incompatible format".into());
    let integer = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0)).map_err(|_| incompatible());
    let application = integer("PRAGMA application_id")?;
    let version = integer("PRAGMA user_version")?;
    let objects = integer("SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")?;
    if application == 0 && version == 0 && objects == 0 {
        return Ok(());
    }
    let list = REQUIRED_STAGE_TABLES.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(",");
    let tables = integer(&format!("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ({list})"))?;
    if application != APPLICATION_ID || version != VERSION || tables != REQUIRED_STAGE_TABLES.len() as i64 {
        return Err(incompatible());
    }
    Ok(())
}

/// Encodes at most 100 listing-issue messages as NUL-separated bytes (NULs inside are dropped).
pub(crate) fn encode_messages(messages: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, message) in messages.iter().take(100).enumerate() {
        if index > 0 {
            out.push(0);
        }
        out.extend(message.bytes().filter(|b| *b != 0));
    }
    out
}

pub(crate) fn decode_messages(bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes.split(|b| *b == 0).map(|m| String::from_utf8_lossy(m).into_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swift_and_foreign_inventories_are_rejected_but_empty_files_initialize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.sqlite");
        // A Swift format-5 stage: same application id, older version.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA application_id=1330790996; PRAGMA user_version=5; CREATE TABLE nodes(id INTEGER);")
            .unwrap();
        drop(conn);
        assert!(validate_stage_file(&path).is_err());
        let empty = dir.path().join("empty.sqlite");
        drop(Connection::open(&empty).unwrap());
        validate_stage_file(&empty).unwrap();
        // A foreign database without version markers.
        let foreign = dir.path().join("foreign.sqlite");
        Connection::open(&foreign).unwrap().execute_batch("CREATE TABLE t(x)").unwrap();
        assert!(validate_stage_file(&foreign).is_err());
    }

    #[test]
    fn listing_messages_round_trip_without_nul() {
        let messages = vec!["a\0b".to_string(), "é".to_string(), String::new(), "x".to_string()];
        assert_eq!(decode_messages(&encode_messages(&messages)), ["ab", "é", "", "x"]);
        assert!(decode_messages(&encode_messages(&[])).is_empty());
    }
}
