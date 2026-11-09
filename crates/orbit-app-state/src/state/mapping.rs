//! Mapping lifecycle: opening locations, sessions, previews, folder installation, navigation,
//! publication, pause/resume, monitoring and refresh. Port of the corresponding parts of Swift
//! `DiskModel` (`scan`, `scanResolved`, `beginPreview`, `readPreview`, `installFolder`,
//! `loadFolder`, `finishScan`, `cancelScan`, `scheduleRefresh`, `refreshChangedFolders`).

use std::sync::Arc;

use orbit_core::paths::is_within;
use orbit_core::view::NodeMeta;
use orbit_platform::FileIdentity;

use super::{AppState, COUNT_READ_INTERVAL, FOLDER_CACHE_LIMIT, Pending, REFRESH_DELAY, SYNC_PREPARE_LIMIT};
use crate::backend::{MappingRequest, Request, Resolution, ResolveCandidate, ViewRequest, ViewSource};
use crate::event::{MappingEvent, Persist, Regions, TimerKind};
use crate::folder::{ColorState, PreparedFolder, prepare_folder};
use crate::hash::name_hash;
use crate::ids::{RequestId, SessionId};
use crate::model::{Item, ScanRemoval, SnapshotInfo};
use crate::paths::{normalized, parent_or_root};
use crate::session::{Remembered, Session};

pub(crate) const TITLE_MAPPING: &str = "Mapping your files…";
pub(crate) const TITLE_OPENING: &str = "Opening saved map…";
const STATUS_PARTIAL_KEPT: &str = "Partial map kept. Restart mapping to finish.";
const STATUS_UPDATING_AFFECTED: &str = "Updating affected folders…";

/// Work held while a pointer gesture is down, applied in order when it ends.
#[derive(Debug, Default)]
pub(crate) struct Deferred {
    pub preview: Option<(Arc<PreparedFolder>, u64, u64)>,
    pub completion: Option<DeferredCompletion>,
    pub indexed: Option<DeferredIndexed>,
    pub sort: Option<(crate::event::NameOrder, u64)>,
}

#[derive(Debug)]
pub(crate) struct DeferredCompletion {
    pub session: SessionId,
    pub generation: u64,
    pub epoch: u64,
    pub snapshot: SnapshotInfo,
    pub view: Arc<PreparedFolder>,
    pub reused: bool,
}

#[derive(Debug)]
pub(crate) struct DeferredIndexed {
    pub view: Arc<PreparedFolder>,
    pub snapshot: SnapshotInfo,
    pub generation: u64,
    pub browse: u64,
    pub session: Option<SessionId>,
    pub epoch: u64,
}

impl AppState {
    // -------------------------------------------------------------------------------------
    // Opening a location.

    /// Swift `scan(_:force:)`: resolve which root covers `path`, then attach or start a session.
    pub(crate) fn open_location(&mut self, path: Vec<u8>, force: bool) {
        let path = normalized(&path).into_owned();
        if self.root_path() != Some(&path[..]) {
            self.clear_drag();
        }
        let query = self.search.query.clone();
        if force && self.snapshot.is_some() && self.root_path() == Some(&path[..]) {
            self.refreshing = true;
        }
        let previous = self.resolution_request.take();
        self.cancel(previous);
        self.resolution = self.token();
        let candidate = if force {
            None
        } else {
            self.sessions
                .iter()
                .filter(|s| s.identity.is_some() && is_within(&path, &s.root))
                .max_by_key(|s| s.root.len())
        };
        let request_candidate = candidate.and_then(|s| {
            Some(ResolveCandidate {
                root: Arc::clone(&s.root),
                identity: s.identity?,
                covering_removals: s.removals.iter().filter(|r| is_within(&path, &r.path)).cloned().collect(),
            })
        });
        let candidate = candidate.map(|s| (s.id, s.epoch));
        self.opening = true;
        self.notify(Regions::HEADER);
        let pending = Pending::Resolve { resolution: self.resolution, path: path.clone(), force, query, candidate };
        let id = self.request(pending, Request::Resolve { path, force, candidate: request_candidate });
        self.resolution_request = Some(id);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_resolved(
        &mut self,
        id: RequestId,
        resolution: u64,
        path: Vec<u8>,
        force: bool,
        query: String,
        candidate: Option<(SessionId, u64)>,
        result: Resolution,
    ) {
        if self.resolution != resolution {
            return;
        }
        if self.resolution_request == Some(id) {
            self.resolution_request = None;
        }
        if let Some(exclusion) = result.exclusion {
            self.opening = false;
            self.set_error(Some(exclusion));
            return;
        }
        if let Some((session, epoch)) = candidate
            && self.session(session).is_some_and(|s| s.epoch != epoch)
        {
            self.open_location(path, force);
            return;
        }
        let target_matches = self.lookup_item(&path).is_none_or(|item| item.identity() == result.target_identity);
        let candidate_alive = !result.used_candidate || candidate.is_some_and(|(id, _)| self.session(id).is_some());
        let (root, identity) = if target_matches && candidate_alive {
            (result.root, result.root_identity)
        } else {
            (path.clone(), result.target_identity)
        };
        self.scan_resolved(path, root, identity, result.target_identity, query, force);
    }

    fn navigate_within(&mut self, requested: &[u8], target_identity: Option<FileIdentity>, query: &str) -> bool {
        let Some(item) = self.lookup_item(requested).filter(Item::is_dir) else { return false };
        if item.identity() != target_identity {
            self.open_location(requested.to_vec(), true);
            return true;
        }
        if self.current_path() == Some(requested) {
            self.opening = false;
            return true;
        }
        if self.search.query == query {
            self.set_query(String::new());
        }
        self.load_folder(requested.to_vec(), None);
        true
    }

    fn scan_resolved(
        &mut self,
        requested: Vec<u8>,
        root: Vec<u8>,
        identity: Option<FileIdentity>,
        target_identity: Option<FileIdentity>,
        query: String,
        force: bool,
    ) {
        let session_usable = self.session_by_root(&root).is_some_and(|s| !s.failed && !s.stopped);
        let displayed_identity = self.displayed_root().map(|(_, identity)| identity);
        if !force && session_usable && identity.is_some() && displayed_identity == Some(identity) {
            let saved_root = self.snapshot.as_ref().filter(|_| !self.scanning).map(|s| Arc::clone(&s.root));
            if saved_root.is_some_and(|saved| *saved == *root && is_within(&requested, &saved))
                && self.navigate_within(&requested, target_identity, &query)
            {
                return;
            }
            if self.partial
                && self.root_path() == Some(&root[..])
                && is_within(&requested, &root)
                && self.navigate_within(&requested, target_identity, &query)
            {
                return;
            }
        }
        let same_root = self.displayed_root().is_some_and(|(r, i)| *r == *root && i == identity);
        let desired = if requested != root {
            requested.clone()
        } else if same_root {
            self.requested_path.clone().or_else(|| self.current_path().map(<[u8]>::to_vec)).unwrap_or(root.clone())
        } else {
            root.clone()
        };
        if !same_root {
            self.clear_drag();
        }
        self.save_active_session_view();
        let prior = self.session_by_root(&root).map(|s| (s.id, s.identity, s.stopped, s.failed));
        let reusable = !force
            && identity.is_some()
            && prior
                .is_some_and(|(_, prior_identity, stopped, failed)| prior_identity == identity && !stopped && !failed);
        let preserve = self.active.is_some() && (!same_root || reusable);
        self.stop_work(preserve);
        self.generation = self.token();
        if requested != root
            && let Some(target) = target_identity
        {
            self.requested_target = Some((requested.clone(), target));
        }
        match prior {
            Some((id, ..)) if reusable => self.reattach(id, &requested, &root, desired),
            _ => self.start_session(prior.map(|p| p.0), root, identity, desired, same_root, force, &query),
        }
    }

    fn save_active_session_view(&mut self) {
        let Some(id) = self.active else { return };
        let route = self.requested_path.clone().or_else(|| self.current_path().map(<[u8]>::to_vec));
        let selection = self.selected.as_ref().map(|item| Arc::clone(&item.path));
        let dirs = std::mem::take(&mut self.pending_dirs);
        let full = self.pending_full_refresh;
        if let Some(session) = self.session_mut(id) {
            session.last_route = route;
            if let Some(remembered) = &mut session.remembered {
                remembered.selection = selection;
            }
            session.pending_dirs.extend(dirs);
            session.pending_full_refresh |= full;
        }
    }

    /// Swift `stopWork`: detach everything tied to the displayed attachment.
    pub(crate) fn stop_work(&mut self, preserve_scan: bool) {
        let read = self.preview_read.take();
        self.cancel(read);
        self.preview_read_again = None;
        let load = self.folder_load.take();
        self.cancel(load);
        self.deferred.preview = None;
        self.deferred.completion = None;
        self.deferred.indexed = None;
        self.gesture = None;
        if let Some((order, revision)) = self.deferred.sort.take() {
            self.finish_name_sort(order, revision);
        }
        self.detach_validation();
        self.removal_result_status = None;
        if !preserve_scan {
            if let Some(session) = self.active_session_mut()
                && !session.stopped
            {
                session.stopped = true;
                let id = session.id;
                self.send(Request::StopMapping { session: id });
            }
            let refresh = self.refresh_request.take();
            self.cancel(refresh);
        }
        self.active = None;
        self.cancel_search();
        self.search.searching = false;
        self.refresh_timer = None;
        self.count_timer = None;
        if let Some(session) = self.monitored.take() {
            self.send(Request::StopMonitor { session });
        }
        self.pending_dirs.clear();
        self.pending_full_refresh = false;
        self.browse = self.token();
        self.requested_path = None;
        self.requested_target = None;
        self.opening = false;
        self.notify(Regions::HEADER | Regions::FOOTER);
    }

    /// Switching back to a session that is still valid (Swift `scanResolved`, reusable branch).
    fn reattach(&mut self, id: SessionId, requested: &[u8], root: &[u8], desired: Vec<u8>) {
        let revision = self.token();
        let Some(session) = self.session_mut(id) else { return };
        session.collection = session.collection.renewed(revision);
        let status = session.removal_status().cloned();
        let error = session.removal_error().cloned();
        let removing = session.removing;
        let mut dirs: Vec<Vec<u8>> = session.removal_parents.iter().cloned().collect();
        dirs.extend(std::mem::take(&mut session.pending_dirs));
        let full = std::mem::take(&mut session.pending_full_refresh);
        let route = if requested != root { requested.to_vec() } else { session.last_route.clone().unwrap_or(desired) };
        let finished = session.finished;
        let live = session.live_preview;
        let saved = session.saved.clone();
        let epoch = session.epoch;
        let progress = session.progress.clone();
        self.set_status(status.clone());
        self.set_error(error);
        self.removal_result_status = if removing { None } else { status };
        self.pending_dirs.extend(dirs);
        self.pending_full_refresh = full;
        self.show_review = false;
        self.confirmation = None;
        self.active = Some(id);
        self.attach_validation(None);
        self.requested_path = Some(route.clone());
        if let Some(progress) = progress {
            self.item_count = progress.item_count;
            self.progress_bytes = progress.bytes_scanned;
            self.scan_path = progress.current_path;
        }
        if finished {
            self.scanning = true;
            self.refreshing = false;
            self.set_updates_paused(false);
            self.title = TITLE_OPENING;
            if let Some(saved) = saved.clone() {
                let pending = Pending::Finish {
                    generation: self.generation,
                    session: id,
                    epoch,
                    snapshot: saved.clone(),
                    reused: true,
                };
                self.request_view(pending, id, ViewSource::Saved(saved.id), route.clone(), true);
            }
            if live {
                self.begin_preview(id, route);
            } else if let Some(saved) = saved {
                self.snapshot = Some(saved.clone());
                self.partial = false;
                self.preview_session = None;
                self.restore_remembered(id, &route, Some(saved));
            }
            return;
        }
        self.scanning = live;
        self.refreshing = !live;
        self.set_updates_paused(false);
        self.scan_path = root.to_vec();
        if live {
            self.begin_preview(id, route);
        } else if let Some(saved) = saved {
            self.snapshot = Some(saved.clone());
            self.partial = false;
            self.preview_session = None;
            self.opening = true;
            self.restore_remembered(id, &route, None);
            if self.active == Some(id) {
                let pending = Pending::Baseline {
                    generation: self.generation,
                    session: id,
                    epoch,
                    snapshot: saved.clone(),
                    route: route.clone(),
                };
                self.request_view(pending, id, ViewSource::Saved(saved.id), route, true);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start_session(
        &mut self,
        prior: Option<SessionId>,
        root: Vec<u8>,
        identity: Option<FileIdentity>,
        desired: Vec<u8>,
        same_root: bool,
        force: bool,
        query: &str,
    ) {
        let prior = prior.and_then(|id| {
            let index = self.sessions.iter().position(|s| s.id == id)?;
            Some(self.sessions.remove(index))
        });
        if let Some(prior) = &prior
            && !prior.stopped
        {
            self.send(Request::StopMapping { session: prior.id });
        }
        let upgrading = same_root && self.snapshot.as_ref().is_some_and(|s| s.needs_boot_upgrade);
        let keep_map = same_root && self.snapshot.is_some() && !self.partial && !upgrading;
        self.scanning = !keep_map;
        self.refreshing = keep_map;
        self.set_updates_paused(false);
        self.title = if force { TITLE_MAPPING } else { TITLE_OPENING };
        self.scan_path = root.clone();
        self.item_count = 0;
        self.progress_bytes = 0;
        self.set_error(None);
        self.set_status(keep_map.then(|| "Refreshing the full map…".to_owned()));
        if !keep_map && self.search.query == query {
            self.set_query(String::new());
        }
        self.requested_path = Some(desired.clone());
        let id = self.ids.session();
        let root: Arc<[u8]> = Arc::from(root);
        let empty = crate::collection::CollectionSnapshot::empty(self.token());
        let mut session = Session::new(id, Arc::clone(&root), identity, empty);
        session.live_preview = !keep_map;
        match prior {
            Some(prior) if prior.identity == identity && *prior.root == *root => {
                let prior_status = prior.removal_status().cloned();
                let prior_error = prior.removal_error().cloned();
                session.job = prior.job;
                session.collection = prior.collection;
                session.removing = prior.removing;
                session.removal_parents = prior.removal_parents;
                session.pending_dirs = prior.pending_dirs;
                session.pending_full_refresh = prior.pending_full_refresh;
                session.removal_versions = prior.removal_versions;
                session.removals = prior.removals;
                session.epoch = prior.epoch;
                session.remembered = prior.remembered;
                session.recovery_pending = prior.recovery_pending;
                session.preview_blocked = prior.preview_blocked;
                if !force {
                    if let Some(progress) = prior.progress.as_ref() {
                        self.item_count = progress.item_count;
                        self.progress_bytes = progress.bytes_scanned;
                        self.scan_path = progress.current_path.clone();
                    }
                    session.progress = prior.progress.map(|p| crate::model::ProgressSnapshot {
                        revision: 0,
                        item_count: p.item_count,
                        bytes_scanned: p.bytes_scanned,
                        current_path: p.current_path,
                        ..Default::default()
                    });
                    session.discovered_files = prior.discovered_files;
                    session.cached_files = prior.cached_files;
                }
                session.set_removal_status(prior_status);
                session.set_removal_error(prior_error);
                session.collection_status = prior.collection_status;
                session.collection_error = prior.collection_error;
                session.presented_collection_error = prior.presented_collection_error;
                session.collection_status_revision = prior.collection_status_revision;
                session.collection_error_revision = prior.collection_error_revision;
            }
            Some(prior) => {
                if let Some(job) = &prior.job {
                    let inflight = job.inflight;
                    self.cancel(inflight);
                }
            }
            None => {}
        }
        session.collection = session.collection.renewed(self.token());
        self.pending_dirs.extend(session.removal_parents.iter().cloned());
        self.pending_dirs.extend(std::mem::take(&mut session.pending_dirs));
        self.pending_full_refresh = std::mem::take(&mut session.pending_full_refresh);
        self.show_review = false;
        self.confirmation = None;
        if keep_map || upgrading {
            session.saved = self.snapshot.clone();
            session.last_route = Some(desired.clone());
        }
        let removals = Arc::clone(&session.removals);
        let live = session.live_preview;
        self.sessions.push(session);
        self.active = Some(id);
        self.attach_validation(None);
        self.send(Request::StartMapping(MappingRequest {
            session: id,
            root,
            identity,
            force,
            keep_map,
            route: desired.clone(),
            removals,
        }));
        if live {
            self.begin_preview(id, desired);
        }
        self.pump_collection(id);
    }

    // -------------------------------------------------------------------------------------
    // Previews.

    pub(crate) fn color_seed(&self) -> Option<ColorState> {
        self.folder.as_ref().map(|_| self.colors.clone())
    }

    pub(crate) fn request_view(
        &mut self,
        pending: Pending,
        session: SessionId,
        source: ViewSource,
        path: Vec<u8>,
        fallback: bool,
    ) -> Option<RequestId> {
        let s = self.session(session)?;
        let request = ViewRequest {
            session,
            root: Arc::clone(&s.root),
            source,
            path,
            removals: Arc::clone(&s.removals),
            fallback,
        };
        let colors = self.color_seed();
        Some(self.request(pending, Request::View { request, colors }))
    }

    fn begin_preview(&mut self, id: SessionId, path: Vec<u8>) {
        self.send(Request::Prioritize { session: id, path: path.clone() });
        let Some(session) = self.session(id) else { return };
        let owner = (Arc::clone(&session.root), session.identity);
        let displayed = self.displayed_root().map(|(root, identity)| (root.to_vec(), identity));
        if displayed.is_none_or(|(root, identity)| *root != *owner.0 || identity != owner.1) {
            self.clear_presentation(true);
        }
        self.snapshot = None;
        self.saved_map = false;
        self.folder_cache.clear();
        self.partial = true;
        self.preview_session = Some(id);
        self.requested_path = Some(path.clone());
        self.last_preview = None;
        if self.active == Some(id) {
            self.restore_remembered(id, &path, None);
        }
        self.request_preview_read(false);
    }

    /// Swift `readPreview`: one read at a time; updates during a read coalesce into one more.
    pub(crate) fn request_preview_read(&mut self, force: bool) {
        let Some(id) = self.preview_session.filter(|id| Some(*id) == self.active && self.partial) else { return };
        if self.preview_read.is_some() {
            if !force && self.opening {
                return;
            }
            self.preview_read_again = Some(self.preview_read_again.unwrap_or(false) || force);
            return;
        }
        let Some(session) = self.session(id) else { return };
        if session.preview_blocked {
            return;
        }
        let path = self
            .requested_path
            .clone()
            .or_else(|| self.current_path().map(<[u8]>::to_vec))
            .unwrap_or_else(|| session.root.to_vec());
        if !force && self.last_preview.as_ref().is_some_and(|(p, r)| *p == path && *r == session.preview_revision) {
            return;
        }
        let fallback = force || session.removals.iter().any(|r| is_within(&path, &r.path));
        let pending = Pending::PreviewRead {
            generation: self.generation,
            browse: self.browse,
            session: id,
            epoch: session.epoch,
        };
        self.preview_read = self.request_view(pending, id, ViewSource::Preview, path, fallback);
    }

    pub(crate) fn on_preview_view(
        &mut self,
        id: RequestId,
        pending: Pending,
        view: Result<Option<Arc<PreparedFolder>>, String>,
    ) {
        match pending {
            Pending::PreviewRead { generation, browse, session, epoch } => {
                if self.preview_read == Some(id) {
                    self.preview_read = None;
                }
                let again = self.preview_read_again.take();
                let valid = generation == self.generation
                    && browse == self.browse
                    && self.preview_session == Some(session)
                    && self.session(session).is_some_and(|s| s.epoch == epoch);
                if valid
                    && let Ok(Some(view)) = view
                    && self.check_requested_target(&view, false)
                {
                    self.receive_preview(view);
                }
                if let Some(force) = again {
                    self.request_preview_read(force);
                }
            }
            Pending::PreviewFolder { generation, browse, session, epoch, path } => {
                if self.folder_load == Some(id) {
                    self.folder_load = None;
                }
                let valid = generation == self.generation
                    && browse == self.browse
                    && self.partial
                    && self.preview_session == Some(session)
                    && self.session(session).is_some_and(|s| s.epoch == epoch && !s.preview_blocked);
                if !valid {
                    return;
                }
                match view {
                    Ok(Some(view)) => self.receive_preview(view),
                    _ => {
                        self.opening = false;
                        self.notify(Regions::HEADER);
                        let removed =
                            self.active_session().is_some_and(|s| s.removals.iter().any(|r| is_within(&path, &r.path)));
                        if removed {
                            self.request_preview_read(true);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn receive_preview(&mut self, view: Arc<PreparedFolder>) {
        if !self.partial || self.active_session().is_some_and(|s| s.preview_blocked) {
            return;
        }
        let revision = view.tree().revision;
        if let Some((path, last)) = &self.last_preview
            && path[..] == *view.folder_path()
            && (*last > revision || (*last == revision && !self.opening))
        {
            return;
        }
        self.last_preview = Some((view.folder_path().to_vec(), revision));
        if self.gesture.is_some() {
            self.deferred.preview = Some((view, self.generation, self.browse));
            return;
        }
        self.publish_preview(view);
    }

    pub(crate) fn publish_preview(&mut self, view: Arc<PreparedFolder>) {
        let selection = self.selected.as_ref().map(|item| Arc::clone(&item.path));
        self.install_folder(view, selection, true, None, None);
        if self.filtering() && self.search.request.is_none() {
            self.search();
        }
    }

    /// The location requested inside a map must still be the item that was resolved
    /// (Swift `requestedTarget`). Returns `false` when a forced scan of the target replaced
    /// this result. `strict`: a different folder than the target also fails (saved reads).
    fn check_requested_target(&mut self, view: &PreparedFolder, strict: bool) -> bool {
        let Some((target, identity)) = self.requested_target.clone() else { return true };
        if self.requested_path.as_deref() != Some(&target[..]) {
            return true;
        }
        let removed = self.active_session().is_some_and(|s| s.removals.iter().any(|r| is_within(&target, &r.path)));
        if view.folder_path() == &target[..] {
            if !removed && view.folder_meta().identity != Some(identity) {
                self.requested_target = None;
                self.open_location(target, true);
                return false;
            }
            self.requested_target = None;
        } else if removed {
            self.requested_target = None;
        } else if strict {
            self.requested_target = None;
            self.open_location(target, true);
            return false;
        }
        true
    }

    fn restore_remembered(&mut self, id: SessionId, path: &[u8], published: Option<SnapshotInfo>) -> bool {
        let Some(session) = self.session(id) else { return false };
        let target_ok = |folder: &PreparedFolder| {
            self.requested_target
                .as_ref()
                .is_none_or(|(p, identity)| p[..] != *path || folder.folder_meta().identity == Some(*identity))
        };
        let remembered = session.remembered.as_ref().filter(|r| {
            self.active == Some(id)
                && !session.preview_blocked
                && r.epoch == session.epoch
                && r.folder.folder_path() == path
                && *r.folder.root_path() == *session.root
                && r.folder.root_meta().identity == session.identity
                && target_ok(&r.folder)
        });
        let Some(remembered) = remembered else {
            if self.folder.is_some() && !self.displayed_matches_active() {
                self.clear_presentation(true);
            }
            return false;
        };
        let (folder, selection) = (Arc::clone(&remembered.folder), remembered.selection.clone());
        self.install_folder(folder, selection, false, published, None);
        true
    }

    // -------------------------------------------------------------------------------------
    // Installing folders.

    /// Swift `clearPresentation`.
    pub(crate) fn clear_presentation(&mut self, clear_selection: bool) {
        let old = self.folder.take();
        self.map_revision = self.token();
        self.list_revision = self.token();
        self.order = None;
        if clear_selection {
            self.selected = None;
            self.colors = ColorState { folder: Arc::from(&b""[..]), revision: self.token(), ordinals: Arc::default() };
        }
        if old.is_some() {
            self.on_root_changed();
        }
        self.notify(Regions::MAP | Regions::LIST | Regions::HEADER | Regions::INSPECTOR | Regions::TRAY);
    }

    fn on_root_changed(&mut self) {
        self.clear_drag();
        self.detach_validation();
        self.attach_validation(None);
        self.clear_all_hover();
    }

    /// Swift `installFolder`: publish a prepared folder. Callers have checked the tokens.
    pub(crate) fn install_folder(
        &mut self,
        view: Arc<PreparedFolder>,
        selection: Option<Arc<[u8]>>,
        retain_pending: bool,
        published: Option<SnapshotInfo>,
        completed: Option<bool>,
    ) {
        let old = self.folder.take();
        let same_folder = old.as_ref().is_some_and(|f| f.folder_path() == view.folder_path());
        let same_root = old
            .as_ref()
            .is_some_and(|f| f.root_path() == view.root_path() && f.root_meta().identity == view.root_meta().identity);
        // Colors: a branch keeps its ordinal while this folder stays visible.
        if !same_folder || self.colors.ordinals.is_empty() {
            self.colors = ColorState {
                folder: Arc::from(view.folder_path()),
                revision: self.token(),
                ordinals: Arc::clone(&view.initial_colors),
            };
        } else if let Some((ordinals, _, extended)) =
            view.preserved_colors.as_ref().filter(|(_, revision, _)| *revision == self.colors.revision)
        {
            self.colors.ordinals = Arc::clone(ordinals);
            if *extended {
                self.colors.revision = self.token();
            }
        } else {
            let mut ordinals = (*self.colors.ordinals).clone();
            let keys: Vec<u64> = view.rows().iter().map(|&ix| name_hash(view.name(ix))).collect();
            if ordinals.extend(&keys) {
                self.colors.revision = self.token();
            }
            self.colors.ordinals = Arc::new(ordinals);
        }
        self.map_revision = self.token();
        self.folder = Some(Arc::clone(&view));
        self.requested_path = Some(view.folder_path().to_vec());
        self.order = None;
        self.list_revision = self.token();
        if same_folder && same_root {
            if let Some(old) = &old {
                self.refresh_hover(old);
            }
        } else {
            self.clear_all_hover();
        }
        if !same_root {
            self.on_root_changed();
        }
        self.schedule_name_sort();
        // Selection survives when the item still exists (previews keep pending selections).
        self.selected = selection.and_then(|path| match self.lookup_item(&path) {
            Some(item) => Some(item),
            None if retain_pending => Some(
                self.selected
                    .take()
                    .filter(|item| item.path == path)
                    .unwrap_or_else(|| Item::new(path, NodeMeta::default())),
            ),
            None => None,
        });
        let active = self.active;
        if let Some(saved) = &published {
            if let Some(reused) = completed {
                if let Some(id) = active
                    && let Some(session) = self.session_mut(id)
                {
                    session.preview_blocked = false;
                }
                self.snapshot = Some(saved.clone());
                self.partial = false;
                self.preview_session = None;
                self.last_preview = None;
                self.folder_cache.clear();
                self.saved_map = reused;
                self.scanning = false;
                self.refreshing = false;
                let status = self.active_session().and_then(|s| s.removal_status().cloned());
                self.set_status(status);
            }
            self.folder_cache.retain(|tree| tree.folder_path != view.tree().folder_path);
            self.folder_cache.push_back(Arc::clone(view.tree()));
            while self.folder_cache.len() > FOLDER_CACHE_LIMIT {
                self.folder_cache.pop_front();
            }
        }
        let selected_path = self.selected.as_ref().map(|item| Arc::clone(&item.path));
        let owns =
            active.filter(|id| self.session(*id).is_some_and(|s| s.owns(view.root_path(), view.root_meta().identity)));
        if let Some(id) = owns {
            let partial_preview = self.partial && self.preview_session == Some(id);
            let status_free = self.status.is_none();
            let error_free = self.error.is_none();
            let count = view.root_meta().file_count;
            let (mut status, mut error) = (None, None);
            if let Some(session) = self.session_mut(id) {
                if partial_preview {
                    session.discovered_files = count;
                }
                if completed.is_some() {
                    session.finished = true;
                }
                if published.is_some() && session.finished {
                    session.discovered_files = count;
                    if session.removals.is_empty() {
                        session.cached_files = Some(count);
                    }
                }
                session.remembered =
                    Some(Remembered { folder: Arc::clone(&view), epoch: session.epoch, selection: selected_path });
                if let Some(saved) = &published {
                    session.saved = Some(saved.clone());
                }
                if status_free {
                    status = session.collection_status.take();
                }
                if error_free {
                    error = session.presented_collection_error.clone().or_else(|| session.collection_error.clone());
                    if error.is_some() && session.presented_collection_error.is_none() {
                        session.presented_collection_error = error.clone();
                        session.collection_error = None;
                    }
                }
            }
            if status.is_some() {
                self.set_status_quietly(status);
            }
            if error.is_some() {
                self.set_error_quietly(error);
            }
        }
        self.opening = false;
        self.notify(Regions::MAP | Regions::LIST | Regions::HEADER | Regions::INSPECTOR | Regions::TRAY);
    }

    pub(crate) fn publish(
        &mut self,
        view: Arc<PreparedFolder>,
        snapshot: SnapshotInfo,
        selection: Option<Arc<[u8]>>,
        completed: Option<bool>,
    ) {
        self.install_folder(view, selection, false, Some(snapshot), completed);
        if self.filtering() {
            self.search();
        }
    }

    // -------------------------------------------------------------------------------------
    // Navigation.

    /// Swift `loadFolder`.
    pub(crate) fn load_folder(&mut self, path: Vec<u8>, selection: Option<Arc<[u8]>>) {
        self.deferred.indexed = None;
        let load = self.folder_load.take();
        self.cancel(load);
        let read = self.preview_read.take();
        self.cancel(read);
        self.preview_read_again = None;
        let Some(session) = self.active_session() else { return };
        if session.preview_blocked {
            return;
        }
        let (id, epoch) = (session.id, session.epoch);
        let removals_empty = session.removals.is_empty();
        if self.current_path() != Some(&path[..]) {
            self.clear_all_hover();
        }
        if self.partial && self.preview_session.is_some() {
            self.send(Request::Prioritize { session: id, path: path.clone() });
            self.requested_path = Some(path.clone());
            self.browse = self.token();
            self.selected =
                selection.map(|p| self.lookup_item(&p).unwrap_or_else(|| Item::new(p, NodeMeta::default())));
            self.opening = true;
            self.notify(Regions::HEADER | Regions::INSPECTOR);
            let pending = Pending::PreviewFolder {
                generation: self.generation,
                browse: self.browse,
                session: id,
                epoch,
                path: path.clone(),
            };
            self.folder_load = self.request_view(pending, id, ViewSource::Preview, path, false);
            return;
        }
        let Some(saved) = self.snapshot.clone() else { return };
        self.requested_path = Some(path.clone());
        self.browse = self.token();
        let cached =
            removals_empty.then(|| self.folder_cache.iter().find(|tree| tree.folder_path == path).cloned()).flatten();
        if let Some(tree) =
            cached.filter(|tree| tree.children(orbit_core::view::FolderTree::FOLDER).len() < SYNC_PREPARE_LIMIT)
            && let Some(view) = prepare_folder(tree, self.color_seed().as_ref(), None)
        {
            self.receive_indexed_folder(Arc::new(view), saved, selection);
            return;
        }
        self.opening = true;
        self.notify(Regions::HEADER);
        let pending = Pending::SavedFolder {
            generation: self.generation,
            browse: self.browse,
            epoch,
            snapshot: saved.clone(),
            path: path.clone(),
            selection,
        };
        self.folder_load = self.request_view(pending, id, ViewSource::Saved(saved.id), path, true);
    }

    fn receive_indexed_folder(&mut self, view: Arc<PreparedFolder>, saved: SnapshotInfo, selection: Option<Arc<[u8]>>) {
        if !self.check_requested_target(&view, true) {
            return;
        }
        if self.gesture.is_some() && self.current_path() == Some(view.folder_path()) {
            self.deferred.indexed = Some(DeferredIndexed {
                view,
                snapshot: saved,
                generation: self.generation,
                browse: self.browse,
                session: self.active,
                epoch: self.active_epoch(),
            });
            return;
        }
        self.publish(view, saved, selection, None);
    }

    pub(crate) fn on_saved_view(
        &mut self,
        id: RequestId,
        pending: Pending,
        view: Result<Option<Arc<PreparedFolder>>, String>,
    ) {
        match pending {
            Pending::SavedFolder { generation, browse, epoch, snapshot, path, selection, .. } => {
                if self.folder_load == Some(id) {
                    self.folder_load = None;
                }
                if generation != self.generation || browse != self.browse {
                    return;
                }
                if self.snapshot.as_ref().map(|s| s.id) != Some(snapshot.id) || self.active_epoch() != epoch {
                    self.load_folder(path, selection);
                    return;
                }
                match view {
                    Ok(Some(view)) => self.receive_indexed_folder(view, snapshot, selection),
                    Ok(None) => {
                        self.opening = false;
                        self.set_error(Some("The indexed folder is unavailable.".into()));
                    }
                    Err(error) => {
                        self.opening = false;
                        self.set_error(Some(error));
                    }
                }
            }
            Pending::Finish { generation, session, epoch, snapshot, reused } => {
                if generation != self.generation
                    || self.active != Some(session)
                    || self.session(session).is_none_or(|s| s.stopped)
                {
                    return;
                }
                match view {
                    Ok(Some(view)) => {
                        let identity = self.session(session).and_then(|s| s.identity);
                        if view.root_meta().identity != identity {
                            self.display_mapping_failure(session, "The mapped folder changed.".into());
                            return;
                        }
                        if let Some((target, target_identity)) = self.requested_target.clone()
                            && self.requested_path.as_deref() == Some(&target[..])
                            && view.folder_path() == &target[..]
                            && view.folder_meta().identity != Some(target_identity)
                        {
                            self.requested_target = None;
                            self.open_location(target, true);
                            return;
                        }
                        let completion = DeferredCompletion { session, generation, epoch, snapshot, view, reused };
                        if self.gesture.is_some() {
                            self.deferred.completion = Some(completion);
                        } else {
                            self.run_completion(completion);
                        }
                    }
                    Ok(None) => self.display_mapping_failure(session, "The indexed folder is unavailable.".into()),
                    Err(error) => self.display_mapping_failure(session, error),
                }
            }
            Pending::Baseline { generation, session, epoch, snapshot, route } => {
                let current = generation == self.generation
                    && self.active == Some(session)
                    && self.refreshing
                    && self.session(session).is_some_and(|s| s.epoch == epoch && !s.stopped)
                    && self.requested_path.as_deref() == Some(&route[..]);
                if let (true, Ok(Some(view))) = (current, view) {
                    self.snapshot = Some(snapshot.clone());
                    self.partial = false;
                    self.preview_session = None;
                    let selection = self.selected.as_ref().map(|item| Arc::clone(&item.path));
                    self.publish(view, snapshot, selection, None);
                }
            }
            _ => {}
        }
    }

    /// A held or immediate scan completion (Swift `observeSession` success + `finishScan`).
    pub(crate) fn run_completion(&mut self, completion: DeferredCompletion) {
        let DeferredCompletion { session, generation, epoch, snapshot, view, reused } = completion;
        if self.generation != generation || self.active != Some(session) {
            return;
        }
        let Some(s) = self.session(session) else { return };
        if s.stopped {
            return;
        }
        if s.epoch != epoch {
            let parents: Vec<Vec<u8>> = s.removal_parents.iter().cloned().collect();
            self.snapshot = Some(snapshot);
            self.scanning = false;
            self.refreshing = false;
            self.pending_dirs.extend(parents);
            self.schedule_refresh();
            return;
        }
        self.finish_scan(session, snapshot, view, reused);
    }

    fn finish_scan(&mut self, session: SessionId, saved: SnapshotInfo, view: Arc<PreparedFolder>, reused: bool) {
        let Some(s) = self.session(session) else { return };
        if !s.removals.is_empty() {
            let parents: Vec<Vec<u8>> = s.removal_parents.iter().cloned().collect();
            let status = if s.removing {
                s.removal_status().cloned()
            } else {
                Some(match s.removal_status() {
                    Some(status) => format!("{status} Updating the map…"),
                    None => "Updating the map after removal…".to_owned(),
                })
            };
            self.snapshot = Some(saved.clone());
            self.scanning = false;
            self.refreshing = false;
            self.pending_dirs.extend(parents);
            self.set_status(status);
            if self.preview_session.is_none() {
                self.partial = false;
                let route = self.requested_path.clone().unwrap_or_else(|| saved.root.to_vec());
                let selection = self.selected.as_ref().map(|item| Arc::clone(&item.path));
                self.load_folder(route, selection);
            }
            self.begin_monitoring(session, saved);
            self.schedule_refresh();
            return;
        }
        let preserves_route = self.root_path() == Some(&saved.root[..]);
        let requested = self.requested_path.clone().filter(|p| is_within(p, &saved.root));
        let destination =
            requested.or_else(|| preserves_route.then(|| self.current_path().map(<[u8]>::to_vec)).flatten());
        if let Some(destination) = destination.filter(|d| d[..] != *view.folder_path()) {
            // The route changed while the published view was read: read the latest route.
            self.opening = true;
            let pending = Pending::Finish {
                generation: self.generation,
                session,
                epoch: self.active_epoch(),
                snapshot: saved.clone(),
                reused,
            };
            self.request_view(pending, session, ViewSource::Saved(saved.id), destination, true);
            return;
        }
        let selection = preserves_route.then(|| self.selected.as_ref().map(|item| Arc::clone(&item.path))).flatten();
        self.publish(view, saved.clone(), selection, Some(reused));
        let read = self.preview_read.take();
        self.cancel(read);
        self.preview_read_again = None;
        self.deferred.preview = None;
        self.deferred.completion = None;
        self.out.push(crate::event::Effect::Persist(Persist::LastLocation(saved.root.to_vec())));
        self.begin_monitoring(session, saved);
        self.schedule_refresh();
        self.refresh_volumes();
    }

    pub(crate) fn begin_monitoring(&mut self, session: SessionId, snapshot: SnapshotInfo) {
        if let Some(previous) = self.monitored.take() {
            self.send(Request::StopMonitor { session: previous });
        }
        self.monitored = Some(session);
        self.send(Request::Monitor { session, snapshot });
    }

    fn display_mapping_failure(&mut self, session: SessionId, message: String) {
        if self.active != Some(session) {
            return;
        }
        let read = self.preview_read.take();
        self.cancel(read);
        self.scanning = false;
        self.refreshing = false;
        if self.resolution_request.is_none() {
            self.opening = false;
        }
        self.set_error(Some(message));
        let paused = self.snapshot.is_some() || self.partial;
        self.set_updates_paused(paused);
        if self.partial {
            self.set_status(Some(STATUS_PARTIAL_KEPT.into()));
        }
        self.notify(Regions::HEADER | Regions::FOOTER);
    }

    // -------------------------------------------------------------------------------------
    // Engine notifications.

    pub(crate) fn on_mapping(&mut self, id: SessionId, event: MappingEvent) {
        let active = self.active == Some(id);
        let Some(session) = self.session_mut(id) else { return };
        match event {
            MappingEvent::DiscoveryStarted => {
                if active && !session.stopped {
                    self.title = TITLE_MAPPING;
                    self.notify(Regions::HEADER | Regions::FOOTER);
                }
            }
            MappingEvent::Progress(progress) => {
                if session.progress.as_ref().is_some_and(|old| old.revision > progress.revision) {
                    return;
                }
                if !session.finished
                    && let Some(cached) = progress.cached_file_count
                {
                    session.cached_files = Some(cached);
                }
                let visible = active && !session.stopped && !session.finished;
                if visible {
                    self.item_count = progress.item_count;
                    self.progress_bytes = progress.bytes_scanned;
                    self.scan_path = progress.current_path.clone();
                }
                if let Some(session) = self.session_mut(id) {
                    session.progress = Some(progress);
                }
                // The footer, plus the map's opening view until the first folder is shown. The
                // list is never notified for progress.
                let opening_view = visible && self.folder.is_none();
                self.notify(if opening_view { Regions::FOOTER | Regions::MAP } else { Regions::FOOTER });
            }
            MappingEvent::PreviewChanged { revision, search_ready } => {
                session.preview_revision = session.preview_revision.max(revision);
                session.search_ready |= search_ready;
                if !active || session.stopped || session.finished {
                    return;
                }
                let live = session.live_preview;
                if self.preview_session == Some(id) && self.partial {
                    self.request_preview_read(false);
                } else if !live {
                    self.schedule_count_read();
                }
            }
            MappingEvent::SearchReady => session.search_ready = true,
            MappingEvent::SavedAvailable(snapshot) => {
                session.saved = Some(snapshot.clone());
                session.live_preview = false;
                let (epoch, stopped) = (session.epoch, session.stopped);
                if !active || stopped {
                    return;
                }
                self.snapshot = Some(snapshot.clone());
                self.partial = false;
                self.preview_session = None;
                self.scanning = false;
                self.refreshing = true;
                let route = self.requested_path.clone().unwrap_or_else(|| snapshot.root.to_vec());
                let pending = Pending::Baseline {
                    generation: self.generation,
                    session: id,
                    epoch,
                    snapshot: snapshot.clone(),
                    route: route.clone(),
                };
                self.request_view(pending, id, ViewSource::Saved(snapshot.id), route, true);
                self.notify(Regions::HEADER | Regions::FOOTER);
            }
            MappingEvent::Published { snapshot, reused, root_file_count } => {
                session.saved = Some(snapshot.clone());
                session.finished = true;
                session.cached_files = Some(root_file_count);
                if session.removals.is_empty() {
                    session.discovered_files = root_file_count;
                    if !session.removing {
                        session.live_preview = false;
                    }
                }
                let (epoch, stopped, blocked) = (session.epoch, session.stopped, session.preview_blocked);
                let parents: Vec<Vec<u8>> = session.removal_parents.iter().cloned().collect();
                self.notify(Regions::FOOTER);
                if !active || stopped {
                    return;
                }
                if blocked {
                    self.snapshot = Some(snapshot);
                    self.scanning = false;
                    self.refreshing = false;
                    self.pending_dirs.extend(parents);
                    self.schedule_refresh();
                    return;
                }
                let root = Arc::clone(&snapshot.root);
                let route = self
                    .requested_path
                    .clone()
                    .filter(|p| is_within(p, &root))
                    .or_else(|| {
                        (self.root_path() == Some(&root[..])).then(|| self.current_path().map(<[u8]>::to_vec)).flatten()
                    })
                    .unwrap_or_else(|| root.to_vec());
                let pending = Pending::Finish {
                    generation: self.generation,
                    session: id,
                    epoch,
                    snapshot: snapshot.clone(),
                    reused,
                };
                self.request_view(pending, id, ViewSource::Saved(snapshot.id), route, true);
            }
            MappingEvent::Failed(message) => {
                session.failed = true;
                let stopped = session.stopped;
                self.notify(Regions::FOOTER);
                if active && !stopped {
                    self.display_mapping_failure(id, message);
                }
            }
            MappingEvent::Changes { directories, full_refresh } => {
                if !active || self.monitored != Some(id) || self.updates_paused {
                    return;
                }
                self.pending_dirs.extend(directories);
                self.pending_full_refresh |= full_refresh;
                self.schedule_refresh();
                self.notify(Regions::FOOTER);
            }
            MappingEvent::MonitorUnavailable => {
                if !active || self.monitored != Some(id) {
                    return;
                }
                self.monitored = None;
                self.set_updates_paused(true);
                self.set_status(Some("Saved map available. Automatic updates unavailable; use Refresh.".into()));
            }
            MappingEvent::RemovalReplay { removals, affected_parents, full_refresh } => {
                self.on_removal_replay(id, removals, affected_parents, full_refresh);
            }
        }
    }

    fn on_removal_replay(&mut self, id: SessionId, removals: Vec<ScanRemoval>, parents: Vec<Vec<u8>>, full: bool) {
        let limit = self.cfg.recovery_parent_limit;
        let active = self.active == Some(id);
        let Some(session) = self.session_mut(id) else { return };
        session.epoch += 1;
        let overflow = session.merge_removals(removals);
        let mut combined = session.removal_parents.clone();
        combined.extend(parents.iter().cloned());
        let exceeded = combined.len() > limit;
        if exceeded || full {
            session.pending_full_refresh = true;
        } else {
            session.removal_parents = combined;
            let epoch = session.epoch;
            for parent in &parents {
                session.removal_versions.insert(parent.clone(), epoch);
            }
        }
        session.recovery_pending = true;
        let blocked = overflow || full || exceeded;
        if blocked {
            session.preview_blocked = true;
        }
        if !active {
            return;
        }
        if exceeded || full {
            self.pending_full_refresh = true;
        } else {
            self.pending_dirs.extend(parents);
        }
        self.deferred.preview = None;
        if blocked {
            self.clear_presentation(false);
            self.set_status(Some(STATUS_UPDATING_AFFECTED.into()));
        }
        self.folder_cache.clear();
        if self.filtering() {
            self.search();
        }
        if self.partial && self.preview_session.is_some() {
            self.last_preview = None;
            self.request_preview_read(true);
        } else if let Some(saved) = &self.snapshot {
            let route = self.requested_path.clone().unwrap_or_else(|| saved.root.to_vec());
            let selection = self.selected.as_ref().map(|item| Arc::clone(&item.path));
            self.load_folder(route, selection);
        }
        self.schedule_refresh();
    }

    // -------------------------------------------------------------------------------------
    // Discovered-file counts of mappings without a visible preview (0.5 s throttle).

    fn schedule_count_read(&mut self) {
        let now = self.now;
        let Some(session) = self.active_session() else { return };
        if session.count_read.is_some() || self.count_timer.is_some() {
            return;
        }
        if let Some(last) = session.last_count_read {
            let elapsed = now.saturating_duration_since(last);
            if elapsed < COUNT_READ_INTERVAL {
                self.count_timer = Some(self.timer(TimerKind::CountRead, COUNT_READ_INTERVAL - elapsed));
                return;
            }
        }
        self.read_discovered_count();
    }

    pub(crate) fn read_discovered_count(&mut self) {
        let now = self.now;
        let Some(session) = self.active_session() else { return };
        if session.finished || session.stopped || session.live_preview || session.count_read.is_some() {
            return;
        }
        let (id, epoch, removals) = (session.id, session.epoch, Arc::clone(&session.removals));
        let request =
            self.request(Pending::Count { session: id, epoch }, Request::DiscoveredCount { session: id, removals });
        if let Some(session) = self.session_mut(id) {
            session.count_read = Some(request);
            session.last_count_read = Some(now);
        }
    }

    pub(crate) fn on_count(&mut self, id: RequestId, session: SessionId, epoch: u64, count: Result<u64, String>) {
        let active = self.active == Some(session);
        let Some(s) = self.session_mut(session) else { return };
        if s.count_read == Some(id) {
            s.count_read = None;
        }
        if !active || s.finished || s.stopped || s.live_preview || s.epoch != epoch {
            return;
        }
        if let Ok(count) = count {
            s.discovered_files = count;
            self.notify(Regions::FOOTER);
        }
    }

    // -------------------------------------------------------------------------------------
    // Pause, resume, refresh.

    /// Swift `cancelScan`: stop mapping (or pause updates), keep what is shown.
    pub(crate) fn pause(&mut self) {
        let resolution = self.resolution_request.take();
        self.cancel(resolution);
        self.resolution = self.token();
        let attached = self.active;
        self.stop_work(false);
        self.active = attached;
        self.attach_validation(None);
        self.generation = self.token();
        self.scanning = false;
        self.refreshing = false;
        let paused = self.snapshot.is_some() || self.partial;
        self.set_updates_paused(paused);
        let status = if self.is_removing() {
            self.active_session().and_then(|s| s.removal_status().cloned())
        } else if self.partial {
            Some(STATUS_PARTIAL_KEPT.into())
        } else if self.snapshot.is_none() {
            Some("Scan cancelled.".into())
        } else {
            Some("Updates paused. Refresh to resume.".into())
        };
        self.set_status(status);
        if self.filtering() {
            self.search();
        }
        if let Some(id) = attached {
            self.pump_collection(id);
        }
    }

    pub(crate) fn resume(&mut self) {
        if let Some(root) = self.active_session().map(|s| s.root.to_vec()) {
            self.open_location(root, false);
        }
    }

    pub(crate) fn rescan(&mut self) {
        let root = self.root_path().map(<[u8]>::to_vec).or_else(|| self.active_session().map(|s| s.root.to_vec()));
        if let Some(root) = root {
            self.open_location(root, true);
        }
    }

    pub(crate) fn schedule_refresh(&mut self) {
        let recovery = self.active_session().is_some_and(|s| s.recovery_pending);
        if self.refreshing
            || self.scanning
            || self.is_removing()
            || (self.updates_paused && !recovery)
            || self.refresh_timer.is_some()
            || self.refresh_request.is_some()
            || (!self.pending_full_refresh && self.pending_dirs.is_empty())
        {
            return;
        }
        self.refresh_timer = Some(self.timer(TimerKind::Refresh, REFRESH_DELAY));
    }

    pub(crate) fn refresh_changed_folders(&mut self) {
        let Some(saved) = self.snapshot.clone() else { return };
        if self.refreshing || self.scanning || self.is_removing() {
            return;
        }
        if self.pending_full_refresh {
            self.pending_full_refresh = false;
            self.pending_dirs.clear();
            if let Some(session) = self.active_session_mut() {
                session.pending_full_refresh = false;
            }
            self.open_location(saved.root.to_vec(), true);
            return;
        }
        if self.pending_dirs.is_empty() {
            return;
        }
        let paths: Vec<Vec<u8>> = std::mem::take(&mut self.pending_dirs).into_iter().collect();
        let Some(session) = self.active_session() else { return };
        let versions: Vec<(Vec<u8>, u64)> = session.removal_versions.iter().map(|(p, v)| (p.clone(), *v)).collect();
        let (id, epoch, root) = (session.id, session.epoch, Arc::clone(&session.root));
        self.refreshing = true;
        let status = match &self.removal_result_status {
            Some(status) => format!("{status} Updating the map…"),
            None => "Updating changed folders…".to_owned(),
        };
        self.set_status(Some(status));
        let pending =
            Pending::Refresh { generation: self.generation, session: Some(id), epoch, paths: paths.clone(), versions };
        let request = crate::backend::RefreshRequest { session: id, root, snapshot: saved.id, paths };
        self.refresh_request = Some(self.request(pending, Request::Refresh(request)));
    }

    pub(crate) fn on_refreshed(&mut self, id: RequestId, pending: Pending, result: Result<SnapshotInfo, String>) {
        let Pending::Refresh { generation, session, epoch, paths, versions } = pending else { return };
        if self.refresh_request == Some(id) {
            self.refresh_request = None;
        }
        if generation != self.generation {
            return;
        }
        match result {
            Ok(updated) => {
                self.snapshot = Some(updated.clone());
                if let Some(s) = session.and_then(|id| self.session_mut(id)) {
                    s.saved = Some(updated.clone());
                    for (path, version) in versions {
                        if s.removal_versions.get(&path) == Some(&version) {
                            s.removal_versions.remove(&path);
                            s.removal_parents.remove(&path);
                            let kept: Vec<ScanRemoval> =
                                s.removals.iter().filter(|r| parent_or_root(&r.path) != &path[..]).cloned().collect();
                            s.removals = kept.into();
                        }
                    }
                    s.recovery_pending = s.epoch != epoch || !s.removal_parents.is_empty();
                    if !s.recovery_pending {
                        s.preview_blocked = false;
                    }
                }
                self.folder_cache.clear();
                self.refreshing = false;
                self.saved_map = false;
                let removals_empty = self.active_session().is_none_or(|s| s.removals.is_empty());
                if self.partial && removals_empty {
                    self.preview_session = None;
                    self.partial = false;
                    if let Some(s) = self.active_session_mut() {
                        s.live_preview = false;
                    }
                }
                let status = self.removal_result_status.take().unwrap_or_else(|| "Updated changed folders.".into());
                self.set_status(Some(status));
                if !self.partial {
                    let route = self
                        .requested_path
                        .clone()
                        .or_else(|| self.current_path().map(<[u8]>::to_vec))
                        .unwrap_or_else(|| updated.root.to_vec());
                    let selection = self.selected.as_ref().map(|item| Arc::clone(&item.path));
                    self.load_folder(route, selection);
                }
                self.schedule_refresh();
            }
            Err(error) => {
                self.refreshing = false;
                self.pending_dirs.extend(paths);
                self.set_updates_paused(true);
                if let Some(session) = self.monitored.take() {
                    self.send(Request::StopMonitor { session });
                }
                let prefix = self.removal_result_status.as_ref().map(|s| format!("{s} ")).unwrap_or_default();
                let prior = self.removal_result_status.is_some().then(|| self.error.clone()).flatten();
                self.set_status(Some(format!("{prefix}Map kept. Update failed; use Refresh to retry.")));
                self.removal_result_status = None;
                let message = match prior {
                    Some(prior) => format!("{prior}\nMap update failed: {error}"),
                    None => error,
                };
                self.set_error(Some(message));
            }
        }
        self.notify(Regions::FOOTER | Regions::HEADER);
    }
}
