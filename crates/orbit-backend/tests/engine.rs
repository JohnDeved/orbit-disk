//! End to end: `AppState` + `EngineBackend<Native>` on real temporary trees. The test performs
//! every backend request synchronously, delivers engine notifications from an inbox and fires
//! timers on an explicit clock. It only waits for real engine events (bounded by `WAIT`).
//!
//! Swift rows of `orbit-app-state/TRIAGE-COVERAGE.md` owned by the backend are named on each
//! test.
#![cfg(unix)]

use std::collections::VecDeque;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_app_state::event::ComputeJob;
use orbit_app_state::{AppState, Config, Effect, Event, MappingEvent, Notifier, Request, RequestId, Timer, perform};
use orbit_backend::{BackendOptions, EngineBackend};
use orbit_core::discovery::DiscoveryHooks;
use orbit_core::paths::is_within;
use orbit_core::recovery::SavedRecovery;
use orbit_core::saved::SavedMaps;
use orbit_core::store::IndexPaths;
use orbit_core::view::FolderTree;
use orbit_native::Native;
use orbit_platform::Cancellation;
use parking_lot::{Condvar, Mutex};

const WAIT: Duration = Duration::from_secs(15);

fn bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

fn write(path: &Path, size: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; size]).unwrap();
}

/// A temporary tree. The prefix has no leading dot: hidden folders are protected from removal.
struct Fixture {
    _dir: tempfile::TempDir,
    base: PathBuf,
    home: PathBuf,
    paths: IndexPaths,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("orbit-backend-").tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let paths = IndexPaths::under(&base.join("engine"));
        Self { _dir: dir, base, home, paths }
    }

    /// `name/` with `big/{a,b}.bin`, `docs/{x,y}.txt`, `docs/deep/z.txt`, `slow/s1.dat`,
    /// `slow/inner/s2.dat`.
    fn tree(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        write(&root.join("big/a.bin"), 4000);
        write(&root.join("big/b.bin"), 2000);
        write(&root.join("docs/x.txt"), 300);
        write(&root.join("docs/y.txt"), 200);
        write(&root.join("docs/deep/z.txt"), 50);
        write(&root.join("slow/s1.dat"), 100);
        write(&root.join("slow/inner/s2.dat"), 100);
        root
    }

    fn options(&self, hooks: Option<Arc<Gate>>) -> BackendOptions {
        let mut options = BackendOptions::new(self.paths.clone(), self.home.clone());
        options.workers = 2;
        options.monitor_changes = false;
        options.hooks = hooks.map(|gate| gate as Arc<dyn DiscoveryHooks>);
        options
    }

    fn maps(&self) -> SavedMaps<Native> {
        #[allow(clippy::default_constructed_unit_structs)] // `Native` is a unit struct on macOS.
        SavedMaps::new(Arc::new(Native::default()), self.paths.clone())
    }
}

/// Holds discovery before folders inside a blocked path and records every listed folder.
#[derive(Default)]
struct Gate {
    blocked: Mutex<Option<Vec<u8>>>,
    released: Condvar,
    listed: Mutex<Vec<Vec<u8>>>,
}

impl Gate {
    fn block(&self, path: &Path) {
        *self.blocked.lock() = Some(bytes(path));
    }

    fn release(&self) {
        *self.blocked.lock() = None;
        self.released.notify_all();
    }

    fn listed_within(&self, root: &Path) -> usize {
        let root = bytes(root);
        self.listed.lock().iter().filter(|p| is_within(p, &root)).count()
    }
}

impl DiscoveryHooks for Gate {
    fn before_directory(&self, path: &[u8], _ticket: u64) {
        self.listed.lock().push(path.to_vec());
        let mut blocked = self.blocked.lock();
        while blocked.as_ref().is_some_and(|b| is_within(path, b)) {
            self.released.wait(&mut blocked);
        }
    }
}

#[derive(Default)]
struct Inbox {
    events: Mutex<Vec<Event>>,
    arrived: Condvar,
}

/// The host loop of the app, run by the test.
struct App {
    state: AppState,
    backend: Arc<EngineBackend<Native>>,
    now: Instant,
    requests: VecDeque<(RequestId, Request)>,
    computes: VecDeque<(RequestId, ComputeJob)>,
    timers: Vec<(Instant, Timer)>,
    inbox: Arc<Inbox>,
    notifier: Notifier,
    /// Roots of every `StartMapping` the state issued.
    started: Vec<Vec<u8>>,
}

impl App {
    fn new(fixture: &Fixture, hooks: Option<Arc<Gate>>) -> Self {
        Self::with_options(fixture, fixture.options(hooks))
    }

    fn with_options(fixture: &Fixture, options: BackendOptions) -> Self {
        #[allow(clippy::default_constructed_unit_structs)]
        let backend = Arc::new(EngineBackend::with_options(Arc::new(Native::default()), options));
        let inbox = Arc::new(Inbox::default());
        let sink = Arc::clone(&inbox);
        let notifier: Notifier = Arc::new(move |event| {
            sink.events.lock().push(event);
            sink.arrived.notify_all();
        });
        let now = Instant::now();
        let mut config = Config::new(bytes(&fixture.home));
        config.id_seed = 11;
        let mut app = Self {
            state: AppState::new(config, now),
            backend,
            now,
            requests: VecDeque::new(),
            computes: VecDeque::new(),
            timers: Vec::new(),
            inbox,
            notifier,
            started: Vec::new(),
        };
        let effects = app.state.launch(None, now);
        app.absorb(effects);
        app
    }

    fn absorb(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Backend { id, request } => {
                    if let Request::StartMapping(mapping) = &request {
                        self.started.push(mapping.root.to_vec());
                    }
                    self.requests.push_back((id, request));
                }
                Effect::Cancel(id) => {
                    self.requests.retain(|(rid, _)| *rid != id);
                    self.computes.retain(|(rid, _)| *rid != id);
                }
                Effect::Compute { id, job } => self.computes.push_back((id, job)),
                Effect::Timer { after, timer } => self.timers.push((self.now + after, timer)),
                Effect::Platform(_) | Effect::Persist(_) | Effect::Notify(_) => {}
            }
        }
    }

    fn send(&mut self, event: Event) {
        let effects = self.state.handle(event, self.now);
        self.absorb(effects);
    }

    /// Performs requests, compute jobs and delivered notifications until none is left.
    fn pump(&mut self) {
        loop {
            if let Some((id, request)) = self.requests.pop_front() {
                if let Some(event) = perform(&*self.backend, id, request, &Cancellation::new(), &self.notifier) {
                    self.send(event);
                }
            } else if let Some((id, job)) = self.computes.pop_front() {
                self.send(Event::Computed { id, result: job.run() });
            } else {
                let events = std::mem::take(&mut *self.inbox.events.lock());
                if events.is_empty() {
                    return;
                }
                for event in events {
                    self.send(event);
                }
            }
        }
    }

    /// Runs the loop until `done`: fires due timers on the explicit clock when idle, otherwise
    /// waits for the next engine notification.
    fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}; status {:?}", self.state.status());
            self.timers.sort_by_key(|(at, _)| *at);
            if !self.timers.is_empty() {
                let (at, timer) = self.timers.remove(0);
                self.now = self.now.max(at);
                self.send(Event::Timer(timer));
                continue;
            }
            let mut events = self.inbox.events.lock();
            if events.is_empty() {
                self.inbox.arrived.wait_for(&mut events, Duration::from_millis(50));
            }
        }
    }

    fn open(&mut self, path: &Path) {
        self.send(Event::OpenLocation { path: bytes(path), force: false });
    }

    fn rows(&self) -> Vec<String> {
        (0..self.state.row_count())
            .filter_map(|i| self.state.row(i))
            .map(|row| String::from_utf8_lossy(row.name()).into_owned())
            .collect()
    }

    fn current(&self) -> Option<PathBuf> {
        self.state.current_path().map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p)))
    }

    fn published(&self, root: &Path) -> bool {
        self.state.snapshot().is_some_and(|s| *s.root == *bytes(root))
            && !self.state.is_partial()
            && !self.state.is_scanning()
            && self.state.folder().is_some()
    }

    fn shows(&self, folder: &Path, rows: &[&str]) -> bool {
        self.current().as_deref() == Some(folder) && self.rows() == rows
    }

    fn collection(&self) -> Vec<Vec<u8>> {
        self.state.collection().map(|c| c.items().iter().map(|i| i.path.to_vec()).collect()).unwrap_or_default()
    }

    fn delete_collection_permanently(&mut self) {
        self.send(Event::BeginReview);
        let review = self.state.review_token().expect("review sheet");
        self.send(Event::ConfirmPermanentDelete { review });
        self.until("removal", |a| !a.state.removal_active());
    }
}

/// Rows of the saved map folder at `path` as (name, files, size).
fn saved_rows(app: &App, path: &Path) -> Vec<(String, u64, u64)> {
    let snapshot = app.backend.core_snapshot(app.state.snapshot().unwrap().id).unwrap();
    let tree = app.fixture_maps().view(&snapshot, &bytes(path), &[], None).unwrap();
    let mut rows: Vec<(String, u64, u64)> = tree
        .children(FolderTree::FOLDER)
        .iter()
        .map(|&child| {
            let meta = tree.node(child).meta;
            (String::from_utf8_lossy(tree.name(child)).into_owned(), meta.file_count, meta.size)
        })
        .collect();
    rows.sort();
    rows
}

impl App {
    fn fixture_maps(&self) -> SavedMaps<Native> {
        #[allow(clippy::default_constructed_unit_structs)]
        SavedMaps::new(Arc::new(Native::default()), self.backend.options().paths.clone())
    }
}

/// Open a location: the partial map is browsable while discovery runs, then the saved map takes
/// over at the same folder; search, collect and permanent delete; the removal is reconciled into
/// the saved map by the refresh of its parent and the journal has nothing left to recover.
#[test]
fn map_browse_search_collect_delete_and_reconcile() {
    let fx = Fixture::new();
    let root = fx.tree("root");
    let gate = Arc::new(Gate::default());
    gate.block(&root.join("slow"));
    let mut app = App::new(&fx, Some(Arc::clone(&gate)));

    app.open(&root);
    app.until("the partial map", |a| a.state.is_partial() && a.rows().len() == 3);
    assert!(app.state.snapshot().is_none(), "nothing is published while discovery is held");
    app.send(Event::Open { path: Arc::from(bytes(&root.join("docs"))) });
    app.until("docs in the partial map", |a| a.shows(&root.join("docs"), &["x.txt", "y.txt", "deep"]));
    assert!(app.state.is_partial());

    gate.release();
    app.until("the saved map", |a| a.published(&root));
    assert!(app.shows(&root.join("docs"), &["x.txt", "y.txt", "deep"]), "the folder survives publication");
    assert_eq!(
        saved_rows(&app, &root),
        [("big".into(), 2, 6000), ("docs".into(), 3, 550), ("slow".into(), 2, 200)],
        "the saved map matches the disk"
    );

    app.send(Event::QueryChanged("s2".into()));
    app.until("search hits", |a| a.rows() == ["s2.dat"]);
    app.send(Event::QueryChanged(String::new()));
    app.until("the folder again", |a| a.rows() == ["x.txt", "y.txt", "deep"]);

    let x = root.join("docs/x.txt");
    app.send(Event::Collect { paths: vec![Arc::from(bytes(&x))] });
    app.until("the collected item", |a| a.collection() == [bytes(&x)]);
    let before = app.state.snapshot().unwrap().id;
    app.delete_collection_permanently();
    assert!(!x.exists(), "deleted from disk");
    assert_eq!(app.state.error(), None);
    assert!(app.state.status().is_some_and(|s| s.starts_with("Deleted 1 item permanently.")));
    assert!(app.collection().is_empty());

    app.until("the reconciled map", |a| {
        a.state.snapshot().is_some_and(|s| s.id != before) && !a.state.is_refreshing() && a.rows() == ["y.txt", "deep"]
    });
    assert_eq!(saved_rows(&app, &root.join("docs")), [("deep".into(), 1, 50), ("y.txt".into(), 1, 200)]);
    let snapshot = app.backend.core_snapshot(app.state.snapshot().unwrap().id).unwrap();
    let (recovery, published) = fx.maps().recover(&snapshot, &Cancellation::new()).unwrap();
    assert!(matches!(recovery, SavedRecovery::UpToDate(_)), "journal completed: {recovery:?}");
    assert!(published.is_none());
}

/// Swift OrbitResumableChecks:92 (reopen after stage publication reflects the updated snapshot;
/// an explicit rescan keeps it) and :282 (mapping excludes its own index and stage).
/// Changed folders are refreshed into the saved map; a new process reopens the updated map
/// without listing a single folder.
#[test]
fn refresh_converges_and_reopening_reuses_the_saved_map_without_rescanning() {
    let mut fx = Fixture::new();
    let root = fx.tree("root");
    // The engine's own folders live inside the mapped root.
    fx.paths = IndexPaths::under(&root.join("engine"));
    let gate = Arc::new(Gate::default());
    let mut app = App::new(&fx, Some(Arc::clone(&gate)));
    app.open(&root);
    app.until("the saved map", |a| a.published(&root));
    let engine = saved_rows(&app, &root).into_iter().find(|(name, ..)| name == "engine");
    assert_eq!(engine.map(|(_, files, _)| files), Some(0), "stage and index are not mapped");

    write(&root.join("docs/new.txt"), 700);
    std::fs::remove_file(root.join("big/b.bin")).unwrap();
    write(&root.join("big/a.bin"), 5000);
    let session = app.state.active_session_id().unwrap();
    let before = app.state.snapshot().unwrap().id;
    let directories = vec![bytes(&root.join("docs")), bytes(&root.join("big"))];
    app.send(Event::Mapping {
        session,
        event: MappingEvent::Changes { directories, subtrees: Vec::new(), full_refresh: false },
    });
    app.until("the refreshed map", |a| a.state.snapshot().is_some_and(|s| s.id != before) && !a.state.is_refreshing());
    let refreshed =
        [("big".into(), 1, 5000), ("docs".into(), 4, 1250), ("engine".into(), 0, 0), ("slow".into(), 2, 200)];
    assert_eq!(saved_rows(&app, &root), refreshed);
    drop(app);

    let gate = Arc::new(Gate::default());
    let mut app = App::new(&fx, Some(Arc::clone(&gate)));
    app.open(&root);
    app.until("the reopened map", |a| a.published(&root));
    assert_eq!(gate.listed_within(&root), 0, "the saved map is reused without listing folders");
    assert_eq!(saved_rows(&app, &root), refreshed);
    assert_eq!(app.rows(), ["big", "docs", "slow", "engine"]);

    app.send(Event::RescanEverything);
    app.until("the rescanned map", |a| a.published(&root) && !a.state.is_refreshing());
    assert!(gate.listed_within(&root) > 0, "an explicit rescan lists the folders again");
    assert_eq!(saved_rows(&app, &root), refreshed);
}

/// A location in cloud storage is refused with the policy message and never mapped.
#[test]
fn cloud_storage_is_refused_with_the_policy_message() {
    let fx = Fixture::new();
    let drive = fx.home.join("Library/CloudStorage/Drive");
    write(&drive.join("file.txt"), 10);
    let mut app = App::new(&fx, None);
    app.open(&drive);
    app.until("the exclusion", |a| a.state.error().is_some());
    assert_eq!(app.state.error(), Some(orbit_core::policy::CLOUD_EXCLUDED));
    assert!(app.started.is_empty(), "nothing is mapped");
}

/// A second root maps while the first keeps mapping in the background; returning to the first
/// shows its finished map without mapping it again.
#[test]
fn a_second_root_maps_while_the_first_continues() {
    let fx = Fixture::new();
    let first = fx.tree("first");
    let second = fx.tree("second");
    let gate = Arc::new(Gate::default());
    gate.block(&first.join("slow"));
    let mut app = App::new(&fx, Some(Arc::clone(&gate)));
    app.open(&first);
    app.until("the first partial map", |a| a.state.is_partial() && a.rows().len() == 3);

    app.open(&second);
    app.until("the second saved map", |a| a.published(&second));
    gate.release();
    let listed = gate.listed_within(&first);
    // The first session publishes in the background (no request involved).
    let maps = fx.maps();
    let deadline = Instant::now() + WAIT;
    while maps.snapshot(&bytes(&first)).is_none() {
        assert!(Instant::now() < deadline, "the first mapping did not finish");
        app.until("idle", |_| true);
        std::thread::sleep(Duration::from_millis(20));
    }

    app.open(&first);
    app.until("the first saved map", |a| a.published(&first) && a.rows().len() == 3);
    assert_eq!(app.started.iter().filter(|r| **r == bytes(&first)).count(), 1, "not mapped again");
    assert!(gate.listed_within(&first) >= listed);
    assert_eq!(saved_rows(&app, &first), [("big".into(), 2, 6000), ("docs".into(), 3, 550), ("slow".into(), 2, 200)]);
}

/// Swift OrbitSessionChecks:244 (a stopped mapping releases its stage so it can reopen) and
/// OrbitResumableChecks:70 (the partial map resumes with the collection intact).
#[test]
fn a_stopped_mapping_resumes_its_stage_with_the_collection() {
    let fx = Fixture::new();
    let root = fx.tree("root");
    let gate = Arc::new(Gate::default());
    gate.block(&root.join("slow"));
    let mut app = App::new(&fx, Some(Arc::clone(&gate)));
    app.open(&root);
    app.until("the partial map", |a| a.state.is_partial() && a.rows().len() == 3);
    let y = root.join("docs/y.txt");
    app.send(Event::Open { path: Arc::from(bytes(&root.join("docs"))) });
    app.until("docs", |a| a.shows(&root.join("docs"), &["x.txt", "y.txt", "deep"]));
    app.send(Event::Collect { paths: vec![Arc::from(bytes(&y))] });
    app.until("the collected item", |a| a.collection() == [bytes(&y)]);

    app.send(Event::Pause);
    app.until("paused", |a| a.state.updates_paused());
    gate.release();
    assert!(app.state.is_partial(), "the partial map stays on screen");

    app.send(Event::Resume);
    app.until("the resumed map", |a| a.published(&root));
    assert_eq!(app.state.error(), None, "the stage was released and reopened");
    assert_eq!(app.collection(), [bytes(&y)]);
    assert_eq!(saved_rows(&app, &root), [("big".into(), 2, 6000), ("docs".into(), 3, 550), ("slow".into(), 2, 200)]);
}

/// Swift OrbitDurableRemovalChecks:461 (overlapping nested saved maps both reconcile a removal
/// after the process ends without refreshing either).
#[test]
fn nested_saved_maps_both_reconcile_a_removal_after_reopening() {
    let fx = Fixture::new();
    let root = fx.tree("root");
    let docs = root.join("docs");
    let mut app = App::new(&fx, None);
    app.open(&docs);
    app.until("the nested map", |a| a.published(&docs));
    app.send(Event::OpenLocation { path: bytes(&root), force: true });
    app.until("the parent map", |a| a.published(&root));
    app.send(Event::Open { path: Arc::from(bytes(&docs)) });
    app.until("docs", |a| a.shows(&docs, &["x.txt", "y.txt", "deep"]));
    let x = docs.join("x.txt");
    app.send(Event::Collect { paths: vec![Arc::from(bytes(&x))] });
    app.until("the collected item", |a| a.collection() == [bytes(&x)]);
    app.delete_collection_permanently();
    assert!(!x.exists());
    // The process ends before any refresh ran.
    drop(app);

    for folder in [&docs, &root] {
        let mut app = App::new(&fx, None);
        app.open(folder);
        app.until("the reopened map", |a| a.published(folder));
        assert!(app.started.iter().all(|r| *r == bytes(folder)));
        let rows = saved_rows(&app, &docs);
        assert_eq!(rows, [("deep".into(), 1, 50), ("y.txt".into(), 1, 200)], "{}", folder.display());
    }
}

/// The platform change monitor of the displayed saved map drives the refresh: a file written
/// after publication appears without any request from the test.
#[test]
fn monitored_changes_refresh_the_saved_map() {
    let fx = Fixture::new();
    let root = fx.tree("root");
    let mut options = fx.options(None);
    options.monitor_changes = true;
    let mut app = App::with_options(&fx, options);
    app.open(&root);
    app.until("the saved map", |a| a.published(&root));
    app.send(Event::Open { path: Arc::from(bytes(&root.join("docs"))) });
    app.until("docs", |a| a.shows(&root.join("docs"), &["x.txt", "y.txt", "deep"]));
    write(&root.join("docs/later.txt"), 1000);
    app.until("the monitored change", |a| a.rows() == ["later.txt", "x.txt", "y.txt", "deep"]);
    assert_eq!(app.state.error(), None);
    assert!(!app.state.updates_paused(), "automatic updates are available");
}

/// While mapping, progress names the folder being read (the opening view and footer show it);
/// a folder that cannot be read reaches the published snapshot as a skipped path.
#[test]
fn progress_names_the_folder_being_read_and_skipped_paths_reach_the_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let root = fx.tree("root");
    let locked = root.join("docs/locked");
    write(&locked.join("secret.txt"), 10);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = std::fs::read_dir(&locked).is_err(); // false when running as root
    let gate = Arc::new(Gate::default());
    gate.block(&root.join("slow"));
    let mut options = fx.options(Some(Arc::clone(&gate)));
    options.workers = 1;
    options.current_path_interval = Duration::ZERO;
    let mut app = App::with_options(&fx, options);

    app.open(&root);
    let slow = bytes(&root.join("slow"));
    app.until("the held folder as the folder being read", |a| a.state.scan_path() == slow.as_slice());
    assert!(app.state.progress_count() > 0);
    gate.release();
    app.until("the saved map", |a| a.published(&root));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    let snapshot = app.state.snapshot().expect("a published map").clone();
    if unreadable {
        let locked = locked.to_string_lossy().into_owned();
        assert!(snapshot.skipped_count >= 1);
        assert!(snapshot.issues.iter().any(|issue| issue.starts_with(&locked)), "{:?}", snapshot.issues);
    }
}

/// A read of a saved map that a refresh replaced in the meantime (its reply not yet handled by
/// the app) answers from the replacement instead of failing with "the saved map changed".
#[test]
fn reads_of_a_replaced_saved_map_answer_from_its_replacement() {
    use orbit_app_state::Backend as _;
    use orbit_app_state::backend::{SearchRequest, SearchSource, ViewRequest, ViewSource};
    let fx = Fixture::new();
    let root = fx.tree("root");
    let mut app = App::new(&fx, None);
    app.open(&root);
    app.until("the saved map", |a| a.published(&root));
    let stale = app.state.snapshot().unwrap().clone();
    let session = app.state.active_session_id().unwrap();
    // A refresh publishes a new saved map behind the app's back.
    write(&root.join("docs/fresh.txt"), 900);
    let core = app.backend.core_snapshot(stale.id).unwrap();
    let maps = app.fixture_maps();
    let listings = [bytes(&root.join("docs"))];
    let replaced = maps.refresh(&core, &listings, &[], None, &Cancellation::new()).unwrap();
    assert_ne!(replaced.id, core.id);

    let view = app
        .backend
        .view(
            &ViewRequest {
                session,
                root: Arc::from(bytes(&root)),
                source: ViewSource::Saved(stale.id),
                path: bytes(&root.join("docs")),
                removals: Arc::from([]),
                fallback: true,
            },
            &Cancellation::new(),
        )
        .expect("the stale read answers")
        .expect("a folder");
    let names: Vec<String> = view
        .children(FolderTree::FOLDER)
        .iter()
        .map(|&child| String::from_utf8_lossy(view.name(child)).into_owned())
        .collect();
    assert!(names.contains(&"fresh.txt".to_owned()), "{names:?}");
    let search = app.backend.search(
        &SearchRequest {
            session,
            root: Arc::from(bytes(&root)),
            source: SearchSource::Saved { snapshot: stale.id, stable_order: false },
            query: "fresh".into(),
            removals: Arc::from([]),
            limit: 10,
        },
        &Cancellation::new(),
    );
    assert!(search.is_ok(), "{search:?}");
}

/// A quick Refresh retries folders the map could not read: once readable (access granted, which
/// no change history reports), their contents appear without any reported change.
#[test]
fn a_quick_refresh_maps_folders_that_became_readable() {
    use orbit_app_state::Backend as _;
    use orbit_app_state::backend::RefreshRequest;
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let root = fx.tree("root");
    let locked = root.join("locked");
    write(&locked.join("secret.bin"), 4_321);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return; // Privileges that ignore permissions.
    }
    let mut app = App::new(&fx, None);
    app.open(&root);
    app.until("the saved map", |a| a.published(&root));
    let before = app.state.snapshot().unwrap().clone();
    let session = app.state.active_session_id().unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let request = |retry_unreadable| RefreshRequest {
        session,
        root: Arc::from(bytes(&root)),
        snapshot: before.id,
        paths: Vec::new(),
        subtrees: Vec::new(),
        retry_unreadable,
    };
    let unchanged = app.backend.refresh(&request(false), &Cancellation::new()).unwrap();
    assert_eq!(unchanged.id, before.id, "nothing reported, nothing retried");
    let retried = app.backend.refresh(&request(true), &Cancellation::new()).unwrap();
    assert_ne!(retried.id, before.id, "the readable folder was mapped");
    let maps = app.fixture_maps();
    let saved = maps.snapshot(&bytes(&root)).unwrap();
    let tree = maps.view(&saved, &bytes(&locked), &[], None).unwrap();
    let meta = tree.node(FolderTree::FOLDER).meta;
    assert_eq!((meta.file_count, meta.size), (1, 4_321));
}
