//! Test support: an in-memory [`FakeBackend`] and a [`Harness`] that drives an [`AppState`] with
//! explicit time. Requests are held until the test performs them, so races are reproduced by
//! choosing the order in which replies arrive — no threads, no sleeps.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use orbit_core::fold::{fold, fold_query};
use orbit_core::paths::{file_name, is_within, join};
use orbit_core::view::{
    FolderTree, FolderTreeBuilder, NodeFlags, NodeIx, NodeMeta, VIEW_DEPTH, VIEW_DESCENDANT_BUDGET,
    ViewSource as TreeSource,
};
use orbit_platform::{Cancellation, FileIdentity};

use crate::backend::{
    Backend, MappingRequest, Notifier, RefreshRequest, RemovalOutcome, RemovalRequest, Request, Resolution,
    ResolveCandidate, SearchReply, SearchRequest, ViewRequest, ViewSource, perform,
};
use crate::event::{ComputeJob, Effect, Event, MappingEvent, Persist, PlatformAction, Regions, Timer};
use crate::ids::{RequestId, SessionId};
use crate::model::{Item, ScanRemoval, SnapshotInfo, Volume};
use crate::paths::parent_or_root;
use crate::state::{AppState, Config};

#[derive(Clone, Copy, Debug)]
struct Entry {
    is_dir: bool,
    size: u64,
    identity: FileIdentity,
    accessible: bool,
    symlink: bool,
}

#[derive(Debug, Default)]
struct Fs {
    entries: BTreeMap<Vec<u8>, Entry>,
    next_ino: u64,
    revision: u64,
    snapshot_ids: u64,
    hidden: BTreeSet<Vec<u8>>,
    saved: HashMap<Vec<u8>, SnapshotInfo>,
    protected: Vec<Vec<u8>>,
    exclusions: HashMap<Vec<u8>, String>,
    volumes: Vec<Volume>,
    fail_refresh: Option<String>,
    sessions: HashMap<SessionId, Vec<u8>>,
    log: Vec<String>,
}

/// An in-memory filesystem + saved-map registry implementing [`Backend`].
#[derive(Debug, Default)]
pub struct FakeBackend {
    fs: Mutex<Fs>,
}

impl FakeBackend {
    pub fn new() -> Self {
        let backend = Self::default();
        backend.add_dir(b"/");
        backend
    }

    fn fs(&self) -> MutexGuard<'_, Fs> {
        self.fs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn insert(&self, path: &[u8], is_dir: bool, size: u64) -> FileIdentity {
        let mut fs = self.fs();
        // Parents exist implicitly.
        let mut parents = Vec::new();
        let mut current = parent_or_root(path);
        while !fs.entries.contains_key(current) {
            parents.push(current.to_vec());
            if current == b"/" {
                break;
            }
            current = parent_or_root(current);
        }
        for parent in parents {
            fs.next_ino += 1;
            let identity = FileIdentity { dev: 1, ino: fs.next_ino };
            fs.entries.insert(parent, Entry { is_dir: true, size: 0, identity, accessible: true, symlink: false });
        }
        fs.next_ino += 1;
        let identity = FileIdentity { dev: 1, ino: fs.next_ino };
        fs.entries.insert(path.to_vec(), Entry { is_dir, size, identity, accessible: true, symlink: false });
        fs.revision += 1;
        identity
    }

    pub fn add_dir(&self, path: &[u8]) -> FileIdentity {
        self.insert(path, true, 0)
    }

    pub fn add_file(&self, path: &[u8], size: u64) -> FileIdentity {
        self.insert(path, false, size)
    }

    pub fn add_symlink(&self, path: &[u8]) {
        self.insert(path, false, 1);
        if let Some(entry) = self.fs().entries.get_mut(path) {
            entry.symlink = true;
        }
    }

    /// Deletes `path` and everything below it.
    pub fn delete(&self, path: &[u8]) {
        let mut fs = self.fs();
        fs.entries.retain(|p, _| !is_within(p, path));
        fs.revision += 1;
    }

    /// Replaces `path` with a new item (new identity) of the same kind and size.
    pub fn replace(&self, path: &[u8]) -> FileIdentity {
        let entry = self.fs().entries.get(path).copied().expect("replace an existing path");
        self.delete(path);
        self.insert(path, entry.is_dir, entry.size)
    }

    pub fn identity_of(&self, path: &[u8]) -> Option<FileIdentity> {
        self.fs().entries.get(path).map(|e| e.identity)
    }

    /// Hides `path` from previews (not discovered yet).
    pub fn hide(&self, path: &[u8]) {
        let mut fs = self.fs();
        fs.hidden.insert(path.to_vec());
        fs.revision += 1;
    }

    pub fn reveal(&self, path: &[u8]) {
        let mut fs = self.fs();
        fs.hidden.remove(path);
        fs.revision += 1;
    }

    pub fn protect(&self, path: &[u8]) {
        self.fs().protected.push(path.to_vec());
    }

    pub fn exclude(&self, path: &[u8], reason: &str) {
        self.fs().exclusions.insert(path.to_vec(), reason.to_owned());
    }

    pub fn set_volumes(&self, volumes: Vec<Volume>) {
        self.fs().volumes = volumes;
    }

    pub fn fail_next_refresh(&self, message: &str) {
        self.fs().fail_refresh = Some(message.to_owned());
    }

    /// Fire-and-forget calls seen so far (`start /r`, `stop`, `monitor /r`, …).
    pub fn log(&self) -> Vec<String> {
        self.fs().log.clone()
    }

    pub fn revision(&self) -> u64 {
        self.fs().revision
    }

    /// Saves (publishes) a map of `root` and returns its snapshot.
    pub fn save(&self, root: &[u8]) -> SnapshotInfo {
        let mut fs = self.fs();
        fs.snapshot_ids += 1;
        let snapshot = SnapshotInfo {
            id: fs.snapshot_ids,
            root: Arc::from(root),
            root_identity: fs.entries.get(root).map(|e| e.identity),
            indexed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + fs.snapshot_ids),
            skipped_count: 0,
            issues: Arc::from([]),
            needs_boot_upgrade: false,
        };
        fs.saved.insert(root.to_vec(), snapshot.clone());
        snapshot
    }

    pub fn file_count(&self, root: &[u8]) -> u64 {
        let fs = self.fs();
        fs.entries.iter().filter(|(p, e)| !e.is_dir && is_within(p, root)).count() as u64
    }

    fn removed(fs: &Fs, path: &[u8], removals: &[ScanRemoval]) -> bool {
        removals
            .iter()
            .any(|r| is_within(path, &r.path) && fs.entries.get(&r.path[..]).is_some_and(|e| e.identity == r.identity))
    }

    fn visible(fs: &Fs, path: &[u8], preview: bool, removals: &[ScanRemoval]) -> bool {
        fs.entries.contains_key(path)
            && !(preview && fs.hidden.iter().any(|h| is_within(path, h)))
            && !Self::removed(fs, path, removals)
    }

    fn children<'a>(fs: &'a Fs, folder: &[u8]) -> impl Iterator<Item = (&'a Vec<u8>, &'a Entry)> + 'a {
        let prefix = if folder == b"/" { b"/".to_vec() } else { join(folder, b"") };
        let depth = prefix.iter().filter(|b| **b == b'/').count();
        fs.entries
            .range(prefix.clone()..)
            .take_while(move |(p, _)| p.starts_with(&prefix))
            .filter(move |(p, _)| p.len() > 1 && p.iter().filter(|b| **b == b'/').count() == depth)
    }

    fn meta(fs: &Fs, path: &[u8], preview: bool, removals: &[ScanRemoval]) -> NodeMeta {
        let entry = fs.entries[path];
        let (size, files) = if entry.is_dir {
            fs.entries
                .iter()
                .filter(|(p, e)| {
                    !e.is_dir && p.len() > path.len() && is_within(p, path) && Self::visible(fs, p, preview, removals)
                })
                .fold((0u64, 0u64), |(s, n), (_, e)| (s.saturating_add(e.size), n + 1))
        } else {
            (entry.size, 1)
        };
        NodeMeta {
            flags: NodeFlags {
                is_dir: entry.is_dir,
                is_symlink: entry.symlink,
                is_mount_point: false,
                accessible: entry.accessible,
            },
            size,
            allocated: size,
            raw_allocated: if entry.is_dir { 0 } else { size },
            file_count: files,
            identity: Some(entry.identity),
            state: Default::default(),
        }
    }

    fn tree(fs: &Fs, root: &[u8], folder: &[u8], preview: bool, removals: &[ScanRemoval]) -> FolderTree {
        let name = if file_name(folder).is_empty() { folder } else { file_name(folder) };
        let mut builder =
            FolderTreeBuilder::new(Arc::from(root), folder.to_vec(), name, Self::meta(fs, folder, preview, removals));
        if folder != root {
            let mut chain = Vec::new();
            let mut current = parent_or_root(folder);
            loop {
                chain.push(current.to_vec());
                if current == root || current == b"/" {
                    break;
                }
                current = parent_or_root(current);
            }
            for ancestor in chain.iter().rev() {
                let name = if file_name(ancestor).is_empty() { &ancestor[..] } else { file_name(ancestor) };
                builder.push_ancestor(name, Self::meta(fs, ancestor, preview, removals));
            }
        }
        let mut queue: Vec<(NodeIx, Vec<u8>, u32)> = vec![(FolderTree::FOLDER, folder.to_vec(), 0)];
        let mut descendants = 0;
        let mut next = 0;
        while next < queue.len() {
            let (ix, path, depth) = queue[next].clone();
            next += 1;
            let children: Vec<Vec<u8>> = Self::children(fs, &path)
                .filter(|(p, _)| Self::visible(fs, p, preview, removals))
                .map(|(p, _)| p.clone())
                .collect();
            for child in children {
                if depth > 0 && descendants >= VIEW_DESCENDANT_BUDGET {
                    break;
                }
                if depth > 0 {
                    descendants += 1;
                }
                let meta = Self::meta(fs, &child, preview, removals);
                let child_ix = builder.push_child(ix, file_name(&child), meta);
                if meta.flags.is_dir && depth + 1 < VIEW_DEPTH {
                    queue.push((child_ix, child, depth + 1));
                }
            }
        }
        builder.finish(fs.revision, false, if preview { TreeSource::Live } else { TreeSource::Saved })
    }
}

impl Backend for FakeBackend {
    fn resolve(&self, path: &[u8], force: bool, candidate: Option<&ResolveCandidate>, _: &Cancellation) -> Resolution {
        let fs = self.fs();
        if let Some(reason) = fs.exclusions.get(path) {
            return Resolution { root: path.to_vec(), exclusion: Some(reason.clone()), ..Resolution::default() };
        }
        let Some(target) = fs.entries.get(path).map(|e| e.identity) else {
            return Resolution {
                root: path.to_vec(),
                exclusion: Some(format!(
                    "“{}” couldn’t be opened because there is no such file.",
                    String::from_utf8_lossy(file_name(path))
                )),
                ..Resolution::default()
            };
        };
        if let Some(candidate) = candidate.filter(|_| !force) {
            let replaced = candidate
                .covering_removals
                .iter()
                .any(|r| fs.entries.get(&r.path[..]).is_some_and(|e| e.identity != r.identity));
            let root_ok = fs.entries.get(&candidate.root[..]).map(|e| e.identity) == Some(candidate.identity);
            if !replaced && root_ok && is_within(path, &candidate.root) {
                return Resolution {
                    root: candidate.root.to_vec(),
                    root_identity: Some(candidate.identity),
                    target_identity: Some(target),
                    used_candidate: true,
                    exclusion: None,
                };
            }
        }
        if !force {
            let covering = fs.saved.keys().filter(|root| is_within(path, root)).max_by_key(|root| root.len());
            if let Some(root) = covering {
                return Resolution {
                    root: root.clone(),
                    root_identity: fs.entries.get(root).map(|e| e.identity),
                    target_identity: Some(target),
                    used_candidate: false,
                    exclusion: None,
                };
            }
        }
        Resolution {
            root: path.to_vec(),
            root_identity: Some(target),
            target_identity: Some(target),
            ..Resolution::default()
        }
    }

    fn start_mapping(&self, request: &MappingRequest, _: Notifier) {
        let line = format!(
            "start {} force={} keep={}",
            String::from_utf8_lossy(&request.root),
            request.force,
            request.keep_map
        );
        let mut fs = self.fs();
        fs.sessions.insert(request.session, request.root.to_vec());
        fs.log.push(line);
    }

    fn stop_mapping(&self, _: SessionId) {
        self.fs().log.push("stop".into());
    }

    fn prioritize(&self, _: SessionId, path: &[u8]) {
        self.fs().log.push(format!("prioritize {}", String::from_utf8_lossy(path)));
    }

    fn view(&self, request: &ViewRequest, _: &Cancellation) -> Result<Option<FolderTree>, String> {
        let fs = self.fs();
        let preview = request.source == ViewSource::Preview;
        let fallback = request.fallback || !preview;
        let mut path: &[u8] = &request.path;
        loop {
            let ok = is_within(path, &request.root)
                && fs.entries.get(path).is_some_and(|e| e.is_dir)
                && Self::visible(&fs, path, preview, &request.removals);
            if ok {
                break;
            }
            if !fallback {
                return Ok(None);
            }
            if path == &request.root[..] || path == b"/" {
                return if preview { Ok(None) } else { Err("The indexed folder is unavailable.".into()) };
            }
            path = parent_or_root(path);
            if !is_within(path, &request.root) {
                path = &request.root;
            }
        }
        Ok(Some(Self::tree(&fs, &request.root, path, preview, &request.removals)))
    }

    fn discovered_file_count(&self, session: SessionId, _: &[ScanRemoval], _: &Cancellation) -> Result<u64, String> {
        let fs = self.fs();
        let root = fs.sessions.get(&session).cloned().unwrap_or_else(|| b"/".to_vec());
        let visible = |p: &[u8]| is_within(p, &root) && !fs.hidden.iter().any(|h| is_within(p, h));
        Ok(fs.entries.iter().filter(|(p, e)| !e.is_dir && visible(p)).count() as u64)
    }

    fn search(&self, request: &SearchRequest, _: &Cancellation) -> Result<SearchReply, String> {
        let fs = self.fs();
        let preview = matches!(request.source, crate::backend::SearchSource::Preview);
        let needle = fold_query(&request.query);
        let mut hits: Vec<Item> = fs
            .entries
            .keys()
            .filter(|p| p.len() > request.root.len() && is_within(p, &request.root))
            .filter(|p| Self::visible(&fs, p, preview, &request.removals))
            .filter(|p| fold(file_name(p)).contains(&needle))
            .map(|p| Item::new(p.clone(), Self::meta(&fs, p, preview, &request.removals)))
            .collect();
        hits.sort_by(|a, b| b.meta.size.cmp(&a.meta.size).then_with(|| a.path.cmp(&b.path)));
        let total = hits.len() as u64;
        hits.truncate(request.limit);
        Ok(SearchReply { hits, total, preview_revision: fs.revision, removals_changed: false })
    }

    fn identity(&self, path: &[u8]) -> Option<FileIdentity> {
        self.identity_of(path)
    }

    fn rejection_reason(&self, path: &[u8], root: &[u8]) -> Option<String> {
        if path == root {
            return Some("The mapped folder cannot be removed.".into());
        }
        if !is_within(path, root) {
            return Some("This item is outside the scanned folder.".into());
        }
        self.fs()
            .protected
            .iter()
            .any(|p| is_within(path, p))
            .then(|| "System and Library folders are protected.".to_owned())
    }

    fn remove(&self, request: &RemovalRequest) -> RemovalOutcome {
        let mut outcome = RemovalOutcome::default();
        let mut parents = BTreeSet::new();
        for item in &request.items {
            let name = item.display_name();
            if request.root_identity.is_none() || self.identity_of(&request.root) != request.root_identity {
                outcome
                    .failures
                    .push("The mapped folder changed. Refresh the map before removing any more items.".into());
                outcome.root_changed = true;
                break;
            }
            let parent = parent_or_root(&item.path).to_vec();
            if item.meta.identity.is_none() || self.identity_of(&item.path) != item.meta.identity {
                outcome
                    .failures
                    .push(format!("{name}: the item changed. Remove it from the collection and add it again."));
                parents.insert(parent);
                continue;
            }
            if let Some(reason) = self.rejection_reason(&item.path, &request.root) {
                outcome.failures.push(format!("{name}: {reason}"));
                continue;
            }
            parents.insert(parent);
            self.delete(&item.path);
            outcome.removed.push(Arc::clone(&item.path));
        }
        if parents.len() > request.parent_limit {
            outcome.parents_overflow = true;
        } else {
            outcome.changed_parents = parents.into_iter().collect();
        }
        outcome
    }

    fn refresh(&self, request: &RefreshRequest, _: &Cancellation) -> Result<SnapshotInfo, String> {
        if let Some(message) = self.fs().fail_refresh.take() {
            return Err(message);
        }
        Ok(self.save(&request.root))
    }

    fn monitor(&self, _: SessionId, snapshot: &SnapshotInfo, _: Notifier) {
        self.fs().log.push(format!("monitor {}", String::from_utf8_lossy(&snapshot.root)));
    }

    fn stop_monitor(&self, _: SessionId) {
        self.fs().log.push("stop monitor".into());
    }

    fn volumes(&self) -> Vec<Volume> {
        self.fs().volumes.clone()
    }

    fn directories(&self, paths: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let fs = self.fs();
        paths.iter().filter(|p| fs.entries.get(&p[..]).is_some_and(|e| e.is_dir)).cloned().collect()
    }
}

/// Drives an [`AppState`] against a [`FakeBackend`] with explicit time.
pub struct Harness {
    pub state: AppState,
    pub backend: Arc<FakeBackend>,
    pub now: Instant,
    /// Backend requests not yet performed, in issue order.
    pub requests: Vec<(RequestId, Request)>,
    pub computes: Vec<(RequestId, ComputeJob)>,
    pub timers: Vec<(Instant, Timer)>,
    pub cancelled: Vec<RequestId>,
    pub platform: Vec<PlatformAction>,
    pub persisted: Vec<Persist>,
    /// Mapping requests seen (fire-and-forget), newest last.
    pub mappings: Vec<MappingRequest>,
    /// Regions notified by the last step.
    pub last_notify: Regions,
    notifier: Notifier,
    inbox: Arc<Mutex<Vec<Event>>>,
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Harness").field("requests", &self.requests.len()).field("timers", &self.timers.len()).finish()
    }
}

impl Harness {
    pub fn new(config: Config) -> Self {
        let now = Instant::now();
        let inbox: Arc<Mutex<Vec<Event>>> = Arc::default();
        let sink = Arc::clone(&inbox);
        let notifier: Notifier = Arc::new(move |event| sink.lock().unwrap_or_else(|e| e.into_inner()).push(event));
        Self {
            state: AppState::new(config, now),
            backend: Arc::new(FakeBackend::new()),
            now,
            requests: Vec::new(),
            computes: Vec::new(),
            timers: Vec::new(),
            cancelled: Vec::new(),
            platform: Vec::new(),
            persisted: Vec::new(),
            mappings: Vec::new(),
            last_notify: Regions::NONE,
            notifier,
            inbox,
        }
    }

    pub fn absorb(&mut self, effects: Vec<Effect>) {
        self.last_notify = Regions::NONE;
        for effect in effects {
            match effect {
                Effect::Backend { id, request } => {
                    if let Request::StartMapping(mapping) = &request {
                        self.mappings.push(mapping.clone());
                    }
                    self.requests.push((id, request));
                }
                Effect::Cancel(id) => {
                    self.cancelled.push(id);
                    self.requests.retain(|(rid, _)| *rid != id);
                    self.computes.retain(|(rid, _)| *rid != id);
                }
                Effect::Compute { id, job } => self.computes.push((id, job)),
                Effect::Timer { after, timer } => self.timers.push((self.now + after, timer)),
                Effect::Platform(action) => self.platform.push(action),
                Effect::Persist(persist) => self.persisted.push(persist),
                Effect::Notify(regions) => self.last_notify = regions,
            }
        }
    }

    /// Handles one event and records its effects; returns the notified regions.
    pub fn send(&mut self, event: Event) -> Regions {
        let effects = self.state.handle(event, self.now);
        self.absorb(effects);
        self.last_notify
    }

    /// Removes the first pending request matching `pred` without performing it.
    pub fn take(&mut self, mut pred: impl FnMut(&Request) -> bool) -> Option<(RequestId, Request)> {
        let index = self.requests.iter().position(|(_, r)| pred(r))?;
        Some(self.requests.remove(index))
    }

    /// Performs a request against the fake backend and delivers its reply.
    pub fn deliver(&mut self, (id, request): (RequestId, Request)) -> Regions {
        let cancel = Cancellation::new();
        match perform(&*self.backend, id, request, &cancel, &self.notifier) {
            Some(event) => self.send(event),
            None => Regions::NONE,
        }
    }

    /// Performs a request now but returns its reply instead of delivering it (to deliver late).
    pub fn perform_held(&self, (id, request): (RequestId, Request)) -> Option<Event> {
        perform(&*self.backend, id, request, &Cancellation::new(), &self.notifier)
    }

    /// Performs the first pending request matching `pred`. Returns whether one was found.
    pub fn run_one(&mut self, pred: impl FnMut(&Request) -> bool) -> bool {
        match self.take(pred) {
            Some(request) => {
                self.deliver(request);
                true
            }
            None => false,
        }
    }

    /// Performs pending requests and compute jobs in issue order until none are left.
    pub fn run(&mut self) {
        for _ in 0..10_000 {
            if !self.requests.is_empty() {
                let request = self.requests.remove(0);
                self.deliver(request);
            } else if !self.computes.is_empty() {
                let (id, job) = self.computes.remove(0);
                self.send(Event::Computed { id, result: job.run() });
            } else {
                let events: Vec<Event> = std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()));
                if events.is_empty() {
                    return;
                }
                for event in events {
                    self.send(event);
                }
            }
        }
        panic!("requests did not settle");
    }

    /// Moves time forward and fires due timers in deadline order (without running requests).
    pub fn advance(&mut self, by: Duration) {
        let until = self.now + by;
        loop {
            self.timers.sort_by_key(|(at, _)| *at);
            let Some(&(at, timer)) = self.timers.first().filter(|(at, _)| *at <= until) else { break };
            self.timers.remove(0);
            self.now = self.now.max(at);
            self.send(Event::Timer(timer));
        }
        self.now = until;
    }

    /// Runs requests and fires timers (advancing time up to `limit`) until quiescent.
    pub fn settle(&mut self) {
        for _ in 0..1_000 {
            self.run();
            self.timers.sort_by_key(|(at, _)| *at);
            let Some(&(at, _)) = self.timers.first() else { return };
            let wait = at.saturating_duration_since(self.now);
            self.advance(wait);
        }
        panic!("did not settle");
    }

    pub fn has_request(&self, mut pred: impl FnMut(&Request) -> bool) -> bool {
        self.requests.iter().any(|(_, r)| pred(r))
    }

    /// The newest mapping session started for `root`.
    pub fn session_for(&self, root: &[u8]) -> SessionId {
        self.mappings.iter().rev().find(|m| *m.root == *root).map(|m| m.session).expect("a mapping session for root")
    }

    pub fn mapping(&mut self, root: &[u8], event: MappingEvent) -> Regions {
        let session = self.session_for(root);
        self.send(Event::Mapping { session, event })
    }

    /// The engine finished mapping `root`: save it and publish.
    pub fn publish(&mut self, root: &[u8]) -> SnapshotInfo {
        let snapshot = self.backend.save(root);
        let count = self.backend.file_count(root);
        self.mapping(
            root,
            MappingEvent::Published { snapshot: snapshot.clone(), reused: false, root_file_count: count },
        );
        snapshot
    }

    /// The live preview of `root` changed.
    pub fn preview(&mut self, root: &[u8]) -> Regions {
        let revision = self.backend.revision();
        self.mapping(root, MappingEvent::PreviewChanged { revision, search_ready: true })
    }

    pub fn open(&mut self, path: &[u8]) {
        self.send(Event::OpenLocation { path: path.to_vec(), force: false });
        self.run();
    }

    /// Opens `root`, lets the preview show, publishes and settles.
    pub fn open_mapped(&mut self, root: &[u8]) -> SnapshotInfo {
        self.open(root);
        self.preview(root);
        self.run();
        let snapshot = self.publish(root);
        self.run();
        snapshot
    }

    pub fn path(path: &str) -> Arc<[u8]> {
        Arc::from(path.as_bytes())
    }

    /// Row names in display order.
    pub fn rows(&self) -> Vec<String> {
        (0..self.state.row_count())
            .filter_map(|i| self.state.row(i))
            .map(|row| String::from_utf8_lossy(row.name()).into_owned())
            .collect()
    }

    pub fn collection(&self) -> Vec<String> {
        self.state
            .collection()
            .map(|c| c.items().iter().map(|i| String::from_utf8_lossy(&i.path).into_owned()).collect())
            .unwrap_or_default()
    }
}
