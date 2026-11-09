//! In-memory recovery targets backed by the real filesystem, for tests.

use super::{RecoveryClock, RecoveryTarget, ReplayTarget};
use crate::journal::{Fence, RemovalCursor, current_identity};
use crate::paths;
use crate::types::{CoreError, ScanRemoval};
use orbit_platform::{Cancellation, FileIdentity, PlatformError};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A clock that only moves when told to; `pause` is free.
#[derive(Debug, Default)]
pub struct FakeClock(AtomicU64);

impl FakeClock {
    pub fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_nanos() as u64, Ordering::SeqCst);
    }
}

impl RecoveryClock for FakeClock {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.0.load(Ordering::SeqCst))
    }

    fn pause(&self) {}
}

fn list_tree(dir: &[u8], rows: &mut BTreeMap<Vec<u8>, FileIdentity>) {
    let Ok(entries) = std::fs::read_dir(orbit_platform::posix::path_from_bytes(dir)) else { return };
    for entry in entries.flatten() {
        let path = paths::join(dir, entry.file_name().as_bytes());
        let Some(identity) = current_identity(&path) else { continue };
        rows.insert(path.clone(), identity);
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            list_tree(&path, rows);
        }
    }
}

fn remove_subtree(rows: &mut BTreeMap<Vec<u8>, FileIdentity>, path: &[u8]) {
    let doomed: Vec<_> = rows.keys().filter(|k| paths::is_within(k, path)).cloned().collect();
    for key in doomed {
        rows.remove(&key);
    }
}

/// A saved map: `rows` is the private working copy, `published` what readers see.
#[derive(Default)]
pub struct FakeMap {
    pub root: Vec<u8>,
    pub rows: BTreeMap<Vec<u8>, FileIdentity>,
    pub published: BTreeMap<Vec<u8>, FileIdentity>,
    pub cursor: Option<RemovalCursor>,
    pub full_refresh: Option<RemovalCursor>,
    pub publishes: usize,
    pub reconciled: Vec<Vec<u8>>,
    pub on_reconcile: Option<Box<dyn FnMut()>>,
}

impl FakeMap {
    pub fn scan(root: &Path) -> Self {
        let root = root.as_os_str().as_bytes().to_vec();
        let mut rows = BTreeMap::new();
        rows.insert(root.clone(), current_identity(&root).unwrap());
        list_tree(&root, &mut rows);
        Self { root, published: rows.clone(), rows, ..Self::default() }
    }
}

impl RecoveryTarget for FakeMap {
    fn root(&self) -> &[u8] {
        &self.root
    }

    fn removal_cursor(&mut self) -> crate::Result<Option<RemovalCursor>> {
        Ok(self.cursor.clone())
    }

    fn indexed_identities(&mut self, paths: &[Vec<u8>]) -> crate::Result<Vec<Option<FileIdentity>>> {
        Ok(paths.iter().map(|p| self.rows.get(p).copied()).collect())
    }

    fn apply_removals(&mut self, removals: &[ScanRemoval], _cancel: &Cancellation) -> crate::Result<()> {
        for removal in removals {
            if self.rows.get(&removal.path) == Some(&removal.identity) {
                remove_subtree(&mut self.rows, &removal.path);
            }
        }
        Ok(())
    }

    fn reconcile_directories(&mut self, directories: &[Vec<u8>], cancel: &Cancellation) -> crate::Result<()> {
        if let Some(hook) = self.on_reconcile.as_mut() {
            hook();
        }
        for dir in directories {
            if cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            if current_identity(dir) != self.rows.get(dir).copied() {
                return Err(PlatformError::IdentityChanged {
                    path: Path::new(std::ffi::OsStr::from_bytes(dir)).into(),
                }
                .into());
            }
            self.reconciled.push(dir.clone());
            let children: Vec<_> =
                self.rows.keys().filter(|k| paths::parent(k) == Some(dir.as_slice())).cloned().collect();
            for child in children {
                if current_identity(&child) != self.rows.get(&child).copied() {
                    remove_subtree(&mut self.rows, &child);
                }
            }
            let mut fresh = BTreeMap::new();
            list_tree(dir, &mut fresh);
            for (path, identity) in fresh {
                self.rows.entry(path).or_insert(identity);
            }
        }
        Ok(())
    }

    fn require_full_refresh(&mut self, cursor: &RemovalCursor) -> crate::Result<()> {
        self.rows = self.published.clone();
        self.full_refresh = Some(cursor.clone());
        Ok(())
    }

    fn publish(&mut self, cursor: &RemovalCursor, fence: &Fence<'_>) -> crate::Result<()> {
        assert!(fence.is_at(cursor), "publish called outside the high-water rule");
        self.published = self.rows.clone();
        self.cursor = Some(cursor.clone());
        self.publishes += 1;
        Ok(())
    }
}

/// A mapping stage: dirty directories are "written" when [`FakeStage::drain`] runs.
#[derive(Default)]
pub struct FakeStage {
    pub root: Vec<u8>,
    pub cursor: Option<RemovalCursor>,
    pub fresh: bool,
    pub replay_parents: BTreeSet<Vec<u8>>,
    pub dirty: BTreeSet<Vec<u8>>,
    pub full_refresh: bool,
    pub async_discovery: bool,
    pub cursor_writes: usize,
}

impl FakeStage {
    pub fn new(root: &Path, fresh: bool) -> Self {
        Self { root: root.as_os_str().as_bytes().to_vec(), fresh, ..Self::default() }
    }

    /// The writer processes all queued work.
    pub fn drain(&mut self) {
        self.dirty.clear();
        self.full_refresh = false;
    }
}

impl ReplayTarget for FakeStage {
    fn root(&self) -> &[u8] {
        &self.root
    }

    fn removal_cursor(&mut self) -> crate::Result<Option<RemovalCursor>> {
        Ok(self.cursor.clone())
    }

    fn write_removal_cursor(&mut self, cursor: &RemovalCursor) -> crate::Result<()> {
        self.cursor = Some(cursor.clone());
        self.cursor_writes += 1;
        Ok(())
    }

    fn is_fresh_for_removal_anchor(&mut self) -> crate::Result<bool> {
        Ok(self.fresh)
    }

    fn reset_replay_parents(&mut self) -> crate::Result<()> {
        self.replay_parents.clear();
        Ok(())
    }

    fn add_replay_parents(&mut self, parents: &[Vec<u8>]) -> crate::Result<()> {
        self.replay_parents.extend(parents.iter().cloned());
        Ok(())
    }

    fn replay_parents(&mut self, after: Option<&[u8]>, limit: usize) -> crate::Result<Vec<Vec<u8>>> {
        Ok(self.replay_parents.iter().filter(|p| after.is_none_or(|a| p.as_slice() > a)).take(limit).cloned().collect())
    }

    fn mark_changed(&mut self, directory: &[u8]) -> crate::Result<()> {
        self.dirty.insert(directory.to_vec());
        Ok(())
    }

    fn mark_full_refresh(&mut self) -> crate::Result<()> {
        self.full_refresh = true;
        Ok(())
    }

    fn has_replay_dirty_work(&mut self) -> crate::Result<bool> {
        Ok(!self.dirty.is_empty() || self.full_refresh)
    }

    fn has_pending_directories(&mut self) -> crate::Result<bool> {
        Ok(false)
    }

    fn has_active_async_discovery(&self) -> bool {
        self.async_discovery
    }

    fn validate_root(&mut self) -> crate::Result<()> {
        Ok(())
    }
}
