//! Completed listings of a resumed stage, served to discovery so a resumed mapping shows (and
//! does not re-list) what the stage already committed. Swift: `DiscoveryCacheReader`.
//!
//! Each discovery worker opens its own read-only connection with no busy timeout. Every
//! [`ListingCacheReader::listing`] runs in its own short read transaction, so the writer's WAL
//! checkpoints are never held back for long. A directory is served only when its stage row has
//! the queued identity, it is not pending, not dirty and (during a full refresh) already belongs
//! to the current refresh generation; otherwise only its expectations are returned and discovery
//! lists it from the filesystem.

use crate::discovery::cache::{CachedListing, ListingCache, ListingCacheReader};
use crate::records::RecordBatch;
use crate::schema::{identity_from_sql, to_sql_u64};
use orbit_platform::{EntryKind, EntryRecord, FileIdentity};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::PathBuf;

/// [`ListingCache`] over a stage database.
#[derive(Debug, Clone)]
pub struct StageListingCache {
    database: PathBuf,
    root_identity: FileIdentity,
}

impl StageListingCache {
    pub fn new(database: PathBuf, root_identity: FileIdentity) -> Self {
        Self { database, root_identity }
    }
}

impl ListingCache for StageListingCache {
    fn open_reader(&self) -> Option<Box<dyn ListingCacheReader>> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&self.database, flags).ok()?;
        conn.busy_timeout(std::time::Duration::ZERO).ok()?;
        conn.execute_batch("BEGIN").ok()?;
        let state = (|| -> rusqlite::Result<(i64, Option<(i64, i64)>)> {
            let generation: i64 =
                conn.query_row("SELECT generation FROM full_refresh WHERE singleton=1", [], |r| r.get(0))?;
            let root: Option<(i64, i64)> = conn
                .query_row("SELECT dev,inode FROM nodes WHERE parent IS NULL", [], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            Ok((generation, root))
        })();
        let _ = conn.execute_batch("ROLLBACK");
        let (generation, root) = state.ok()?;
        if root.map(|(d, i)| identity_from_sql(d, i)) != Some(self.root_identity) {
            return None;
        }
        Some(Box::new(StageCacheReader { conn, generation }))
    }
}

struct StageCacheReader {
    conn: Connection,
    generation: i64,
}

impl StageCacheReader {
    fn read(&mut self, path: &[u8], identity: FileIdentity) -> rusqlite::Result<Option<CachedListing>> {
        let state = self
            .conn
            .prepare_cached(
                "SELECT d.id,d.pending,COALESCE(dirty.pending,0),COALESCE(dirty.revision,0),f.generation,f.pending,
                        d.full_generation
                 FROM dirs d CROSS JOIN full_refresh f LEFT JOIN dirty ON dirty.parent=d.path
                 WHERE d.path=?1 AND d.dev=?2 AND d.inode=?3",
            )?
            .query_row(rusqlite::params![path, to_sql_u64(identity.dev), to_sql_u64(identity.ino)], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            })
            .optional()?;
        let Some((id, pending, dirty, revision, generation, refreshing, dir_generation)) = state else {
            return Ok(None);
        };
        if pending != 0 || dirty != 0 || (refreshing != 0 && dir_generation != generation) {
            return Ok(Some(CachedListing { records: None, revision, generation, import: false }));
        }
        let mut rows = self
            .conn
            .prepare_cached("SELECT name,kind,is_mount,size,raw_alloc,dev,inode,links FROM nodes WHERE parent=?1")?;
        let mut records = RecordBatch::default();
        let mut query = rows.query([id])?;
        while let Some(row) = query.next()? {
            let name: Vec<u8> = row.get(0)?;
            let kind = match row.get::<_, i64>(1)? {
                0 => EntryKind::File,
                1 => EntryKind::Directory,
                2 => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
            let (size, allocated): (i64, i64) = (row.get(3)?, row.get(4)?);
            let is_dir = kind == EntryKind::Directory;
            records.push(&EntryRecord {
                name: &name,
                kind,
                id: identity_from_sql(row.get(5)?, row.get(6)?),
                nlink: u32::try_from(row.get::<_, i64>(7)?.max(1)).unwrap_or(u32::MAX),
                size: if is_dir { 0 } else { size.max(0) as u64 },
                allocated: if is_dir { 0 } else { allocated.max(0) as u64 },
                is_mount_point: row.get::<_, i64>(2)? != 0,
            });
        }
        Ok(Some(CachedListing { records: Some(records), revision, generation, import: false }))
    }
}

impl ListingCacheReader for StageCacheReader {
    fn generation(&self) -> i64 {
        self.generation
    }

    fn listing(&mut self, path: &[u8], identity: FileIdentity) -> Option<CachedListing> {
        if self.conn.execute_batch("BEGIN").is_err() {
            return None;
        }
        let result = self.read(path, identity);
        let _ = self.conn.execute_batch("ROLLBACK");
        result.ok().flatten()
    }
}
