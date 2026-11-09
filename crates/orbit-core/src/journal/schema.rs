//! Journal database format.
//!
//! Format history (`PRAGMA user_version`, `application_id = 'ORJN'`):
//! - 2, 3: Swift Orbit Disk (identities as `"dev:ino"` text; 3 added `pending_operations`).
//! - 4: this port. Identities are integer `dev`/`ino` columns (`u64` stored by bit pattern).
//!
//! A version 2/3 journal is migrated in place, in one `BEGIN IMMEDIATE` transaction under the
//! fence, preserving the journal identity, every sequence number, completion flags and lock
//! names. Saved maps and stages written by the Swift app keep their cursors, and an operation a
//! killed Swift process left behind is replayed like any other abandoned operation.

use super::JournalError;
use orbit_platform::FileIdentity;
use rusqlite::{Connection, OptionalExtension, params};

pub const APPLICATION_ID: i64 = 0x4f52_4a4e;
pub const VERSION: i64 = 4;

/// The version 4 schema. Kept as one constant so the DDL in reports and code cannot drift.
pub const DDL: &str = "
CREATE TABLE metadata(
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  identity TEXT NOT NULL);
CREATE TABLE operations(
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  root BLOB NOT NULL,
  root_dev INTEGER NOT NULL,
  root_ino INTEGER NOT NULL,
  lock_name TEXT NOT NULL UNIQUE,
  complete INTEGER NOT NULL DEFAULT 0);
CREATE INDEX pending_operations ON operations(complete, seq);
CREATE TABLE items(
  op INTEGER NOT NULL,
  path BLOB NOT NULL,
  dev INTEGER NOT NULL,
  ino INTEGER NOT NULL,
  canonical BLOB,
  parent_dev INTEGER,
  parent_ino INTEGER,
  PRIMARY KEY(op, path)) WITHOUT ROWID;
";

/// Identity stored for a migrated row whose Swift text identity cannot be parsed. Inode 0 never
/// names a live file; such items are also stored with a NULL canonical path, so recovery reports
/// them as unresolved and falls back to a full refresh.
pub const UNKNOWN_IDENTITY: FileIdentity = FileIdentity { dev: 0, ino: 0 };

#[inline]
pub fn to_sql(value: u64) -> i64 {
    value as i64
}

#[inline]
pub fn from_sql(value: i64) -> u64 {
    value as u64
}

/// Parses a Swift `"\(st_dev):\(st_ino)"` identity. Darwin's `st_dev` is a signed 32-bit value,
/// so negative devices are widened through their unsigned bit pattern, exactly as
/// `orbit_platform::posix::stat_identity` does.
pub fn parse_legacy_identity(text: &str) -> Option<FileIdentity> {
    let (dev, ino) = text.split_once(':')?;
    let dev: i64 = dev.parse().ok()?;
    let dev = if dev < 0 { u64::from(i32::try_from(dev).ok()? as u32) } else { dev as u64 };
    Some(FileIdentity { dev, ino: ino.parse().ok()? })
}

pub enum Layout {
    Empty,
    Legacy(i64),
    Current,
}

pub fn detect(conn: &Connection) -> Result<Layout, JournalError> {
    let app: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let objects: i64 =
        conn.query_row("SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'", [], |r| r.get(0))?;
    match (app, version, objects) {
        (0, 0, 0) => Ok(Layout::Empty),
        (APPLICATION_ID, 2 | 3, n) if n > 0 => Ok(Layout::Legacy(version)),
        (APPLICATION_ID, VERSION, n) if n > 0 => Ok(Layout::Current),
        _ => Err(JournalError::Incompatible(format!("application id {app:#x}, version {version}"))),
    }
}

fn require_tables(conn: &Connection) -> Result<(), JournalError> {
    let tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('metadata','operations','items')",
        [],
        |r| r.get(0),
    )?;
    if tables != 3 {
        return Err(JournalError::Incompatible("journal schema".into()));
    }
    Ok(())
}

pub fn verify_current(conn: &Connection) -> Result<(), JournalError> {
    require_tables(conn)
}

pub fn create(conn: &Connection, identity: &str) -> Result<(), JournalError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        conn.execute_batch(&format!("PRAGMA application_id={APPLICATION_ID}; PRAGMA user_version={VERSION};"))?;
        conn.execute_batch(DDL)?;
        conn.execute("INSERT INTO metadata VALUES(1, ?1)", [identity])?;
        conn.execute_batch("COMMIT")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    result
}

/// Migrates a Swift journal (version 2 or 3) to version 4 in one transaction.
pub fn migrate_legacy(conn: &Connection) -> Result<(), JournalError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = migrate_inside(conn);
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn migrate_inside(conn: &Connection) -> Result<(), JournalError> {
    require_tables(conn)?;
    let reserved: Option<i64> =
        conn.query_row("SELECT seq FROM sqlite_sequence WHERE name='operations'", [], |r| r.get(0)).optional()?;
    conn.execute_batch(
        "DROP INDEX IF EXISTS pending_operations;
         ALTER TABLE operations RENAME TO legacy_operations;
         ALTER TABLE items RENAME TO legacy_items;",
    )?;
    // `metadata` is kept as is: its identity is what saved cursors refer to.
    let ddl_without_metadata = DDL.split_once(';').map(|(_, rest)| rest).unwrap_or_default();
    conn.execute_batch(ddl_without_metadata)?;
    {
        let mut read = conn.prepare("SELECT seq, root, root_identity, lock_name, complete FROM legacy_operations")?;
        let mut write = conn.prepare(
            "INSERT INTO operations(seq, root, root_dev, root_ino, lock_name, complete) VALUES(?1,?2,?3,?4,?5,?6)",
        )?;
        let mut rows = read.query([])?;
        while let Some(row) = rows.next()? {
            let identity =
                legacy_text(row.get_ref(2)?).and_then(|t| parse_legacy_identity(&t)).unwrap_or(UNKNOWN_IDENTITY);
            let lock = legacy_text(row.get_ref(3)?).ok_or_else(|| JournalError::Incompatible("lock name".into()))?;
            write.execute(params![
                row.get::<_, i64>(0)?,
                legacy_blob(row.get_ref(1)?),
                to_sql(identity.dev),
                to_sql(identity.ino),
                lock,
                row.get::<_, i64>(4)?
            ])?;
        }
    }
    {
        let mut read = conn.prepare("SELECT op, path, identity, canonical, parent_identity FROM legacy_items")?;
        let mut write = conn.prepare(
            "INSERT INTO items(op, path, dev, ino, canonical, parent_dev, parent_ino) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        )?;
        let mut rows = read.query([])?;
        while let Some(row) = rows.next()? {
            let identity = legacy_text(row.get_ref(2)?).and_then(|t| parse_legacy_identity(&t));
            let parent = legacy_text(row.get_ref(4)?).and_then(|t| parse_legacy_identity(&t));
            let canonical = match row.get_ref(3)? {
                rusqlite::types::ValueRef::Null => None,
                value => Some(legacy_blob(value)),
            };
            // An unreadable identity must not be trusted for matching: drop the canonical path so
            // the item is reported as unresolved.
            let canonical = if identity.is_some() { canonical } else { None };
            let identity = identity.unwrap_or(UNKNOWN_IDENTITY);
            write.execute(params![
                row.get::<_, i64>(0)?,
                legacy_blob(row.get_ref(1)?),
                to_sql(identity.dev),
                to_sql(identity.ino),
                canonical,
                parent.map(|p| to_sql(p.dev)),
                parent.map(|p| to_sql(p.ino)),
            ])?;
        }
    }
    conn.execute_batch("DROP TABLE legacy_items; DROP TABLE legacy_operations;")?;
    // AUTOINCREMENT must never reissue a sequence the Swift journal already handed out.
    if let Some(reserved) = reserved {
        let updated =
            conn.execute("UPDATE sqlite_sequence SET seq=max(seq, ?1) WHERE name='operations'", [reserved])?;
        if updated == 0 {
            conn.execute("INSERT INTO sqlite_sequence(name, seq) VALUES('operations', ?1)", [reserved])?;
        }
    }
    conn.execute_batch(&format!("PRAGMA user_version={VERSION};"))?;
    Ok(())
}

fn legacy_text(value: rusqlite::types::ValueRef<'_>) -> Option<String> {
    match value {
        rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => String::from_utf8(t.to_vec()).ok(),
        _ => None,
    }
}

fn legacy_blob(value: rusqlite::types::ValueRef<'_>) -> Vec<u8> {
    match value {
        rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => t.to_vec(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_identities_widen_signed_darwin_devices() {
        assert_eq!(parse_legacy_identity("16777232:42"), Some(FileIdentity { dev: 16_777_232, ino: 42 }));
        assert_eq!(parse_legacy_identity("-2:7"), Some(FileIdentity { dev: u64::from(u32::MAX - 1), ino: 7 }));
        assert_eq!(parse_legacy_identity("old-id"), None);
        assert_eq!(parse_legacy_identity("1:x"), None);
    }
}
