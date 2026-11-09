//! The production [`Backend`] of the app state over `orbit-core`.
//!
//! ```text
//! EngineBackend::new(Arc<Native>)            discards obsolete caches once, locates the index
//!   resolve      cloud exclusion -> covering session (reachable, removals not replaced)
//!                -> SavedMaps::coverage (saved map or stage of an ancestor) -> the path itself
//!   start_mapping   worker thread per session (see `mapping`): journal recovery of the saved
//!                   map, reuse, MappingSession, throttled Progress / PreviewChanged, Published
//!   view / search / discovered_file_count   the session's live mapping, else its saved map
//!   monitor      platform change monitor -> debounced MappingEvent::Changes (see `monitor`)
//!   refresh      SavedMaps::refresh; persists the history position of the batches it covers
//!   remove       orbit_core::removal::remove (Trash or permanent delete, journaled)
//! ```
//!
//! Threading: every method may be called concurrently from the host's background executor and
//! may block. Registry locks are held only to copy handles; engine calls run outside them.
//! Long-running work (mapping, monitoring) runs on the backend's own threads and reports
//! through the [`Notifier`] at most once per `progress_interval` / `change_debounce`, so the
//! host's event queue never builds a backlog.

mod convert;
mod mapping;
mod monitor;
mod registry;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use orbit_app_state::backend::{
    MappingRequest, RefreshRequest, RemovalOutcome, RemovalRequest, Resolution, ResolveCandidate, SearchReply,
    SearchRequest, SearchSource, ViewRequest, ViewSource,
};
use orbit_app_state::{Backend, Event, MappingEvent, Notifier, SessionId, SnapshotInfo, Volume};
use orbit_core::ScanRemoval;
use orbit_core::discovery::DiscoveryHooks;
use orbit_core::paths::{is_within, parent, standardize};
use orbit_core::policy::{CloudValidator, PlatformPolicyFs, RemovalPolicy, cloud_exclusions};
use orbit_core::removal::{RemovalMode, remove};
use orbit_core::saved::SavedMaps;
use orbit_core::schema::SnapshotInfo as CoreSnapshot;
use orbit_core::session::MappingSession;
use orbit_core::store::{IndexPaths, discard_obsolete_caches};
use orbit_core::view::FolderTree;
use orbit_core::{CoreError, Result as CoreResult};
use orbit_platform::posix::{lstat, path_from_bytes};
use orbit_platform::{Cancellation, ChangeCheckpoint, FileIdentity, Platform, PlatformError, ProtectedPaths};
use parking_lot::Mutex;

use crate::convert::{FOLDER_UNAVAILABLE, MAP_UNAVAILABLE, MAP_UPDATING, ROOT_CHANGED, message};
use crate::monitor::MonitorRun;
use crate::registry::{Entry, Snapshots};

/// How long removal-policy inputs (cloud roots, mounted volumes) are reused between checks.
const POLICY_INPUTS_TTL: Duration = Duration::from_secs(2);

/// Configuration of an [`EngineBackend`].
#[derive(Clone)]
pub struct BackendOptions {
    /// Where saved maps, stages, locks and the removal journal live.
    pub paths: IndexPaths,
    /// The user's home folder (cloud storage and protected locations).
    pub home: PathBuf,
    /// Discovery workers per mapping.
    pub workers: usize,
    /// Minimum time between two rounds of progress/preview events of one mapping.
    pub progress_interval: Duration,
    /// Monitor batches arriving within this window are merged into one `Changes` event.
    pub change_debounce: Duration,
    /// Watch saved maps for filesystem changes. Off: `monitor` does nothing (tests).
    pub monitor_changes: bool,
    /// How long a new mapping waits for a stage another session still holds.
    pub busy_retry: Duration,
    /// Discovery hooks for tests and diagnostics.
    pub hooks: Option<Arc<dyn DiscoveryHooks>>,
}

impl std::fmt::Debug for BackendOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendOptions").field("paths", &self.paths).field("home", &self.home).finish_non_exhaustive()
    }
}

impl BackendOptions {
    /// The platform's index locations and the current user's home.
    pub fn for_platform<P: Platform>(platform: &P) -> Self {
        Self::new(IndexPaths::for_platform(platform), home_dir())
    }

    pub fn new(paths: IndexPaths, home: PathBuf) -> Self {
        Self {
            paths,
            home,
            workers: orbit_core::discovery::DEFAULT_WORKERS,
            progress_interval: Duration::from_millis(100),
            change_debounce: Duration::from_millis(250),
            monitor_changes: true,
            busy_retry: orbit_core::session::STAGE_BUSY_RETRY,
            hooks: None,
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| PathBuf::from("/"))
}

/// The engine behind [`orbit_app_state::AppState`]. Cheap to share (`Arc`); `Send + Sync`.
pub struct EngineBackend<P: Platform> {
    inner: Arc<Inner<P>>,
}

impl<P: Platform> std::fmt::Debug for EngineBackend<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineBackend").field("options", &self.inner.options).finish_non_exhaustive()
    }
}

/// Removal-policy facts that are expensive to gather per item.
struct PolicyInputs {
    home: Vec<u8>,
    protected: ProtectedPaths,
    clouds: Vec<Vec<u8>>,
    volumes: Vec<Vec<u8>>,
}

pub(crate) struct Inner<P: Platform> {
    pub platform: Arc<P>,
    pub options: BackendOptions,
    pub maps: Arc<SavedMaps<P>>,
    sessions: Mutex<HashMap<SessionId, Arc<Entry<P>>>>,
    snapshots: Mutex<Snapshots>,
    policy: Mutex<Option<(Instant, Arc<PolicyInputs>)>>,
}

impl<P: Platform> EngineBackend<P> {
    /// The app's backend: deletes the cache folders of earlier builds (once, here), then uses
    /// the platform's index locations. Saved maps are recovered from the removal journal lazily,
    /// when a mapping of their root starts.
    pub fn new(platform: Arc<P>) -> Self {
        discard_obsolete_caches(&platform.app_dirs());
        let options = BackendOptions::for_platform(&*platform);
        Self::with_options(platform, options)
    }

    /// A backend with explicit locations and tuning (tests, tools). Does not touch obsolete
    /// caches.
    pub fn with_options(platform: Arc<P>, options: BackendOptions) -> Self {
        let maps = Arc::new(SavedMaps::new(Arc::clone(&platform), options.paths.clone()));
        Self {
            inner: Arc::new(Inner {
                platform,
                options,
                maps,
                sessions: Mutex::default(),
                snapshots: Mutex::default(),
                policy: Mutex::new(None),
            }),
        }
    }

    pub fn options(&self) -> &BackendOptions {
        &self.inner.options
    }

    /// The orbit-core snapshot behind an app-state snapshot id.
    pub fn core_snapshot(&self, id: u64) -> Option<CoreSnapshot> {
        self.inner.snapshots.lock().get(id)
    }

    /// Stops every mapping and monitor and waits for their threads. Stages stay resumable.
    pub fn shut_down(&self) {
        let entries: Vec<Arc<Entry<P>>> = self.inner.sessions.lock().drain().map(|(_, e)| e).collect();
        for entry in &entries {
            entry.stop();
        }
        for entry in entries {
            entry.shut_down();
        }
    }
}

impl<P: Platform> Drop for EngineBackend<P> {
    fn drop(&mut self) {
        self.shut_down();
    }
}

impl<P: Platform> Inner<P> {
    fn entry(&self, session: SessionId) -> Option<Arc<Entry<P>>> {
        self.sessions.lock().get(&session).cloned()
    }

    /// Identity of the directory at `root` (a final symlink is not followed).
    pub fn root_identity(&self, root: &[u8]) -> Result<FileIdentity, String> {
        let path = path_from_bytes(root);
        match self.platform.without_materialization(|| self.platform.open_root(path)) {
            Ok(Ok((_, identity))) => Ok(identity),
            Ok(Err(error)) | Err(error) => Err(open_error(root, &error)),
        }
    }

    /// A resumable stage of `root` exists (Swift `ResumableStage.hasCandidate`).
    pub fn has_stage(&self, root: &[u8], identity: FileIdentity) -> bool {
        let database = orbit_core::lease::stage_database_path(&self.options.paths.staging, root, identity);
        lstat(&database).is_ok_and(|info| info.is_file() && info.size > 0)
    }

    /// The app's view of a saved map and its root file count.
    pub fn describe(&self, snapshot: &CoreSnapshot) -> Result<(SnapshotInfo, u64), String> {
        let node = self
            .maps
            .store()
            .node(snapshot, &snapshot.root_path, None)
            .map_err(|e| message(&e))?
            .ok_or_else(|| MAP_UNAVAILABLE.to_owned())?;
        let id = self.snapshots.lock().register(snapshot);
        let info = SnapshotInfo {
            id,
            root: Arc::from(snapshot.root_path.as_slice()),
            root_identity: node.meta.identity,
            indexed_at: SystemTime::UNIX_EPOCH + Duration::from_secs_f64(snapshot.scanned_at.max(0.0)),
            skipped_count: snapshot.skipped,
            needs_boot_upgrade: convert::needs_boot_upgrade(snapshot),
        };
        Ok((info, node.meta.file_count))
    }

    fn policy_inputs(&self) -> Arc<PolicyInputs> {
        let mut cached = self.policy.lock();
        if let Some((at, inputs)) = cached.as_ref()
            && at.elapsed() < POLICY_INPUTS_TTL
        {
            return Arc::clone(inputs);
        }
        let platform = &*self.platform;
        let home = &self.options.home;
        let fs = PlatformPolicyFs(platform);
        let known: Vec<Vec<u8>> = platform.cloud_roots(home).iter().map(|p| bytes(p).to_vec()).collect();
        let inputs = Arc::new(PolicyInputs {
            home: bytes(home).to_vec(),
            protected: platform.protected_paths(home),
            clouds: cloud_exclusions::<_, Vec<u8>>(&known, &[], &fs),
            volumes: platform.volumes().into_iter().filter(|v| !v.is_boot).map(|v| bytes(&v.path).to_vec()).collect(),
        });
        *cached = Some((Instant::now(), Arc::clone(&inputs)));
        inputs
    }

    fn cloud_exclusion(&self, path: &[u8]) -> Option<String> {
        let inputs = self.policy_inputs();
        let fs = PlatformPolicyFs(&*self.platform);
        CloudValidator::new(&fs, &inputs.clouds, false).exclusion_reason(path).map(str::to_owned)
    }

    fn saved_snapshot(&self, id: u64) -> Result<CoreSnapshot, String> {
        self.snapshots.lock().get(id).ok_or_else(|| MAP_UNAVAILABLE.to_owned())
    }

    /// The saved folder at `path`, or its nearest indexed ancestor.
    fn saved_view(
        &self,
        snapshot: &CoreSnapshot,
        path: &[u8],
        removals: &[ScanRemoval],
        cancel: &Cancellation,
    ) -> Result<FolderTree, String> {
        let root = snapshot.root_path.as_slice();
        let mut path = if is_within(path, root) { path } else { root };
        loop {
            match self.maps.view(snapshot, path, removals, Some(cancel)) {
                Ok(tree) => return Ok(tree),
                Err(CoreError::Invalid(_)) if path != root => path = parent(path).unwrap_or(root),
                Err(CoreError::Invalid(_)) => return Err(FOLDER_UNAVAILABLE.to_owned()),
                Err(error) => return Err(message(&error)),
            }
        }
    }

    /// The live folder at `path`; with `fallback`, its nearest known ancestor.
    fn live_view(
        session: &MappingSession<P>,
        path: &[u8],
        removals: &[ScanRemoval],
        fallback: bool,
        cancel: &Cancellation,
    ) -> Result<Option<FolderTree>, String> {
        let root = session.root();
        let mut path = if is_within(path, root) || !fallback { path } else { root };
        loop {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            match session.view(path, removals) {
                Ok(Some(tree)) => return Ok(Some(tree)),
                Ok(None) if fallback && path != root && is_within(path, root) => path = parent(path).unwrap_or(root),
                Ok(None) => return Ok(None),
                Err(error) => return Err(message(&error)),
            }
        }
    }
}

fn bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_encoded_bytes()
}

fn open_error(path: &[u8], error: &PlatformError) -> String {
    let name = orbit_core::paths::file_name(path);
    let name = String::from_utf8_lossy(if name.is_empty() { path } else { name });
    match error {
        PlatformError::Dataless { .. } => orbit_core::policy::CLOUD_DATALESS.to_owned(),
        PlatformError::Os { code, .. } => {
            let error = std::io::Error::from_raw_os_error(*code);
            if error.kind() == std::io::ErrorKind::NotFound {
                format!("“{name}” couldn’t be opened because there is no such file.")
            } else {
                format!("“{name}” couldn’t be opened: {error}")
            }
        }
        other => format!("“{name}” couldn’t be opened: {other}"),
    }
}

fn result_message<T>(result: CoreResult<T>) -> Result<T, String> {
    result.map_err(|e| message(&e))
}

impl<P: Platform> Backend for EngineBackend<P> {
    fn resolve(
        &self,
        path: &[u8],
        force: bool,
        candidate: Option<&ResolveCandidate>,
        cancel: &Cancellation,
    ) -> Resolution {
        let inner = &*self.inner;
        let Some(path) = standardize(path) else {
            return Resolution {
                root: path.to_vec(),
                exclusion: Some(orbit_core::policy::NOT_LOCAL.into()),
                ..Resolution::default()
            };
        };
        let excluded =
            |reason: String| Resolution { root: path.clone(), exclusion: Some(reason), ..Resolution::default() };
        if let Some(reason) = inner.cloud_exclusion(&path) {
            return excluded(reason);
        }
        let target = match inner.root_identity(&path) {
            Ok(identity) => identity,
            Err(reason) => return excluded(reason),
        };
        let platform = &*inner.platform;
        let resolved = platform.without_materialization(|| {
            if let Some(candidate) = candidate.filter(|_| !force) {
                let replaced = candidate.covering_removals.iter().any(|removal| {
                    lstat(path_from_bytes(&removal.path)).is_ok_and(|info| info.identity != removal.identity)
                });
                let reachable = !replaced
                    && is_within(&path, &candidate.root)
                    && orbit_core::saved::can_reach(platform, &candidate.root, &path, candidate.identity, None, cancel)
                        .unwrap_or(false);
                if reachable {
                    return Resolution {
                        root: candidate.root.to_vec(),
                        root_identity: Some(candidate.identity),
                        target_identity: Some(target),
                        used_candidate: true,
                        exclusion: None,
                    };
                }
            }
            if !force
                && !cancel.is_cancelled()
                && let Ok(Some(coverage)) = inner.maps.coverage(&path, cancel)
            {
                return Resolution {
                    root: coverage.root,
                    root_identity: Some(coverage.root_identity),
                    target_identity: Some(coverage.requested_identity),
                    used_candidate: false,
                    exclusion: None,
                };
            }
            Resolution {
                root: path.clone(),
                root_identity: Some(target),
                target_identity: Some(target),
                ..Resolution::default()
            }
        });
        resolved.unwrap_or_else(|error| excluded(open_error(&path, &error)))
    }

    fn start_mapping(&self, request: &MappingRequest, notify: Notifier) {
        let entry = Arc::new(Entry::new(request.root.to_vec()));
        let replaced: Vec<Arc<Entry<P>>> = {
            let mut sessions = self.inner.sessions.lock();
            let same_root: Vec<SessionId> = sessions
                .iter()
                .filter(|(id, e)| **id != request.session && e.root == entry.root)
                .map(|(id, _)| *id)
                .collect();
            let mut replaced: Vec<_> = same_root.iter().filter_map(|id| sessions.remove(id)).collect();
            replaced.extend(sessions.insert(request.session, Arc::clone(&entry)));
            replaced
        };
        for previous in &replaced {
            previous.stop();
        }
        let (inner, worker_entry, request) = (Arc::clone(&self.inner), Arc::clone(&entry), request.clone());
        let failed = Arc::clone(&notify);
        let session = request.session;
        let spawned = std::thread::Builder::new()
            .name("orbit-mapping".into())
            .spawn(move || mapping::run(&inner, &worker_entry, &request, notify, replaced));
        match spawned {
            Ok(worker) => entry.set_worker(worker),
            Err(error) => failed(Event::Mapping { session, event: MappingEvent::Failed(error.to_string()) }),
        }
    }

    fn stop_mapping(&self, session: SessionId) {
        if let Some(entry) = self.inner.entry(session) {
            entry.stop();
        }
    }

    fn prioritize(&self, session: SessionId, path: &[u8]) {
        if let Some(live) = self.inner.entry(session).and_then(|e| e.live()) {
            live.prioritize(Some(path));
        }
    }

    fn view(&self, request: &ViewRequest, cancel: &Cancellation) -> Result<Option<FolderTree>, String> {
        let inner = &*self.inner;
        match request.source {
            ViewSource::Preview => {
                let Some(entry) = inner.entry(request.session) else { return Ok(None) };
                if let Some(live) = entry.live() {
                    return Inner::live_view(&live, &request.path, &request.removals, request.fallback, cancel);
                }
                match entry.saved() {
                    Some(saved) => inner.saved_view(&saved, &request.path, &request.removals, cancel).map(Some),
                    None => Ok(None),
                }
            }
            ViewSource::Saved(id) => {
                let saved = inner.saved_snapshot(id)?;
                inner.saved_view(&saved, &request.path, &request.removals, cancel).map(Some)
            }
        }
    }

    fn discovered_file_count(
        &self,
        session: SessionId,
        removals: &[ScanRemoval],
        cancel: &Cancellation,
    ) -> Result<u64, String> {
        let inner = &*self.inner;
        let Some(entry) = inner.entry(session) else { return Ok(0) };
        if let Some(live) = entry.live() {
            return result_message(live.discovered_file_count(removals))?.ok_or_else(|| MAP_UPDATING.to_owned());
        }
        match entry.saved() {
            Some(saved) => {
                let tree = inner.saved_view(&saved, &saved.root_path, removals, cancel)?;
                Ok(tree.node(FolderTree::FOLDER).meta.file_count)
            }
            None => Ok(0),
        }
    }

    fn search(&self, request: &SearchRequest, cancel: &Cancellation) -> Result<SearchReply, String> {
        let inner = &*self.inner;
        let saved = match request.source {
            SearchSource::Preview => {
                let entry = inner.entry(request.session).ok_or_else(|| MAP_UPDATING.to_owned())?;
                if let Some(live) = entry.live() {
                    let revision = live.revision();
                    let results = result_message(live.search(&request.query, &request.removals, request.limit))?
                        .ok_or_else(|| MAP_UPDATING.to_owned())?;
                    return Ok(convert::search_reply(results, revision));
                }
                entry.saved().ok_or_else(|| MAP_UPDATING.to_owned())?
            }
            // The saved map's order (largest first, then path) is stable across hand-offs.
            SearchSource::Saved { snapshot, .. } => inner.saved_snapshot(snapshot)?,
        };
        let results =
            result_message(inner.maps.search(&saved, &request.query, &request.removals, request.limit, Some(cancel)))?;
        Ok(convert::search_reply(results, 0))
    }

    fn identity(&self, path: &[u8]) -> Option<FileIdentity> {
        lstat(path_from_bytes(path)).ok().map(|info| info.identity)
    }

    fn rejection_reason(&self, path: &[u8], root: &[u8]) -> Option<String> {
        let inner = &*self.inner;
        let inputs = inner.policy_inputs();
        let fs = PlatformPolicyFs(&*inner.platform);
        RemovalPolicy::new(&fs, root, &inputs.home, &inputs.protected, &inputs.clouds, &inputs.volumes)
            .rejection_reason(path)
    }

    fn remove(&self, request: &RemovalRequest) -> RemovalOutcome {
        let inner = &*self.inner;
        let Some(root_identity) = request.root_identity else {
            return RemovalOutcome {
                failures: vec![ROOT_CHANGED.into()],
                root_changed: true,
                ..RemovalOutcome::default()
            };
        };
        let (items, failures) = convert::removal_items(&request.items);
        let core_request = orbit_core::removal::RemovalRequest { root: request.root.to_vec(), root_identity, items };
        let mode = if request.permanently { RemovalMode::Delete } else { RemovalMode::Trash };
        let outcome =
            remove(&*inner.platform, inner.maps.paths(), core_request, mode, &inner.options.home, &Cancellation::new());
        convert::removal_outcome(outcome, request.parent_limit, failures)
    }

    fn refresh(&self, request: &RefreshRequest, cancel: &Cancellation) -> Result<SnapshotInfo, String> {
        let inner = &*self.inner;
        let saved = inner.saved_snapshot(request.snapshot)?;
        let entry = inner.entry(request.session);
        // Only history whose folders this refresh re-lists may be marked as applied. A failed
        // refresh forgets those batches: the retry then keeps the older position (safe).
        let checkpoint = entry.as_ref().and_then(|e| e.ledger.lock().settle(&request.paths));
        let updated = result_message(inner.maps.refresh(&saved, &request.paths, checkpoint.as_ref(), cancel))?;
        let (info, _) = inner.describe(&updated)?;
        if let Some(entry) = entry {
            entry.set_saved(updated.clone());
        }
        // The rewritten file needs a new accelerator; the next search uses SQL until it is ready.
        let _ = inner.maps.warm_search(updated);
        Ok(info)
    }

    fn monitor(&self, session: SessionId, snapshot: &SnapshotInfo, notify: Notifier) {
        let inner = &*self.inner;
        if !inner.options.monitor_changes {
            return;
        }
        let unavailable =
            |notify: &Notifier| notify(Event::Mapping { session, event: MappingEvent::MonitorUnavailable });
        let (Some(entry), Ok(saved)) = (inner.entry(session), inner.saved_snapshot(snapshot.id)) else {
            return unavailable(&notify);
        };
        entry.stop_monitor();
        let root = path_from_bytes(&saved.root_path).to_path_buf();
        let since = (!saved.event_identity.is_empty()).then(|| ChangeCheckpoint {
            root: root.clone(),
            identity: saved.event_identity.clone(),
            event_id: saved.event_id,
        });
        entry.ledger.lock().clear();
        let started = MonitorRun::start(
            inner.platform.new_monitor(),
            root,
            &inner.options.paths.exclusion_roots(),
            since.as_ref(),
            session,
            Arc::clone(&notify),
            Arc::clone(&entry.ledger),
            inner.options.change_debounce,
        );
        match started {
            Ok(run) => entry.replace_monitor(Some(run)),
            Err(_) => unavailable(&notify),
        }
    }

    fn stop_monitor(&self, session: SessionId) {
        if let Some(entry) = self.inner.entry(session) {
            entry.stop_monitor();
        }
    }

    fn volumes(&self) -> Vec<Volume> {
        self.inner
            .platform
            .volumes()
            .into_iter()
            .map(|v| Volume {
                path: bytes(&v.path).to_vec(),
                name: v.name,
                capacity: v.total_bytes,
                available: v.available_bytes,
            })
            .collect()
    }

    fn directories(&self, paths: &[Vec<u8>]) -> Vec<Vec<u8>> {
        paths.iter().filter(|p| std::fs::metadata(path_from_bytes(p)).is_ok_and(|m| m.is_dir())).cloned().collect()
    }
}
