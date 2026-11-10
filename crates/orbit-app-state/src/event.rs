//! Inputs ([`Event`]) and outputs ([`Effect`]) of [`crate::AppState::handle`].

use std::cmp::Ordering;
use std::sync::Arc;
use std::time::Duration;

use orbit_sunburst::SegmentIndex;

use crate::backend::{Reply, Request};
use crate::collection::CollectionSnapshot;
use crate::folder::PreparedFolder;
use crate::hash::FxHashSet;
use crate::ids::{DragId, GestureId, LeaseId, RequestId, SessionId};
use crate::model::{Item, ProgressSnapshot, ScanRemoval, SnapshotInfo, SortOrder};
use crate::review::ReviewToken;

/// Everything that can change the state: user intents, backend replies, engine notifications
/// and timers. Paths identify items (rows, search hits, the selection) because they are stable
/// across folder revisions; map segments carry the layout revision they were hit-tested against.
#[derive(Clone, Debug)]
pub enum Event {
    // Navigation.
    /// Open or resume a map of an absolute folder path (toolbar, Places, automation).
    OpenLocation {
        path: Vec<u8>,
        force: bool,
    },
    /// Open a folder (map click, row arrow, row double-click); a file is selected instead.
    Open {
        path: Arc<[u8]>,
    },
    /// The map center / Up: go to the parent folder and keep the departed folder selected.
    GoUp,
    Select {
        path: Option<Arc<[u8]>>,
    },

    // Hover.
    HoverSegment {
        segment: SegmentIndex,
        layout_revision: u64,
    },
    /// The pointer left the map (or is over a gap / the center).
    ClearMapHover,
    HoverRow {
        path: Arc<[u8]>,
    },
    ClearRowHover,

    // Gestures: pointer down on a map segment or row holds updates until pointer up.
    GestureBegin(GestureId),
    GestureEnd(GestureId),

    // Collection drag.
    DragBegin {
        drag: DragId,
        source: DragSource,
        label: String,
    },
    DragOverTray {
        drag: DragId,
        over: bool,
    },
    DragDrop {
        drag: DragId,
    },
    /// The drag ended without a drop on the tray (Escape, dropped elsewhere).
    DragEnd {
        drag: DragId,
    },

    // Search and sorting.
    QueryChanged(String),
    RetrySearch,
    SortChanged(SortOrder),

    // Collection and review.
    Collect {
        paths: Vec<Arc<[u8]>>,
    },
    Uncollect {
        path: Arc<[u8]>,
        review: Option<ReviewToken>,
    },
    ClearCollection {
        review: ReviewToken,
    },
    BeginReview,
    DismissReview {
        review: ReviewToken,
    },
    DismissCollectionError {
        review: ReviewToken,
    },
    /// The permanent-delete confirmation sheet opened; holds collection publication.
    BeginConfirmation {
        review: ReviewToken,
    },
    EndConfirmation {
        lease: LeaseId,
    },
    ConfirmTrash {
        review: ReviewToken,
    },
    ConfirmPermanentDelete {
        review: ReviewToken,
    },

    // Mapping control (footer buttons).
    /// Stop mapping / pause updates (`cancelScan`).
    Pause,
    /// Continue a stopped mapping (`resumeMapping`).
    Resume,
    /// Bring the map up to date as cheaply as its change history allows: replay what changed,
    /// re-check folders that could not be read, rescan only without usable history.
    Refresh,
    /// Read the whole current root again ("Rescan Everything"); only changes are written.
    RescanEverything,
    DismissError,

    // Platform.
    RevealSelection,
    OpenSelection,
    PreviewSelection,

    // Places and volumes.
    AddPlaces(Vec<Vec<u8>>),
    RemovePlace(Vec<u8>),
    RefreshVolumes,

    // Asynchronous results.
    Backend {
        id: RequestId,
        reply: Reply,
    },
    Computed {
        id: RequestId,
        result: Computed,
    },
    Mapping {
        session: SessionId,
        event: MappingEvent,
    },
    Timer(Timer),
}

/// What a collection drag carries. The source stays the pressed item while crossing others.
#[derive(Clone, Debug)]
pub enum DragSource {
    /// A map segment (a node or a group with its real members).
    Segment { segment: SegmentIndex, layout_revision: u64 },
    /// File rows or search results.
    Items(Vec<Arc<[u8]>>),
}

/// Engine notifications for one mapping session.
#[derive(Clone, Debug)]
pub enum MappingEvent {
    /// Discovery attached to a stage.
    DiscoveryStarted,
    Progress(ProgressSnapshot),
    /// The live preview has new data (`revision` increases).
    PreviewChanged {
        revision: u64,
        search_ready: bool,
    },
    /// An existing saved map can be shown while the stage resumes in the background.
    SavedAvailable(SnapshotInfo),
    /// Mapping finished and the map was saved (or an existing saved map was reused).
    Published {
        snapshot: SnapshotInfo,
        reused: bool,
        root_file_count: u64,
    },
    Failed(String),
    /// Monitor batch: folders whose contents changed since the saved map.
    Changes {
        /// Folders whose own entries changed: their listings are repeated.
        directories: Vec<Vec<u8>>,
        /// Folders to reconcile recursively.
        subtrees: Vec<Vec<u8>>,
        full_refresh: bool,
    },
    MonitorUnavailable,
    /// The removal journal reported removals the views must hide until reconciled.
    RemovalReplay {
        removals: Vec<ScanRemoval>,
        affected_parents: Vec<Vec<u8>>,
        full_refresh: bool,
    },
    /// The saved map's search index is ready (shorter search debounce).
    SearchReady,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimerKind {
    /// Search debounce (16 ms when the index is ready, otherwise 100 ms).
    Search,
    /// Re-run a preview search whose preview changed meanwhile (1.5 s).
    SearchRerun,
    /// Debounced refresh of changed folders (1.5 s).
    Refresh,
    /// Throttled discovered-file count read (0.5 s).
    CountRead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timer {
    pub kind: TimerKind,
    pub token: u64,
}

/// CPU work the host runs off the UI thread; [`ComputeJob::run`] returns the result to send back
/// as [`Event::Computed`].
#[derive(Clone)]
pub enum ComputeJob {
    /// Name sort of ≥ 2,000 rows; returns the display order (indices into the rows).
    NameSort { rows: SortRows, compare: fn(&[u8], &[u8]) -> Ordering },
    /// A large collection without `remove`, under a new `revision`.
    CollectionWithout { collection: CollectionSnapshot, remove: Arc<FxHashSet<Arc<[u8]>>>, revision: u64 },
}

impl std::fmt::Debug for ComputeJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameSort { rows, .. } => f.debug_struct("NameSort").field("rows", &rows.len()).finish(),
            Self::CollectionWithout { collection, remove, .. } => f
                .debug_struct("CollectionWithout")
                .field("items", &collection.len())
                .field("remove", &remove.len())
                .finish(),
        }
    }
}

#[derive(Clone)]
pub enum SortRows {
    Folder(Arc<PreparedFolder>),
    Hits(Arc<[Item]>),
}

impl SortRows {
    pub fn len(&self) -> usize {
        match self {
            Self::Folder(folder) => folder.rows().len(),
            Self::Hits(hits) => hits.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn name(&self, row: usize) -> &[u8] {
        match self {
            Self::Folder(folder) => folder.name(folder.rows()[row]),
            Self::Hits(hits) => hits[row].name(),
        }
    }
}

impl ComputeJob {
    pub fn run(self) -> Computed {
        match self {
            Self::NameSort { rows, compare } => Computed::NameOrder(name_order(&rows, compare)),
            Self::CollectionWithout { collection, remove, revision } => {
                let remove: FxHashSet<&[u8]> = remove.iter().map(|path| &**path).collect();
                Computed::Collection(collection.without(&remove, revision))
            }
        }
    }
}

/// Stable order of `rows` by `compare` on names.
pub fn name_order(rows: &SortRows, compare: fn(&[u8], &[u8]) -> Ordering) -> NameOrder {
    let mut order: Vec<u32> = (0..rows.len() as u32).collect();
    order.sort_by(|&a, &b| compare(rows.name(a as usize), rows.name(b as usize)));
    NameOrder::new(order)
}

/// A name order plus its inverse, so "where is row X displayed" is O(1) and "which related row
/// is displayed first" needs no scan of the whole order (1M-row folders, map hover reveal).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameOrder {
    /// Display position → unsorted row.
    order: Arc<[u32]>,
    /// Unsorted row → display position.
    rank: Arc<[u32]>,
    /// Minimum rank of each block of `RANK_BLOCK` consecutive unsorted rows.
    block_min: Arc<[u32]>,
}

const RANK_BLOCK: usize = 1024;

impl NameOrder {
    pub fn new(order: Vec<u32>) -> Self {
        let mut rank = vec![0u32; order.len()];
        for (position, &row) in order.iter().enumerate() {
            rank[row as usize] = position as u32;
        }
        let block_min = rank.chunks(RANK_BLOCK).map(|block| block.iter().copied().min().unwrap_or(u32::MAX)).collect();
        Self { order: order.into(), rank: rank.into(), block_min }
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The unsorted row shown at `display`.
    pub fn row_at(&self, display: usize) -> Option<u32> {
        self.order.get(display).copied()
    }

    /// The display position of unsorted `row`.
    pub fn rank(&self, row: u32) -> Option<u32> {
        self.rank.get(row as usize).copied()
    }

    /// The first display position among unsorted rows `range` (O(block + range / block)).
    pub fn first_rank_in(&self, range: std::ops::Range<u32>) -> Option<u32> {
        let (start, end) = (range.start as usize, (range.end as usize).min(self.rank.len()));
        if start >= end {
            return None;
        }
        let first_full = start.div_ceil(RANK_BLOCK);
        let last_full = end / RANK_BLOCK;
        if first_full >= last_full {
            return self.rank[start..end].iter().copied().min();
        }
        let head = self.rank[start..first_full * RANK_BLOCK].iter().copied();
        let tail = self.rank[last_full * RANK_BLOCK..end].iter().copied();
        let blocks = self.block_min[first_full..last_full].iter().copied();
        head.chain(blocks).chain(tail).min()
    }
}

#[derive(Clone, Debug)]
pub enum Computed {
    NameOrder(NameOrder),
    Collection(CollectionSnapshot),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformAction {
    /// Select in Finder.
    Reveal(Vec<u8>),
    /// Open with the default application.
    Open(Vec<u8>),
    /// Quick Look.
    Preview(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Persist {
    Places(Vec<Vec<u8>>),
    LastLocation(Vec<u8>),
}

/// UI regions whose entities must be notified. Progress-only changes notify only the footer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Regions(u16);

impl Regions {
    pub const NONE: Self = Self(0);
    /// The sunburst (layout, collection marks, hover outline).
    pub const MAP: Self = Self(1);
    /// The Contents list (rows, row hover, selection marks, search state).
    pub const LIST: Self = Self(1 << 1);
    /// Mapping progress footer.
    pub const FOOTER: Self = Self(1 << 2);
    /// Collection tray (items, drag target, validation state).
    pub const TRAY: Self = Self(1 << 3);
    /// Inspector (selection, actions).
    pub const INSPECTOR: Self = Self(1 << 4);
    /// Review / confirmation sheets.
    pub const SHEETS: Self = Self(1 << 5);
    /// Header: location, breadcrumbs, status and error text, toolbar state.
    pub const HEADER: Self = Self(1 << 6);
    /// Sidebar: places and volumes.
    pub const SIDEBAR: Self = Self(1 << 7);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Regions {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

impl std::ops::BitOrAssign for Regions {
    fn bitor_assign(&mut self, other: Self) {
        *self = self.union(other);
    }
}

/// Requests for the host.
#[derive(Clone, Debug)]
pub enum Effect {
    /// Run `request` off the UI thread with [`crate::backend::perform`] and send back its event.
    Backend {
        id: RequestId,
        request: Request,
    },
    /// The state no longer wants request `id`: cancel its `Cancellation` (its reply is ignored).
    Cancel(RequestId),
    Compute {
        id: RequestId,
        job: ComputeJob,
    },
    /// Send `Event::Timer(timer)` after `after`. Superseded timers are ignored when they fire.
    Timer {
        after: Duration,
        timer: Timer,
    },
    Platform(PlatformAction),
    Persist(Persist),
    /// `cx.notify()` the entities of these regions. Emitted once per `handle` call, last.
    Notify(Regions),
}
