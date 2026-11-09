//! Per-root mapping sessions (Swift `MappingSession`). A session outlives navigation to other
//! roots: its mapping continues in the background and its remembered folder, selection,
//! collection, collection queue, removal state and pending refresh come back with it.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

use orbit_platform::FileIdentity;

use crate::collection::{CollectJob, CollectionSnapshot};
use crate::folder::PreparedFolder;
use crate::hash::FxHashSet;
use crate::ids::{RequestId, SessionId};
use crate::model::{ProgressSnapshot, ScanRemoval, SnapshotInfo};

/// Removal overlays hold at most this many items; more forces a full refresh.
pub const REMOVAL_OVERLAY_LIMIT: usize = 10_000;

#[derive(Debug)]
pub(crate) struct Remembered {
    pub folder: Arc<PreparedFolder>,
    pub epoch: u64,
    pub selection: Option<Arc<[u8]>>,
}

#[derive(Debug)]
pub(crate) struct Session {
    pub id: SessionId,
    pub root: Arc<[u8]>,
    pub identity: Option<FileIdentity>,
    /// Mapping was stopped (Swift: token cancelled). Not reusable; a new session resumes it.
    pub stopped: bool,
    /// Mapping failed (`scanFailed`).
    pub failed: bool,
    /// Mapping finished and saved.
    pub finished: bool,
    /// A live preview is available (mapping without a kept saved map).
    pub live_preview: bool,
    pub preview_revision: u64,
    pub search_ready: bool,
    pub progress: Option<ProgressSnapshot>,
    pub discovered_files: u64,
    pub cached_files: Option<u64>,
    pub last_route: Option<Vec<u8>>,
    pub remembered: Option<Remembered>,
    pub saved: Option<SnapshotInfo>,
    pub collection: CollectionSnapshot,
    pub job: Option<CollectJob>,
    /// Items being removed from a large collection in the background.
    pub edit: Option<crate::state::CollectionEdit>,
    pub collection_status: Option<String>,
    pub collection_error: Option<String>,
    pub presented_collection_error: Option<String>,
    pub collection_status_revision: u64,
    pub collection_error_revision: u64,
    pub removing: bool,
    removal_status: Option<String>,
    removal_error: Option<String>,
    pub removals: Arc<[ScanRemoval]>,
    pub epoch: u64,
    pub removal_parents: BTreeSet<Vec<u8>>,
    pub removal_versions: HashMap<Vec<u8>, u64>,
    pub recovery_pending: bool,
    pub preview_blocked: bool,
    pub pending_dirs: BTreeSet<Vec<u8>>,
    pub pending_full_refresh: bool,
    pub count_read: Option<RequestId>,
    pub last_count_read: Option<Instant>,
}

impl Session {
    pub fn new(id: SessionId, root: Arc<[u8]>, identity: Option<FileIdentity>, collection: CollectionSnapshot) -> Self {
        Self {
            id,
            root,
            identity,
            stopped: false,
            failed: false,
            finished: false,
            live_preview: false,
            preview_revision: 0,
            search_ready: false,
            progress: None,
            discovered_files: 0,
            cached_files: None,
            last_route: None,
            remembered: None,
            saved: None,
            collection,
            job: None,
            edit: None,
            collection_status: None,
            collection_error: None,
            presented_collection_error: None,
            collection_status_revision: 0,
            collection_error_revision: 0,
            removing: false,
            removal_status: None,
            removal_error: None,
            removals: Arc::from([]),
            epoch: 0,
            removal_parents: BTreeSet::new(),
            removal_versions: HashMap::new(),
            recovery_pending: false,
            preview_blocked: false,
            pending_dirs: BTreeSet::new(),
            pending_full_refresh: false,
            count_read: None,
            last_count_read: None,
        }
    }

    pub fn removal_status(&self) -> Option<&String> {
        self.removal_status.as_ref()
    }

    pub fn removal_error(&self) -> Option<&String> {
        self.removal_error.as_ref()
    }

    /// Changing the removal status supersedes queued collection status (Swift `didSet`).
    pub fn set_removal_status(&mut self, status: Option<String>) {
        if status != self.removal_status {
            self.invalidate_collection_status();
        }
        self.removal_status = status;
    }

    pub fn set_removal_error(&mut self, error: Option<String>) {
        if error != self.removal_error {
            self.collection_error_revision = self.collection_error_revision.wrapping_add(1);
        }
        self.removal_error = error;
    }

    pub fn invalidate_collection_status(&mut self) {
        self.collection_status_revision = self.collection_status_revision.wrapping_add(1);
        self.collection_status = None;
    }

    pub fn enqueue_collection_error(&mut self, message: &str) {
        self.collection_error = Some(match self.collection_error.take() {
            Some(pending) => format!("{pending}\n\n{message}"),
            None => message.to_owned(),
        });
    }

    /// Whether the root of a displayed map is this session's root.
    pub fn owns(&self, root: &[u8], identity: Option<FileIdentity>) -> bool {
        self.identity.is_some() && self.identity == identity && *self.root == *root
    }

    /// Adds removals to the overlay, deduplicated, capped at [`REMOVAL_OVERLAY_LIMIT`].
    /// Returns whether items had to be dropped.
    pub fn merge_removals(&mut self, incoming: impl IntoIterator<Item = ScanRemoval>) -> bool {
        let mut keys: FxHashSet<(Vec<u8>, FileIdentity)> =
            self.removals.iter().map(|r| (r.path.clone(), r.identity)).collect();
        let mut merged: Vec<ScanRemoval> = self.removals.to_vec();
        let mut overflow = merged.len() > REMOVAL_OVERLAY_LIMIT;
        for removal in incoming {
            if keys.insert((removal.path.clone(), removal.identity)) {
                if merged.len() < REMOVAL_OVERLAY_LIMIT {
                    merged.push(removal);
                } else {
                    overflow = true;
                }
            }
        }
        self.removals = merged.into();
        overflow
    }

    pub fn pending_removal(&self) -> bool {
        self.removing
            || self.recovery_pending
            || self.preview_blocked
            || !self.removals.is_empty()
            || !self.removal_parents.is_empty()
    }
}
