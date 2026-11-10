//! What the app provides to the automation [`Controller`](crate::Controller).
//!
//! The controller owns the protocol: argument validation, map-context checks, item resolution by
//! `{path, identity}`, immutable page snapshots, removal plan tokens and response JSON. The model
//! owns app state and performs the primitive operations below on the UI thread.

use std::path::Path;
use std::time::{Instant, SystemTime};

/// Identifies one opened map. Rendered as an uppercase UUID in `context`; any action carrying a
/// different context is rejected as `stale_context`.
pub type SessionId = uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanState {
    Pending,
    Scanning,
    Complete,
}

impl ScanState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Scanning => "scanning",
            Self::Complete => "complete",
        }
    }
}

/// One listed item as reported to clients (`path`, `identity`, sizes, flags).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemInfo {
    /// Absolute path bytes; the item's id. Clients must echo it back exactly, so items whose
    /// path is not UTF-8 are listed (lossily) but can never be acted on.
    pub path: Vec<u8>,
    /// Opaque file identity; `None` while unknown. Actions require it to match.
    pub identity: Option<String>,
    pub name: Vec<u8>,
    pub directory: bool,
    pub symbolic_link: bool,
    pub accessible: bool,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub file_count: u64,
    pub scan_state: ScanState,
    /// Scan of this item is complete and no removal size update is pending for it.
    pub sizes_complete: bool,
    pub can_collect: bool,
}

/// Flags and counters read at the start of a command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelState {
    /// A map is opening; no context is available yet.
    pub opening: bool,
    pub scanning: bool,
    pub refreshing: bool,
    /// The map is partial (`mappingIncomplete`).
    pub partial_map: bool,
    pub discovered_files: u64,
    pub discovered_bytes: u64,
    /// The UI search query; empty when showing folder contents.
    pub query: String,
    /// A non-empty query is applied (rows are search results).
    pub filtering: bool,
    pub searching: bool,
    pub search_outdated: bool,
    pub search_failed: bool,
    pub search_match_count: u64,
    /// Rows are being re-sorted; a new list snapshot would be inconsistent.
    pub sorting_rows: bool,
    /// More than one folder on the navigation stack.
    pub can_go_back: bool,
    pub collection_count: u64,
    pub collection_bytes: u64,
    pub validating_collection: bool,
    pub updating_collection: bool,
    /// This session is removing files (blocks collection edits, refresh and removal).
    pub removing: bool,
    /// Any removal is running, in any session (`status.removing`).
    pub active_removals: bool,
    pub updating_sizes: bool,
    pub message: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemovalMode {
    Trash,
    Permanent,
}

impl RemovalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trash => "trash",
            Self::Permanent => "permanent",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "trash" => Some(Self::Trash),
            "permanent" => Some(Self::Permanent),
            _ => None,
        }
    }
}

/// Progress of a removal started by automation (`status.lastRemoval`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemovalReceipt {
    pub running: bool,
    pub removed_count: Option<u64>,
    pub message: String,
    pub error: Option<String>,
}

/// Outcome of [`AutomationModel::start_removal`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalStart<R> {
    /// The removal is running; the receipt reports its outcome later.
    Started(R),
    /// A removal or a collection confirmation is already in progress. Nothing changed and the
    /// plan stays valid.
    Busy,
    /// The removal did not start. The plan is consumed.
    NotStarted,
}

/// The app state and primitive operations the automation controller drives.
///
/// Called on the thread that owns the model (the UI thread); every call reflects state at that
/// moment. Asynchronous work (scan, search, collection validation, removal) starts and returns.
pub trait AutomationModel {
    /// A cheap handle to a mapped item, kept in page snapshots and removal plans.
    type Node: Clone;
    /// A collection review token; detects collection changes between review and removal.
    type Review: Clone;
    /// Identifies a removal started by automation.
    type Receipt: Clone;

    /// The active map's session, or `None` while opening or without a map.
    fn session(&self) -> Option<SessionId>;
    fn state(&self) -> ModelState;
    fn root(&self) -> Option<Self::Node>;
    /// The folder shown in the list.
    fn current_folder(&self) -> Option<Self::Node>;
    fn describe(&self, node: &Self::Node) -> ItemInfo;
    /// The rows of the list in UI order: folder children or search results.
    fn displayed_rows(&self) -> Vec<Self::Node>;
    /// The collection in UI order.
    fn collection(&self) -> Vec<Self::Node>;
    /// The mapped item at `path` in the active map, if any.
    fn find(&self, path: &[u8]) -> Option<Self::Node>;
    fn removal_receipt(&self, receipt: &Self::Receipt) -> Option<RemovalReceipt>;
    /// The collection is unchanged since `review` was issued.
    fn review_matches(&self, review: &Self::Review) -> bool;

    /// Opens or resumes a map of the absolute folder `path`.
    fn scan(&mut self, path: &Path);
    /// Applies a new search query (empty restores folder contents).
    fn set_query(&mut self, query: &str);
    /// Re-runs the current query (used when the same query is submitted again).
    fn retry_search(&mut self);
    fn open(&mut self, folder: &Self::Node);
    fn back(&mut self);
    /// Selects `node`; returns whether the selection now is exactly this path and identity.
    fn select(&mut self, node: &Self::Node) -> bool;
    fn reveal_selection(&mut self);
    fn preview_selection(&mut self);
    /// `everything`: read the whole location again rather than only what changed.
    fn refresh(&mut self, everything: bool);
    fn stop_scan(&mut self);
    /// Queues `nodes` for collection; `false` if the model refused them.
    fn collect(&mut self, nodes: &[Self::Node]) -> bool;
    fn uncollect(&mut self, node: &Self::Node);
    /// The current review token, beginning a review if none is open. `None` when empty.
    fn collection_review(&mut self) -> Option<Self::Review>;
    fn clear_collection(&mut self, review: &Self::Review);
    /// Starts removing the reviewed collection through the app's normal removal pipeline.
    fn start_removal(
        &mut self,
        review: &Self::Review,
        mode: RemovalMode,
        session: SessionId,
    ) -> RemovalStart<Self::Receipt>;
}

/// Time source for snapshot and token expiry.
pub trait Clock {
    /// Monotonic time for expiry.
    fn now(&self) -> Instant;
    /// Wall time reported in `snapshotAt` / `expiresAt`.
    fn wall(&self) -> SystemTime;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }
}
