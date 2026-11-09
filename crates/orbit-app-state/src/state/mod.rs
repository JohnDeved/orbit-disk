//! [`AppState`]: the deterministic application state machine (port of Swift `DiskModel`).
//!
//! `handle(event, now)` mutates the state and returns [`Effect`]s; it never blocks, spawns or
//! sleeps. Every asynchronous request is registered in `pending` under its [`RequestId`] with the
//! tokens it was issued under (generation, browse generation, resolution, session, removal epoch,
//! snapshot, collection revision). A reply whose request is no longer pending, or whose tokens
//! changed, is dropped — the Swift `installFolder` / `presentationTransition` guards.
//!
//! Tokens:
//! - `generation`: a mapping attachment (open location, pause). Results of older ones are ignored.
//! - `browse`: one navigation inside a map. Folder reads for older navigations are ignored.
//! - `resolution`: one open-location request; only the latest resolution may switch roots.
//! - session `epoch`: bumps whenever the removal overlay changes; reads made under an older
//!   overlay are retried.
//! - snapshot id: a saved-map read made against an older snapshot is retried.

mod collect;
mod interaction;
mod mapping;
mod removal;
mod search;

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use orbit_core::paths::{file_name, is_within};
use orbit_core::view::{FolderTree, NodeIx};
use orbit_platform::FileIdentity;
use orbit_sunburst::PresentationCache;

use crate::backend::{Reply, Request};
use crate::collection::{CollectionSnapshot, default_protected};
use crate::event::{Effect, Event, Persist, PlatformAction, Regions, Timer, TimerKind};
use crate::folder::{ColorState, PreparedFolder};
use crate::footer::{FooterInputs, FooterState, compute as compute_footer};
use crate::hash::FxHashMap;
use crate::ids::{DragId, GestureId, IdGen, LeaseId, RemovalId, RequestId, SessionId};
use crate::model::{Item, SnapshotInfo, SortOrder, Volume};
use crate::places::{Places, selected_place};
use crate::review::ReviewToken;
use crate::session::Session;

pub use collect::BACKGROUND_EDIT_ITEMS;
pub(crate) use collect::CollectionEdit;
pub use interaction::{HoverPresentation, RowView};
pub(crate) use mapping::Deferred;
pub use removal::Receipt;
pub(crate) use search::SearchState;

/// Saved folders kept for quick re-display (Swift `folderCache`, 24 entries).
pub const FOLDER_CACHE_LIMIT: usize = 24;
/// Cached folders with fewer rows are prepared synchronously on navigation.
pub const SYNC_PREPARE_LIMIT: usize = 256;
/// Debounce of monitor batches before refreshing changed folders the user can see (inside the
/// displayed folder, its ancestors, or anywhere while searching or reviewing the collection).
pub const REFRESH_DELAY: Duration = Duration::from_millis(1_500);
/// Delays of background refreshes (changes only outside what is shown). Each background refresh
/// moves one step further while changes keep arriving; [`BACKGROUND_QUIET`] without changes starts
/// over. Navigating into a changed folder, searching or reviewing the collection flushes them.
pub const BACKGROUND_REFRESH_DELAYS: [Duration; 3] =
    [Duration::from_secs(5), Duration::from_secs(10), Duration::from_secs(30)];
/// A change after this long without changes resets the background backoff.
pub const BACKGROUND_QUIET: Duration = Duration::from_secs(30);
/// Minimum interval between discovered-file count reads of a hidden mapping.
pub const COUNT_READ_INTERVAL: Duration = Duration::from_millis(500);

pub const STATUS_CHECKING_COLLECTION: &str = "Checking items for collection…";

#[derive(Clone, Debug)]
pub struct Config {
    /// The user's home folder (protected-path rules, `~` abbreviation, the "Home" footer name).
    pub home: Vec<u8>,
    /// Folders whose contents are never collectable.
    pub protected: Vec<Vec<u8>>,
    /// Name collation for the name sort (Finder order). Injected; `orbit-core::collate` provides it.
    pub compare_names: fn(&[u8], &[u8]) -> Ordering,
    /// Beyond this many changed parents, removals force a full refresh.
    pub recovery_parent_limit: usize,
    /// Saved Places (loaded by the host).
    pub places: Vec<Vec<u8>>,
    /// Seed for session ids (random in the app, fixed in tests).
    pub id_seed: u64,
}

impl Config {
    pub fn new(home: impl Into<Vec<u8>>) -> Self {
        let home = home.into();
        Self {
            protected: default_protected(&home),
            home,
            compare_names: |a, b| a.cmp(b),
            recovery_parent_limit: 10_000,
            places: Vec::new(),
            id_seed: 0,
        }
    }
}

/// What a pending request was issued for, with the tokens it captured.
#[derive(Debug)]
pub(crate) enum Pending {
    Resolve {
        resolution: u64,
        path: Vec<u8>,
        force: bool,
        query: String,
        candidate: Option<(SessionId, u64)>,
    },
    PreviewRead {
        generation: u64,
        browse: u64,
        session: SessionId,
        epoch: u64,
    },
    PreviewFolder {
        generation: u64,
        browse: u64,
        session: SessionId,
        epoch: u64,
        path: Vec<u8>,
    },
    SavedFolder {
        generation: u64,
        browse: u64,
        epoch: u64,
        snapshot: SnapshotInfo,
        path: Vec<u8>,
        selection: Option<Arc<[u8]>>,
    },
    Finish {
        generation: u64,
        session: SessionId,
        epoch: u64,
        snapshot: SnapshotInfo,
        reused: bool,
    },
    Baseline {
        generation: u64,
        session: SessionId,
        epoch: u64,
        snapshot: SnapshotInfo,
        route: Vec<u8>,
    },
    Count {
        session: SessionId,
        epoch: u64,
    },
    Search {
        generation: u64,
        session: Option<SessionId>,
        epoch: u64,
        query: String,
        snapshot: Option<u64>,
        stable: bool,
    },
    Validate(collect::PendingValidation),
    Removal {
        removal: RemovalId,
    },
    Refresh {
        generation: u64,
        session: Option<SessionId>,
        epoch: u64,
        paths: Vec<Vec<u8>>,
        versions: Vec<(Vec<u8>, u64)>,
        /// A background refresh of changes outside what is shown (no status, no reload).
        quiet: bool,
    },
    Volumes,
    Places,
    Sort {
        revision: u64,
    },
    CollectionEdit {
        session: SessionId,
        /// Collection revision the edit started from.
        base: u64,
    },
}

/// The collection drag in progress.
#[derive(Debug)]
pub(crate) struct Drag {
    pub id: DragId,
    pub root: Arc<[u8]>,
    pub root_identity: Option<FileIdentity>,
    pub candidates: crate::collection::Candidates,
    pub group: Option<Arc<crate::collection::GroupContext>>,
    pub label: String,
    pub over_tray: bool,
}

/// Validation of the displayed session's queue, shown in the tray and status.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AttachedValidation {
    pub session: SessionId,
    pub job: u64,
    pub status_revision: Option<u64>,
}

pub struct AppState {
    cfg: Config,
    ids: IdGen,
    pub(crate) now: Instant,
    sessions: Vec<Session>,
    active: Option<SessionId>,
    generation: u64,
    browse: u64,
    resolution: u64,
    pending: FxHashMap<RequestId, Pending>,

    // The displayed map.
    folder: Option<Arc<PreparedFolder>>,
    map_revision: u64,
    colors: ColorState,
    snapshot: Option<SnapshotInfo>,
    partial: bool,
    preview_session: Option<SessionId>,
    scanning: bool,
    refreshing: bool,
    opening: bool,
    updates_paused: bool,
    saved_map: bool,
    title: &'static str,
    requested_path: Option<Vec<u8>>,
    requested_target: Option<(Vec<u8>, FileIdentity)>,
    last_preview: Option<(Vec<u8>, u64)>,
    preview_read: Option<RequestId>,
    preview_read_again: Option<bool>,
    folder_load: Option<RequestId>,
    folder_cache: VecDeque<Arc<FolderTree>>,
    resolution_request: Option<RequestId>,
    pending_dirs: BTreeSet<Vec<u8>>,
    pending_full_refresh: bool,
    refresh_timer: Option<u64>,
    /// When the pending refresh timer fires and how urgent its changes are.
    refresh_due: Option<(Instant, mapping::RefreshUrgency)>,
    /// Pending changes became visible (navigation, search, review): refresh without delay.
    refresh_flush: bool,
    /// The running refresh only covers changes outside what is shown: no status, no spinner.
    quiet_refresh: bool,
    /// Index into [`BACKGROUND_REFRESH_DELAYS`].
    background_step: usize,
    /// When the monitor last reported changes.
    last_change: Option<Instant>,
    /// What was shown when refresh urgency was last evaluated (see `mapping::RefreshFocus`).
    refresh_focus: mapping::RefreshFocus,
    refresh_request: Option<RequestId>,
    removal_result_status: Option<String>,
    monitored: Option<SessionId>,
    count_timer: Option<u64>,

    // Interaction.
    selected: Option<Item>,
    map_hover: interaction::MapHover,
    row_hover: Option<Arc<[u8]>>,
    hover: Arc<HoverPresentation>,
    hover_dirty: bool,
    gesture: Option<GestureId>,
    deferred: Deferred,
    drag: Option<Drag>,
    sort: SortOrder,
    order: Option<crate::event::NameOrder>,
    sorting: bool,
    sort_request: Option<RequestId>,
    sort_revision: u64,
    list_revision: u64,
    search: SearchState,
    presentation: PresentationCache<NodeIx>,

    // Collection review.
    show_review: bool,
    review_id: u64,
    confirmation: Option<(LeaseId, ReviewToken)>,
    validation: Option<AttachedValidation>,

    // Removal.
    removals_running: BTreeMap<RemovalId, removal::Running>,
    receipts: Vec<(RemovalId, Receipt)>,

    // Feedback.
    status: Option<String>,
    status_revision: u64,
    error: Option<String>,
    error_revision: u64,
    paused_revision: u64,
    suppress_feedback: bool,
    present_pending_error: bool,

    footer: Option<FooterState>,
    item_count: u64,
    progress_bytes: u64,
    scan_path: Vec<u8>,
    places: Places,
    volumes: Vec<Volume>,
    discovering_volumes: bool,
    volume_request: Option<RequestId>,
    volume_rerun: bool,

    out: Vec<Effect>,
    dirty: Regions,
    pub(crate) automation_effects: Vec<Effect>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("active", &self.active)
            .field("folder", &self.folder.as_ref().map(|f| String::from_utf8_lossy(f.folder_path()).into_owned()))
            .field("sessions", &self.sessions.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl AppState {
    pub fn new(config: Config, now: Instant) -> Self {
        let mut ids = IdGen::new(config.id_seed);
        let colors = ColorState { folder: Arc::from(&b""[..]), revision: ids.next(), ordinals: Arc::default() };
        let places = Places::new(config.places.iter().cloned(), &config.home);
        Self {
            cfg: config,
            now,
            sessions: Vec::new(),
            active: None,
            generation: ids.next(),
            browse: ids.next(),
            resolution: ids.next(),
            pending: FxHashMap::default(),
            folder: None,
            map_revision: ids.next(),
            colors,
            snapshot: None,
            partial: false,
            preview_session: None,
            scanning: false,
            refreshing: false,
            opening: false,
            updates_paused: false,
            saved_map: false,
            title: mapping::TITLE_MAPPING,
            requested_path: None,
            requested_target: None,
            last_preview: None,
            preview_read: None,
            preview_read_again: None,
            folder_load: None,
            folder_cache: VecDeque::new(),
            resolution_request: None,
            pending_dirs: BTreeSet::new(),
            pending_full_refresh: false,
            refresh_timer: None,
            refresh_due: None,
            refresh_flush: false,
            quiet_refresh: false,
            background_step: 0,
            last_change: None,
            refresh_focus: mapping::RefreshFocus::default(),
            refresh_request: None,
            removal_result_status: None,
            monitored: None,
            count_timer: None,
            selected: None,
            map_hover: interaction::MapHover::None,
            row_hover: None,
            hover: Arc::default(),
            hover_dirty: false,
            gesture: None,
            deferred: Deferred::default(),
            drag: None,
            sort: SortOrder::LargestFirst,
            order: None,
            sorting: false,
            sort_request: None,
            sort_revision: ids.next(),
            list_revision: ids.next(),
            search: SearchState::default(),
            presentation: PresentationCache::new(),
            show_review: false,
            review_id: ids.next(),
            confirmation: None,
            validation: None,
            removals_running: BTreeMap::new(),
            receipts: Vec::new(),
            status: None,
            status_revision: 0,
            error: None,
            error_revision: 0,
            paused_revision: 0,
            suppress_feedback: false,
            present_pending_error: false,
            footer: None,
            item_count: 0,
            progress_bytes: 0,
            scan_path: Vec::new(),
            places,
            volumes: vec![Volume::fallback_root()],
            discovering_volumes: true,
            volume_request: None,
            volume_rerun: false,
            out: Vec::new(),
            dirty: Regions::NONE,
            automation_effects: Vec::new(),
            ids,
        }
    }

    /// Effects for app launch: volume discovery and reopening the last location.
    pub fn launch(&mut self, last_location: Option<Vec<u8>>, now: Instant) -> Vec<Effect> {
        self.now = now;
        self.refresh_volumes();
        if let Some(path) = last_location {
            self.open_location(path, false);
        }
        self.finish_step()
    }

    /// Applies one event. Never blocks; returns what the host must do next.
    pub fn handle(&mut self, event: Event, now: Instant) -> Vec<Effect> {
        self.now = now;
        match event {
            Event::OpenLocation { path, force } => self.open_location(path, force),
            Event::Open { path } => self.open(&path),
            Event::GoUp => self.go_up(),
            Event::Select { path } => self.select(path),
            Event::HoverSegment { segment, layout_revision } => self.hover_segment(segment, layout_revision),
            Event::ClearMapHover => self.clear_map_hover(),
            Event::HoverRow { path } => self.hover_row(Some(path)),
            Event::ClearRowHover => self.hover_row(None),
            Event::GestureBegin(id) => self.begin_gesture(id),
            Event::GestureEnd(id) => self.end_gesture(id),
            Event::DragBegin { drag, source, label } => self.begin_drag(drag, source, label),
            Event::DragOverTray { drag, over } => self.set_drag_over_tray(drag, over),
            Event::DragDrop { drag } => self.drop_drag(drag),
            Event::DragEnd { drag } => self.end_drag(drag),
            Event::QueryChanged(query) => self.set_query(query),
            Event::RetrySearch => self.retry_search(),
            Event::SortChanged(order) => self.set_sort(order),
            Event::Collect { paths } => {
                self.collect_paths(&paths);
            }
            Event::Uncollect { path, review } => self.uncollect(&path, review.as_ref()),
            Event::ClearCollection { review } => self.clear_collection(&review),
            Event::BeginReview => {
                self.begin_review();
            }
            Event::DismissReview { review } => self.dismiss_review(&review),
            Event::DismissCollectionError { review } => self.dismiss_collection_error(&review),
            Event::BeginConfirmation { review } => {
                self.begin_confirmation(&review);
            }
            Event::EndConfirmation { lease } => self.end_confirmation(lease),
            Event::ConfirmTrash { review } => self.confirm_removal(&review, false),
            Event::ConfirmPermanentDelete { review } => self.confirm_removal(&review, true),
            Event::Pause => self.pause(),
            Event::Resume => self.resume(),
            Event::Refresh => self.rescan(),
            Event::DismissError => self.set_error(None),
            Event::RevealSelection => {
                if let Some(item) = &self.selected {
                    self.out.push(Effect::Platform(PlatformAction::Reveal(item.path.to_vec())));
                }
            }
            Event::OpenSelection => {
                if let Some(item) = self.selected.clone() {
                    if item.is_dir() {
                        self.open(&item.path);
                    } else {
                        self.out.push(Effect::Platform(PlatformAction::Open(item.path.to_vec())));
                    }
                }
            }
            Event::PreviewSelection => {
                if let Some(item) = self.selected.as_ref().filter(|item| !item.is_dir()) {
                    self.out.push(Effect::Platform(PlatformAction::Preview(item.path.to_vec())));
                }
            }
            Event::AddPlaces(paths) => {
                self.request(Pending::Places, Request::CheckPlaces { paths });
            }
            Event::RemovePlace(path) => {
                if self.places.remove(&path) {
                    self.out.push(Effect::Persist(Persist::Places(self.places.folders().to_vec())));
                    self.dirty |= Regions::SIDEBAR;
                }
            }
            Event::RefreshVolumes => self.refresh_volumes(),
            Event::Backend { id, reply } => self.on_reply(id, reply),
            Event::Computed { id, result } => self.on_computed(id, result),
            Event::Mapping { session, event } => self.on_mapping(session, event),
            Event::Timer(timer) => self.on_timer(timer),
        }
        self.finish_step()
    }

    /// Runs a command that returns a value (automation) as one step.
    pub(crate) fn step<R>(&mut self, now: Instant, command: impl FnOnce(&mut Self) -> R) -> (R, Vec<Effect>) {
        self.now = now;
        let result = command(self);
        (result, self.finish_step())
    }

    /// End of every step: deferred feedback, hover presentation, footer, notify.
    fn finish_step(&mut self) -> Vec<Effect> {
        if self.present_pending_error {
            self.present_pending_error = false;
            self.present_pending_collection_error();
        }
        if self.hover_dirty {
            self.hover_dirty = false;
            let hover = self.make_hover_presentation();
            if *self.hover != hover {
                self.hover = Arc::new(hover);
                self.dirty |= Regions::LIST | Regions::MAP;
            }
        }
        self.reprioritize_refresh();
        self.refresh_footer();
        let mut out = std::mem::take(&mut self.out);
        let dirty = std::mem::take(&mut self.dirty);
        if !dirty.is_empty() {
            out.push(Effect::Notify(dirty));
        }
        out
    }

    fn on_reply(&mut self, id: RequestId, reply: Reply) {
        let Some(pending) = self.pending.remove(&id) else { return };
        if matches!(reply, Reply::Cancelled) {
            self.on_cancelled(id, pending);
            return;
        }
        match (pending, reply) {
            (Pending::Resolve { resolution, path, force, query, candidate }, Reply::Resolved(result)) => {
                self.on_resolved(id, resolution, path, force, query, candidate, result)
            }
            (pending @ (Pending::PreviewRead { .. } | Pending::PreviewFolder { .. }), Reply::View(view)) => {
                self.on_preview_view(id, pending, view)
            }
            (
                pending @ (Pending::SavedFolder { .. } | Pending::Finish { .. } | Pending::Baseline { .. }),
                Reply::View(view),
            ) => self.on_saved_view(id, pending, view),
            (Pending::Count { session, epoch }, Reply::Count(count)) => self.on_count(id, session, epoch, count),
            (pending @ Pending::Search { .. }, Reply::Search(result)) => self.on_search(id, pending, result),
            (Pending::Validate(pending), Reply::Validated(result)) => self.on_validated(id, pending, result),
            (Pending::Removal { removal }, Reply::Removed(outcome)) => self.on_removed(removal, outcome),
            (pending @ Pending::Refresh { .. }, Reply::Refreshed(result)) => self.on_refreshed(id, pending, result),
            (Pending::Volumes, Reply::Volumes(volumes)) => self.on_volumes(id, volumes),
            (Pending::Places, Reply::Places(directories)) => {
                if self.places.add(directories) {
                    self.out.push(Effect::Persist(Persist::Places(self.places.folders().to_vec())));
                    self.dirty |= Regions::SIDEBAR;
                }
            }
            _ => {}
        }
    }

    fn on_cancelled(&mut self, id: RequestId, pending: Pending) {
        match pending {
            // A cancelled validation of a live job is retried (the job itself was not cancelled).
            Pending::Validate(pending) => self.on_validation_lost(id, pending),
            Pending::Volumes => {
                self.volume_request = None;
                self.volume_rerun = false;
            }
            _ => {}
        }
    }

    fn on_timer(&mut self, timer: Timer) {
        match timer.kind {
            TimerKind::Search => self.on_search_timer(timer.token),
            TimerKind::SearchRerun => self.on_search_rerun(timer.token),
            TimerKind::Refresh => {
                if self.refresh_timer == Some(timer.token) {
                    self.refresh_timer = None;
                    let urgency = self.refresh_due.take().map(|(_, urgency)| urgency);
                    self.refresh_changed_folders(urgency);
                }
            }
            TimerKind::CountRead => {
                if self.count_timer == Some(timer.token) {
                    self.count_timer = None;
                    self.read_discovered_count();
                }
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Effects plumbing.

    pub(crate) fn token(&mut self) -> u64 {
        self.ids.next()
    }

    pub(crate) fn request(&mut self, pending: Pending, request: Request) -> RequestId {
        let id = RequestId(self.ids.next());
        self.pending.insert(id, pending);
        self.out.push(Effect::Backend { id, request });
        id
    }

    /// Fire-and-forget backend call.
    pub(crate) fn send(&mut self, request: Request) {
        let id = RequestId(self.ids.next());
        self.out.push(Effect::Backend { id, request });
    }

    pub(crate) fn cancel(&mut self, id: Option<RequestId>) {
        if let Some(id) = id
            && self.pending.remove(&id).is_some()
        {
            self.out.push(Effect::Cancel(id));
        }
    }

    pub(crate) fn timer(&mut self, kind: TimerKind, after: Duration) -> u64 {
        let token = self.ids.next();
        self.out.push(Effect::Timer { after, timer: Timer { kind, token } });
        token
    }

    // ---------------------------------------------------------------------------------------
    // Sessions.

    pub(crate) fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub(crate) fn session_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub(crate) fn session_by_root(&self, root: &[u8]) -> Option<&Session> {
        self.sessions.iter().find(|s| *s.root == *root)
    }

    pub(crate) fn active_session(&self) -> Option<&Session> {
        self.active.and_then(|id| self.session(id))
    }

    pub(crate) fn active_session_mut(&mut self) -> Option<&mut Session> {
        let id = self.active?;
        self.session_mut(id)
    }

    /// Changes whenever the displayed session's removal overlay changes.
    pub fn active_epoch(&self) -> u64 {
        self.active_session().map_or(0, |s| s.epoch)
    }

    pub(crate) fn displayed_root(&self) -> Option<(&[u8], Option<FileIdentity>)> {
        self.folder.as_ref().map(|folder| (folder.root_path(), folder.root_meta().identity))
    }

    /// The displayed map belongs to the active session (Swift `displayedRootMatchesActiveSession`).
    pub(crate) fn displayed_matches_active(&self) -> bool {
        let (Some((root, identity)), Some(session)) = (self.displayed_root(), self.active_session()) else {
            return false;
        };
        session.owns(root, identity)
    }

    /// Sizes of folders may still include removed items (Swift `isUpdatingRemovalSizes`).
    pub fn updating_removal_sizes(&self) -> bool {
        self.active_session().is_some_and(|s| s.removing || s.recovery_pending || !s.removals.is_empty())
    }

    /// Whether the size of the folder at `path` may still include removed items, so it shows
    /// "Updating…" (Swift `isUpdatingRemovalSize(of:)`, narrowed to folders that contain a
    /// removed or removing item or a folder whose contents changed). Files never update. While
    /// the journal recovery or a full refresh is pending, every folder may be affected.
    pub fn updating_removal_size(&self, path: &[u8], is_dir: bool) -> bool {
        if !is_dir || !self.updating_removal_sizes() {
            return false;
        }
        let Some(session) = self.active_session() else { return false };
        if session.recovery_pending || session.pending_full_refresh {
            return true;
        }
        let contains = |inner: &[u8]| inner != path && is_within(inner, path);
        session.removals.iter().any(|removal| contains(&removal.path))
            || session.removal_parents.iter().any(|parent| is_within(parent, path))
            || (session.removing
                && self
                    .removals_running
                    .values()
                    .filter(|running| *running.root == *session.root)
                    .any(|running| running.items.keys().any(|item| contains(item))))
    }

    pub(crate) fn begin_review_command(&mut self) -> Option<ReviewToken> {
        self.begin_review()
    }

    pub(crate) fn start_removal_command(&mut self, permanently: bool) -> Option<RemovalId> {
        self.start_removal(permanently)
    }

    /// The displayed session's removal is running (Swift `isRemoving`).
    pub fn is_removing(&self) -> bool {
        self.active_session().is_some_and(|s| s.removing)
    }

    // ---------------------------------------------------------------------------------------
    // Feedback (status and error lines) with Swift's revision rules.

    fn active_feedback_session_id(&self) -> Option<SessionId> {
        if self.suppress_feedback || !self.displayed_matches_active() {
            return None;
        }
        self.active
    }

    pub(crate) fn set_status(&mut self, text: Option<String>) {
        if (text != self.status || text.is_some())
            && let Some(id) = self.active_feedback_session_id()
            && let Some(session) = self.session_mut(id)
        {
            session.invalidate_collection_status();
        }
        self.status = text;
        self.status_revision = self.status_revision.wrapping_add(1);
        self.dirty |= Regions::HEADER | Regions::FOOTER;
    }

    pub(crate) fn set_status_quietly(&mut self, text: Option<String>) {
        let prior = self.suppress_feedback;
        self.suppress_feedback = true;
        self.set_status(text);
        self.suppress_feedback = prior;
    }

    pub(crate) fn set_error(&mut self, text: Option<String>) {
        if text != self.error
            && let Some(id) = self.active_feedback_session_id()
        {
            let shown = self.error.clone();
            let next = text.clone();
            if let Some(session) = self.session_mut(id) {
                session.collection_error_revision = session.collection_error_revision.wrapping_add(1);
                if session.presented_collection_error.is_some() && session.presented_collection_error == shown {
                    session.presented_collection_error = None;
                    if next.is_some()
                        && let Some(shown) = shown
                    {
                        session.enqueue_collection_error(&shown);
                    }
                }
            }
        }
        let cleared = text.is_none() && self.error.is_some();
        self.error = text;
        self.error_revision = self.error_revision.wrapping_add(1);
        if cleared {
            self.present_pending_error = true;
        }
        self.dirty |= Regions::HEADER | Regions::TRAY | Regions::SHEETS;
    }

    pub(crate) fn set_error_quietly(&mut self, text: Option<String>) {
        let prior = self.suppress_feedback;
        self.suppress_feedback = true;
        self.set_error(text);
        self.suppress_feedback = prior;
    }

    pub(crate) fn set_updates_paused(&mut self, paused: bool) {
        self.updates_paused = paused;
        self.paused_revision = self.paused_revision.wrapping_add(1);
        self.dirty |= Regions::FOOTER;
    }

    /// Shows a queued collection error once the error line is free (Swift `presentPendingCollectionError`).
    fn present_pending_collection_error(&mut self) {
        if self.error.is_some() {
            return;
        }
        let Some(id) = self.active_feedback_session_id() else { return };
        let Some(session) = self.session_mut(id) else { return };
        let Some(pending) = session.presented_collection_error.clone().or_else(|| session.collection_error.clone())
        else {
            return;
        };
        if session.presented_collection_error.is_none() {
            session.presented_collection_error = Some(pending.clone());
            session.collection_error = None;
        }
        self.set_error_quietly(Some(pending));
        // Presenting is not a dismissal: do not loop.
        self.present_pending_error = false;
    }

    // ---------------------------------------------------------------------------------------
    // Footer.

    /// The name the interface gives a location instead of its last path component: "Home" for
    /// the user's home folder, the volume name for a volume root (`/` reads "Macintosh HD").
    pub fn place_name(&self, path: &[u8]) -> Option<&str> {
        if path == &*self.cfg.home {
            return Some("Home");
        }
        self.volumes.iter().find(|v| v.path == path).map(|volume| volume.name.as_str())
    }

    fn root_display_name(&self, root: &[u8]) -> String {
        if let Some(name) = self.place_name(root) {
            return name.to_owned();
        }
        let name = file_name(root);
        String::from_utf8_lossy(if name.is_empty() { root } else { name }).into_owned()
    }

    pub(crate) fn refresh_footer(&mut self) {
        let next = self.active_session().map(|session| {
            // Monitor changes being debounced or deferred, and background refreshes, are not
            // shown as an update in progress (no spinner); paused updates still offer Refresh.
            let refreshing = self.refreshing && !self.quiet_refresh;
            let pending_refresh = refreshing
                || self.pending_full_refresh
                || (!self.pending_dirs.is_empty() && (self.updates_paused || self.refresh_shown()))
                || session.pending_full_refresh
                || !session.pending_dirs.is_empty();
            let pending_presentation =
                self.snapshot.is_some() && session.saved.as_ref().map(|s| s.id) != self.snapshot.as_ref().map(|s| s.id);
            compute_footer(&FooterInputs {
                root_name: self.root_display_name(&session.root),
                progress: session.progress.as_ref(),
                discovered_files: session.discovered_files,
                cached_files: session.cached_files,
                finished: session.finished,
                failed: session.failed,
                stopped: session.stopped,
                refreshing,
                scanning: self.scanning,
                partial: self.partial,
                updates_paused: self.updates_paused,
                pending_removal: session.pending_removal(),
                pending_refresh,
                pending_presentation,
            })
        });
        if next != self.footer {
            self.footer = next;
            self.dirty |= Regions::FOOTER;
        }
    }

    // ---------------------------------------------------------------------------------------
    // Volumes.

    pub(crate) fn refresh_volumes(&mut self) {
        if !self.discovering_volumes {
            self.discovering_volumes = true;
            self.dirty |= Regions::SIDEBAR;
        }
        if self.volume_request.is_some() {
            self.volume_rerun = true;
            return;
        }
        self.volume_request = Some(self.request(Pending::Volumes, Request::Volumes));
    }

    fn on_volumes(&mut self, id: RequestId, volumes: Vec<Volume>) {
        if self.volume_request != Some(id) {
            return;
        }
        self.volume_request = None;
        if std::mem::take(&mut self.volume_rerun) {
            self.volume_request = Some(self.request(Pending::Volumes, Request::Volumes));
            return;
        }
        self.volumes = if volumes.iter().any(|v| v.path == b"/") {
            volumes
        } else {
            std::iter::once(Volume::fallback_root()).chain(volumes).collect()
        };
        self.discovering_volumes = false;
        // The header names volume roots after their volume ([`Self::place_name`]).
        self.dirty |= Regions::SIDEBAR | Regions::FOOTER | Regions::HEADER;
    }

    // ---------------------------------------------------------------------------------------
    // Queries for the UI.

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn active_session_id(&self) -> Option<SessionId> {
        self.active
    }

    /// The displayed folder (tree, rows, lookups).
    pub fn folder(&self) -> Option<&Arc<PreparedFolder>> {
        self.folder.as_ref()
    }

    pub fn root_path(&self) -> Option<&[u8]> {
        self.folder.as_ref().map(|f| f.root_path())
    }

    pub fn current_path(&self) -> Option<&[u8]> {
        self.folder.as_ref().map(|f| f.folder_path())
    }

    pub fn can_go_up(&self) -> bool {
        self.folder.as_ref().is_some_and(|f| !f.tree().ancestors.is_empty())
    }

    /// Changes whenever a new folder revision is installed (the map rebuilds).
    pub fn map_revision(&self) -> u64 {
        self.map_revision
    }

    /// Changes whenever the list rows or their order change.
    pub fn list_revision(&self) -> u64 {
        self.list_revision
    }

    pub fn color_revision(&self) -> u64 {
        self.colors.revision
    }

    /// The stable color ordinal of a row (`None` before a folder is shown).
    pub fn color_ordinal(&self, name: &[u8]) -> Option<u32> {
        self.colors.ordinals.get(crate::hash::name_hash(name))
    }

    pub fn snapshot(&self) -> Option<&SnapshotInfo> {
        self.snapshot.as_ref()
    }

    pub fn indexed_at(&self) -> Option<SystemTime> {
        self.snapshot.as_ref().map(|s| s.indexed_at)
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    pub fn is_refreshing(&self) -> bool {
        self.refreshing
    }

    pub fn is_opening(&self) -> bool {
        self.opening
    }

    pub fn is_partial(&self) -> bool {
        self.partial
    }

    pub fn is_saved_map(&self) -> bool {
        self.saved_map
    }

    pub fn updates_paused(&self) -> bool {
        self.updates_paused
    }

    /// "Mapping your files…" or "Opening saved map…".
    pub fn title(&self) -> &'static str {
        self.title
    }

    pub fn progress_count(&self) -> u64 {
        self.item_count
    }

    /// Bytes of the files found so far by the displayed mapping (opening view "File size").
    pub fn progress_bytes(&self) -> u64 {
        self.progress_bytes
    }

    /// The folder being listed by the displayed mapping (the root until one was listed).
    pub fn scan_path(&self) -> &[u8] {
        &self.scan_path
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn footer(&self) -> Option<&FooterState> {
        self.footer.as_ref()
    }

    pub fn selected(&self) -> Option<&Item> {
        self.selected.as_ref()
    }

    pub fn places(&self) -> &[Vec<u8>] {
        self.places.folders()
    }

    /// The place to highlight: the deepest place containing the current folder.
    pub fn selected_place(&self) -> Option<&[u8]> {
        selected_place(self.current_path(), self.places.folders().iter().map(Vec::as_slice))
    }

    pub fn volumes(&self) -> &[Volume] {
        &self.volumes
    }

    pub fn is_discovering_volumes(&self) -> bool {
        self.discovering_volumes
    }

    /// Any removal is running in any session: quitting and app updates must wait.
    pub fn removal_active(&self) -> bool {
        !self.removals_running.is_empty()
    }

    /// Whether `path` is inside the displayed map's root.
    pub fn in_displayed_root(&self, path: &[u8]) -> bool {
        self.root_path().is_some_and(|root| is_within(path, root))
    }

    /// The displayed collection (empty unless the displayed map is the active session's).
    pub fn collection(&self) -> Option<&CollectionSnapshot> {
        if !self.displayed_matches_active() {
            return None;
        }
        self.active_session().map(|s| &s.collection)
    }

    pub(crate) fn notify(&mut self, regions: Regions) {
        self.dirty |= regions;
    }
}
