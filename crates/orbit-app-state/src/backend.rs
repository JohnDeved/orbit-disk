//! The engine boundary. [`Backend`] is a plain, blocking interface that the host runs on a
//! background executor; [`perform`] executes one [`Request`] and turns its result into the
//! [`Event`] that goes back into [`crate::AppState::handle`]. Long-running mapping and monitoring
//! report through a [`Notifier`] as [`MappingEvent`]s tagged with their session.
//!
//! The integrator implements `Backend` over `orbit-core` (live inventory, stage, saved store,
//! search, policy, removal journal). [`crate::testing::FakeBackend`] implements it in memory.

use std::sync::Arc;

use orbit_core::view::FolderTree;
use orbit_platform::{Cancellation, FileIdentity};

use crate::event::Event;
#[allow(unused_imports)] // referenced from docs
use crate::event::MappingEvent;
use crate::folder::{ColorState, PreparedFolder, prepare_folder};
use crate::ids::{RemovalId, RequestId, SessionId};
use crate::model::{Item, ScanRemoval, SnapshotInfo, Volume};

/// Delivers mapping/monitor events from any thread to the UI thread.
pub type Notifier = Arc<dyn Fn(Event) + Send + Sync>;

/// A session that may already cover a requested path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveCandidate {
    pub root: Arc<[u8]>,
    pub identity: FileIdentity,
    /// Removals of that session that contain the requested path. If any of them was replaced on
    /// disk, the candidate must not be reused.
    pub covering_removals: Vec<ScanRemoval>,
}

/// Which root maps a requested location (Swift `scan` resolution step).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolution {
    /// The candidate session root, a saved map that covers the path, or the path itself.
    pub root: Vec<u8>,
    pub root_identity: Option<FileIdentity>,
    /// Identity of the requested path itself.
    pub target_identity: Option<FileIdentity>,
    /// The candidate session covers the path (reachable within it).
    pub used_candidate: bool,
    /// The location cannot be mapped (cloud placeholder, unreadable…); shown as the error.
    pub exclusion: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MappingRequest {
    pub session: SessionId,
    pub root: Arc<[u8]>,
    pub identity: Option<FileIdentity>,
    /// Re-map from scratch instead of reusing a saved map or a resumable stage.
    pub force: bool,
    /// The saved map stays on screen; discovery runs without a visible preview.
    pub keep_map: bool,
    /// The folder the user wants to see first.
    pub route: Vec<u8>,
    pub removals: Arc<[ScanRemoval]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewSource {
    /// The session's live inventory / mapping stage.
    Preview,
    /// A published saved map.
    Saved(u64),
}

#[derive(Clone, Debug)]
pub struct ViewRequest {
    pub session: SessionId,
    pub root: Arc<[u8]>,
    pub source: ViewSource,
    pub path: Vec<u8>,
    /// Items to hide while the index catches up (identity-matched).
    pub removals: Arc<[ScanRemoval]>,
    /// When the folder does not exist (or is not discovered yet), return the nearest existing
    /// ancestor instead of `None`. Saved views always fall back; previews only when asked.
    pub fallback: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchSource {
    Preview,
    Saved { snapshot: u64, stable_order: bool },
}

#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub session: SessionId,
    pub root: Arc<[u8]>,
    pub source: SearchSource,
    pub query: String,
    pub removals: Arc<[ScanRemoval]>,
    pub limit: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SearchReply {
    pub hits: Vec<Item>,
    pub total: u64,
    /// Preview revision the search read (preview searches only).
    pub preview_revision: u64,
    /// The removal overlay changed while searching; the result is stale.
    pub removals_changed: bool,
}

#[derive(Clone, Debug)]
pub struct ValidateRequest {
    pub session: SessionId,
    pub root: Arc<[u8]>,
    pub root_identity: FileIdentity,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Default)]
pub struct Validation {
    pub accepted: Vec<Item>,
    /// User-visible rejection lines (`"name: reason"`).
    pub rejections: Vec<String>,
    /// The mapped root still has its identity after validation.
    pub root_matches: bool,
    /// Folders of accepted items that changed on disk since the map read them (replaced by a
    /// save, for example): the map refreshes them, nobody has to.
    pub stale_parents: Vec<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct RemovalRequest {
    pub removal: RemovalId,
    pub session: SessionId,
    pub root: Arc<[u8]>,
    pub root_identity: Option<FileIdentity>,
    pub items: Vec<Item>,
    pub permanently: bool,
    /// Beyond this many changed parents, report `parents_overflow` instead of listing them.
    pub parent_limit: usize,
}

#[derive(Clone, Debug, Default)]
pub struct RemovalOutcome {
    /// Paths actually removed (identity verified before removal).
    pub removed: Vec<Arc<[u8]>>,
    pub failures: Vec<String>,
    pub changed_parents: Vec<Vec<u8>>,
    pub parents_overflow: bool,
    /// The mapped root changed identity; removal stopped and updates must pause.
    pub root_changed: bool,
    /// More removed items than the overlay can hold; views need a full refresh.
    pub overlay_overflow: bool,
}

#[derive(Clone, Debug)]
pub struct RefreshRequest {
    pub session: SessionId,
    pub root: Arc<[u8]>,
    pub snapshot: u64,
    /// Folders whose listings to repeat (not their subfolders).
    pub paths: Vec<Vec<u8>>,
    /// Folders to reconcile recursively.
    pub subtrees: Vec<Vec<u8>>,
    /// Also re-list the folders holding ones the map could not read, where those can be read
    /// now (access granted since: no change is reported for that).
    pub retry_unreadable: bool,
}

/// Work the state asks the host to run off the UI thread.
#[derive(Clone, Debug)]
pub enum Request {
    Resolve {
        path: Vec<u8>,
        force: bool,
        candidate: Option<ResolveCandidate>,
    },
    /// Fire-and-forget: progress and results arrive as [`MappingEvent`]s.
    StartMapping(MappingRequest),
    /// Fire-and-forget: stop (pause) the session's mapping; its stage stays resumable.
    StopMapping {
        session: SessionId,
    },
    /// Fire-and-forget: list this folder next.
    Prioritize {
        session: SessionId,
        path: Vec<u8>,
    },
    View {
        request: ViewRequest,
        colors: Option<ColorState>,
    },
    DiscoveredCount {
        session: SessionId,
        removals: Arc<[ScanRemoval]>,
    },
    Search(SearchRequest),
    Validate(ValidateRequest),
    Remove(RemovalRequest),
    Refresh(RefreshRequest),
    /// Fire-and-forget: watch the saved map's root; changes arrive as [`MappingEvent::Changes`].
    Monitor {
        session: SessionId,
        snapshot: SnapshotInfo,
    },
    StopMonitor {
        session: SessionId,
    },
    Volumes,
    /// Which of these paths are existing directories (resolved, symlinks followed).
    CheckPlaces {
        paths: Vec<Vec<u8>>,
    },
}

/// Results of [`Request`]s.
#[derive(Clone, Debug)]
pub enum Reply {
    Resolved(Resolution),
    View(Result<Option<Arc<PreparedFolder>>, String>),
    Count(Result<u64, String>),
    Search(Result<SearchReply, String>),
    Validated(Result<Validation, String>),
    Removed(RemovalOutcome),
    Refreshed(Result<SnapshotInfo, String>),
    Volumes(Vec<Volume>),
    Places(Vec<Vec<u8>>),
    /// The request was cancelled; the state drops it.
    Cancelled,
}

/// The engine operations `AppState` relies on. Methods block; the host calls them off the UI
/// thread (via [`perform`]).
pub trait Backend: Send + Sync {
    fn resolve(
        &self,
        path: &[u8],
        force: bool,
        candidate: Option<&ResolveCandidate>,
        cancel: &Cancellation,
    ) -> Resolution;
    /// Starts or resumes mapping `request.root` and returns. Reports through `notify`:
    /// `DiscoveryStarted`, `Progress`, `PreviewChanged`, `SavedAvailable`, `Published` or `Failed`.
    fn start_mapping(&self, request: &MappingRequest, notify: Notifier);
    fn stop_mapping(&self, session: SessionId);
    fn prioritize(&self, session: SessionId, path: &[u8]);
    fn view(&self, request: &ViewRequest, cancel: &Cancellation) -> Result<Option<FolderTree>, String>;
    fn discovered_file_count(
        &self,
        session: SessionId,
        removals: &[ScanRemoval],
        cancel: &Cancellation,
    ) -> Result<u64, String>;
    fn search(&self, request: &SearchRequest, cancel: &Cancellation) -> Result<SearchReply, String>;
    /// Current identity of `path` (no symlink following), `None` when missing.
    fn identity(&self, path: &[u8]) -> Option<FileIdentity>;
    /// Removal-policy rejection (`TrashPolicy`), e.g. protected or cloud-only items.
    fn rejection_reason(&self, path: &[u8], root: &[u8]) -> Option<String>;
    fn remove(&self, request: &RemovalRequest) -> RemovalOutcome;
    fn refresh(&self, request: &RefreshRequest, cancel: &Cancellation) -> Result<SnapshotInfo, String>;
    fn monitor(&self, session: SessionId, snapshot: &SnapshotInfo, notify: Notifier);
    fn stop_monitor(&self, session: SessionId);
    fn volumes(&self) -> Vec<Volume>;
    fn directories(&self, paths: &[Vec<u8>]) -> Vec<Vec<u8>>;
}

/// Runs one request against `backend`. Returns the reply event, or `None` for fire-and-forget
/// requests. View replies are prepared ([`prepare_folder`]) here so the UI thread only installs.
pub fn perform<B: Backend + ?Sized>(
    backend: &B,
    id: RequestId,
    request: Request,
    cancel: &Cancellation,
    notify: &Notifier,
) -> Option<Event> {
    let reply = match request {
        Request::Resolve { path, force, candidate } => {
            Reply::Resolved(backend.resolve(&path, force, candidate.as_ref(), cancel))
        }
        Request::StartMapping(request) => {
            backend.start_mapping(&request, Arc::clone(notify));
            return None;
        }
        Request::StopMapping { session } => {
            backend.stop_mapping(session);
            return None;
        }
        Request::Prioritize { session, path } => {
            backend.prioritize(session, &path);
            return None;
        }
        Request::Monitor { session, snapshot } => {
            backend.monitor(session, &snapshot, Arc::clone(notify));
            return None;
        }
        Request::StopMonitor { session } => {
            backend.stop_monitor(session);
            return None;
        }
        Request::View { request, colors } => Reply::View(
            backend
                .view(&request, cancel)
                .map(|tree| tree.and_then(|tree| prepare_folder(tree, colors.as_ref(), Some(cancel))).map(Arc::new)),
        ),
        Request::DiscoveredCount { session, removals } => {
            Reply::Count(backend.discovered_file_count(session, &removals, cancel))
        }
        Request::Search(request) => Reply::Search(backend.search(&request, cancel)),
        Request::Validate(request) => Reply::Validated(Ok(validate(backend, &request, cancel))),
        Request::Remove(request) => Reply::Removed(backend.remove(&request)),
        Request::Refresh(request) => Reply::Refreshed(backend.refresh(&request, cancel)),
        Request::Volumes => Reply::Volumes(backend.volumes()),
        Request::CheckPlaces { paths } => Reply::Places(backend.directories(&paths)),
    };
    let reply = if cancel.is_cancelled() { Reply::Cancelled } else { reply };
    Some(Event::Backend { id, reply })
}

/// Collection validation (Swift `validateCollectionNodes` + the root identity checks around it).
fn validate<B: Backend + ?Sized>(backend: &B, request: &ValidateRequest, cancel: &Cancellation) -> Validation {
    let root_before = backend.identity(&request.root) == Some(request.root_identity);
    let mut result = Validation::default();
    for item in &request.items {
        if cancel.is_cancelled() {
            return result;
        }
        let name = item.display_name();
        // The policy checks the item as it is now (symlinks, protected and hidden locations).
        if let Some(reason) = backend.rejection_reason(&item.path, &request.root) {
            result.rejections.push(format!("{name}: {reason}"));
            continue;
        }
        let current = backend.identity(&item.path);
        if !item.meta.flags.accessible && current.is_some_and(|current| item.meta.identity == Some(current)) {
            result.rejections.push(format!("{name}: Orbit Disk can't read this folder."));
            continue;
        }
        match (item.meta.identity, current) {
            (Some(expected), Some(current)) if current == expected => result.accepted.push(item.clone()),
            // Replaced since the map read it (an app saved it anew), or not identified yet:
            // collect what is there now, and let the map catch up. Removal checks the identity
            // again before touching it.
            (_, Some(current)) => {
                let mut item = item.clone();
                item.meta.identity = Some(current);
                let parent = crate::paths::parent_or_root(&item.path).to_vec();
                if !result.stale_parents.contains(&parent) {
                    result.stale_parents.push(parent);
                }
                result.accepted.push(item);
            }
            _ => result.rejections.push(format!("{name}: no longer exists. It was moved or deleted.")),
        }
    }
    result.root_matches = root_before && backend.identity(&request.root) == Some(request.root_identity);
    result
}
