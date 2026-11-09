//! Plain data shared between the state machine, the backend and the UI.

use std::sync::Arc;
use std::time::SystemTime;

use orbit_core::paths::file_name;
use orbit_core::view::NodeMeta;
use orbit_platform::FileIdentity;

pub use orbit_core::types::{ScanRemoval, ScanState};

/// A mapped item detached from any tree: a search hit, the selection, a collected item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub path: Arc<[u8]>,
    pub meta: NodeMeta,
}

impl Item {
    pub fn new(path: impl Into<Arc<[u8]>>, meta: NodeMeta) -> Self {
        Self { path: path.into(), meta }
    }

    pub fn name(&self) -> &[u8] {
        let name = file_name(&self.path);
        if name.is_empty() { &self.path } else { name }
    }

    pub fn identity(&self) -> Option<FileIdentity> {
        self.meta.identity
    }

    pub fn is_dir(&self) -> bool {
        self.meta.flags.is_dir
    }

    pub fn display_name(&self) -> String {
        String::from_utf8_lossy(self.name()).into_owned()
    }
}

/// A published, saved map as the app sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// Changes with every publication (refresh, removal reconciliation).
    pub id: u64,
    pub root: Arc<[u8]>,
    pub root_identity: Option<FileIdentity>,
    pub indexed_at: SystemTime,
    pub skipped_count: u64,
    /// Why items were skipped (`"path: reason"`), at most the first 100 the engine kept.
    pub issues: Arc<[String]>,
    /// The saved map was made by an older boot-volume traversal and must be re-mapped with a
    /// preview instead of being kept on screen during the refresh.
    pub needs_boot_upgrade: bool,
}

/// How a discovery estimate was made (footer detail text).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EstimateBasis {
    #[default]
    ThisScan,
    PreviousMap,
    Revised,
}

/// Mapping progress as reported by the engine. Only the footer reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProgressSnapshot {
    /// Monotonic per mapping stage; older snapshots are dropped.
    pub revision: u64,
    pub item_count: u64,
    pub bytes_scanned: u64,
    pub current_path: Vec<u8>,
    pub discovery_complete: bool,
    pub discovery_fraction: Option<f64>,
    pub estimate_basis: EstimateBasis,
    pub cache_fraction: Option<f64>,
    pub cache_finishing: bool,
    pub cached_file_count: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub path: Vec<u8>,
    pub name: String,
    pub capacity: u64,
    pub available: u64,
}

impl Volume {
    /// Shown until discovery reports the real root volume.
    pub fn fallback_root() -> Self {
        Self { path: b"/".to_vec(), name: "Mac".into(), capacity: 0, available: 0 }
    }
}

/// Sort order of the Contents list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    LargestFirst,
    Name,
}
