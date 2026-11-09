//! Reusing listings from saved maps of subfolders when a parent folder is mapped (Swift
//! `StageReuse` + `DiscoveryReuse`), and the discovery cache that combines them with a resumed
//! stage.
//!
//! Candidates are chosen once, before discovery starts: every valid saved map whose root lies
//! strictly inside the mapped root, that was complete (no skipped folders), belongs to the same
//! change history as this mapping and is not newer than it, has no removal-journal history past
//! its cursor, and is still reachable from the mapped root as the same directory. The stage's
//! scan-start event is then lowered to the oldest candidate's event *before* any record is
//! copied, so the change monitor replays everything that happened since that map was saved.
//!
//! A reused listing is served to discovery (shown immediately and queued for the writer like a
//! filesystem listing) only when the saved map records the queued directory with the same
//! identity and as readable; anything else falls back to the filesystem.

use crate::discovery::cache::{CachedListing, ListingCache, ListingCacheReader};
use crate::journal::{Journal, RemovalCursor};
use crate::records::RecordBatch;
use crate::recovery::SystemClock;
use crate::schema::{self, SnapshotInfo, identity_from_sql, to_sql_u64};
use crate::stage::cache::StageListingCache;
use crate::store::{IndexPaths, Store};
use orbit_platform::{Cancellation, EntryKind, EntryRecord, FileIdentity, Platform};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::PathBuf;
use std::sync::Arc;

/// A saved map of a subfolder whose listings may be copied into the stage.
#[derive(Clone, Debug, PartialEq)]
pub struct ReuseCandidate {
    pub root: Vec<u8>,
    pub root_identity: FileIdentity,
    pub snapshot: SnapshotInfo,
}

/// Finds reusable saved maps below `root` (see the module docs). `history` is the mapping's
/// change-history identity and position.
pub fn select_candidates<P: Platform>(
    platform: &P,
    paths: &IndexPaths,
    journal: Option<&Journal>,
    root: &[u8],
    root_identity: FileIdentity,
    history: (&str, u64),
    cancel: &Cancellation,
) -> Vec<ReuseCandidate> {
    let store = Store::new(paths.clone());
    let Ok(entries) = std::fs::read_dir(&paths.cache) else { return Vec::new() };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        if cancel.is_cancelled() {
            return Vec::new();
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || !name.ends_with(".sqlite") {
            continue;
        }
        let Some(snapshot) = read_root(&entry.path()) else { continue };
        let candidate_root = snapshot.root_path.clone();
        if candidate_root == root || !crate::paths::is_within(&candidate_root, root) {
            continue;
        }
        // The file name must be the canonical one for its root (no stray copies).
        let Some(snapshot) = store.saved_snapshot(&candidate_root) else { continue };
        if snapshot.skipped != 0 || snapshot.event_identity != history.0 || snapshot.event_id > history.1 {
            continue;
        }
        let Ok(Some(node)) = store.node(&snapshot, &candidate_root, Some(cancel)) else { continue };
        let Some(identity) = node.meta.identity.filter(|_| node.meta.flags.is_dir && node.meta.flags.accessible) else {
            continue;
        };
        if !matches!(
            crate::saved::can_reach(platform, root, &candidate_root, root_identity, Some(identity), cancel),
            Ok(true)
        ) {
            continue;
        }
        if let Some(journal) = journal {
            let stored = store
                .removal_cursor(&snapshot, Some(cancel))
                .ok()
                .flatten()
                .map(|(identity, sequence)| RemovalCursor { identity, sequence });
            let Some(stored) = stored else { continue };
            match journal.recover(&candidate_root, Some(&stored), cancel, &SystemClock::default()) {
                Ok(plan) if plan.is_empty() => {}
                _ => continue,
            }
        }
        candidates.push(ReuseCandidate { root: candidate_root, root_identity: identity, snapshot });
    }
    // Deepest first: a folder is served by the most specific map.
    candidates.sort_by(|a, b| b.root.len().cmp(&a.root.len()).then_with(|| a.root.cmp(&b.root)));
    candidates
}

fn read_root(file: &std::path::Path) -> Option<SnapshotInfo> {
    let conn = schema::open_reader(file, None, false).ok()?;
    schema::read_snapshot(&conn, file).ok()
}

/// The discovery cache of a mapping session: a resumed stage's committed listings first, then
/// reusable saved maps of subfolders.
#[derive(Debug, Clone)]
pub struct SessionCache {
    stage: Option<StageListingCache>,
    candidates: Arc<[ReuseCandidate]>,
    generation: i64,
}

impl SessionCache {
    /// `generation`: the stage's full-refresh generation (stamped on imported listings).
    pub fn new(stage: Option<StageListingCache>, candidates: Vec<ReuseCandidate>, generation: i64) -> Self {
        Self { stage, candidates: candidates.into(), generation }
    }

    pub fn is_empty(&self) -> bool {
        self.stage.is_none() && self.candidates.is_empty()
    }
}

impl ListingCache for SessionCache {
    fn open_reader(&self) -> Option<Box<dyn ListingCacheReader>> {
        let stage = match &self.stage {
            Some(cache) => Some(cache.open_reader()?),
            None => None,
        };
        let generation = stage.as_ref().map_or(self.generation, |s| s.generation());
        let sources = self.candidates.iter().map(|c| (c.clone(), None)).collect();
        Some(Box::new(SessionReader { stage, sources, generation }))
    }
}

struct SessionReader {
    stage: Option<Box<dyn ListingCacheReader>>,
    /// Lazily opened read-only connections, one per candidate.
    sources: Vec<(ReuseCandidate, Option<Connection>)>,
    generation: i64,
}

impl SessionReader {
    fn reused(&mut self, path: &[u8], identity: FileIdentity) -> Option<RecordBatch> {
        let (candidate, conn) = self.sources.iter_mut().find(|(c, _)| crate::paths::is_within(path, &c.root))?;
        if conn.is_none() {
            let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            let opened = Connection::open_with_flags(&candidate.snapshot.database, flags).ok()?;
            opened.busy_timeout(std::time::Duration::ZERO).ok()?;
            *conn = Some(opened);
        }
        let conn = conn.as_ref()?;
        conn.execute_batch("BEGIN").ok()?;
        let result = read_listing(conn, &candidate.snapshot.id, path, identity);
        let _ = conn.execute_batch("ROLLBACK");
        result.ok().flatten()
    }
}

fn read_listing(
    conn: &Connection,
    id: &str,
    path: &[u8],
    identity: FileIdentity,
) -> rusqlite::Result<Option<RecordBatch>> {
    // The file may have been replaced by a newer publication since selection.
    let current: Option<String> =
        conn.query_row("SELECT snapshot_id FROM metadata WHERE singleton=1", [], |r| r.get(0)).optional()?;
    if current.as_deref() != Some(id) {
        return Ok(None);
    }
    let parent: Option<i64> = conn
        .prepare_cached(
            "SELECT n.id FROM dirs d JOIN nodes n ON n.id=d.id WHERE d.path=?1 AND d.dev=?2 AND d.inode=?3
             AND n.accessible=1",
        )?
        .query_row(rusqlite::params![path, to_sql_u64(identity.dev), to_sql_u64(identity.ino)], |r| r.get(0))
        .optional()?;
    let Some(parent) = parent else { return Ok(None) };
    let mut statement = conn.prepare_cached(
        "SELECT name,kind,is_mount,size,raw_alloc,dev,inode,links,accessible FROM nodes WHERE parent=?1",
    )?;
    let mut rows = statement.query([parent])?;
    let mut records = RecordBatch::default();
    while let Some(row) = rows.next()? {
        let name: Vec<u8> = row.get(0)?;
        let kind = match row.get::<_, i64>(1)? {
            0 => EntryKind::File,
            1 => EntryKind::Directory,
            2 => EntryKind::Symlink,
            _ => EntryKind::Other,
        };
        // An unreadable subfolder must be retried from the filesystem, not copied as empty.
        if kind == EntryKind::Directory && row.get::<_, i64>(8)? == 0 {
            return Ok(None);
        }
        let is_dir = kind == EntryKind::Directory;
        let (size, allocated): (i64, i64) = (row.get(3)?, row.get(4)?);
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
    Ok(Some(records))
}

impl ListingCacheReader for SessionReader {
    fn generation(&self) -> i64 {
        self.generation
    }

    fn listing(&mut self, path: &[u8], identity: FileIdentity) -> Option<CachedListing> {
        let staged = self.stage.as_mut().and_then(|s| s.listing(path, identity));
        if staged.as_ref().is_some_and(|s| s.records.is_some()) {
            return staged;
        }
        let (revision, generation) = staged.as_ref().map_or((0, self.generation), |s| (s.revision, s.generation));
        match self.reused(path, identity) {
            Some(records) => Some(CachedListing { records: Some(records), revision, generation, import: true }),
            None => staged,
        }
    }
}

/// The paths of saved maps used, for diagnostics.
pub fn candidate_roots(candidates: &[ReuseCandidate]) -> Vec<PathBuf> {
    candidates.iter().map(|c| crate::discovery::native_path(&c.root)).collect()
}
