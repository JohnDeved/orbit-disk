//! Host wiring: effects round-trip through the executors, cancellation, region-only re-rendering
//! and the quit guard. Domain behavior is tested in `orbit-app-state`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use gpui::{AppContext, TestAppContext};
use orbit_app_state::backend::{
    MappingRequest, RefreshRequest, RemovalOutcome, RemovalRequest, Request, Resolution, ResolveCandidate, SearchReply,
    SearchRequest, SearchSource, ViewRequest,
};
use orbit_app_state::event::Effect;
use orbit_app_state::model::ProgressSnapshot;
use orbit_app_state::testing::{FakeBackend, Harness};
use orbit_app_state::{
    AppState, Backend, Config, Event, MappingEvent, Notifier, Regions, RequestId, SessionId, SnapshotInfo, Volume,
};
use orbit_core::types::ScanRemoval;
use orbit_core::view::FolderTree;
use orbit_platform::{Cancellation, FileIdentity};

use crate::frame_stats;
use crate::host::{AppHost, Host, HostOptions};
use crate::lifecycle;
use crate::main_window::MainWindow;
use crate::settings::{Settings, SettingsStore};

fn host_with(state: AppState, backend: Arc<dyn Backend>, cx: &mut TestAppContext) -> Host {
    cx.update(|cx| {
        let entity =
            cx.new(|cx| AppHost::with_state(state, backend, SettingsStore::in_memory(Settings::default()), cx));
        Host::from_entity(entity, cx)
    })
}

/// `/r` mapped and published, settled.
fn mapped() -> Harness {
    let mut h = Harness::new(Config::new(b"/home".to_vec()));
    h.backend.add_file(b"/r/big/a.bin", 100);
    h.backend.add_file(b"/r/docs/x.txt", 30);
    h.backend.add_file(b"/r/file1", 10);
    h.open_mapped(b"/r");
    h.settle();
    h
}

#[gpui::test]
fn backend_effects_round_trip_into_the_state(cx: &mut TestAppContext) {
    let backend = Arc::new(FakeBackend::new());
    backend.add_file(b"/r/big/a.bin", 100);
    backend.add_file(b"/r/file1", 10);
    let host = cx.update(|cx| {
        Host::new(
            HostOptions {
                config: Config::new(b"/home".to_vec()),
                backend: backend.clone(),
                settings: SettingsStore::in_memory(Settings::default()),
            },
            cx,
        )
    });
    // OpenLocation → Resolve (background) → reply handled → StartMapping (background).
    cx.update(|cx| host.send(Event::OpenLocation { path: b"/r".to_vec(), force: false }, cx));
    cx.run_until_parked();
    assert!(backend.log().iter().any(|line| line.starts_with("start /r")), "{:?}", backend.log());
    // A preview notification → View request (background, prepared there) → folder installed.
    let session = cx.read(|cx| host.state(cx).active_session_id()).expect("a session");
    let revision = backend.revision();
    cx.update(|cx| {
        host.send(Event::Mapping { session, event: MappingEvent::PreviewChanged { revision, search_ready: true } }, cx)
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let state = host.state(cx);
        assert_eq!(state.current_path(), Some(&b"/r"[..]));
        assert_eq!(state.row_count(), 2);
        assert_eq!(host.read(cx).in_flight(), 0);
    });
}

/// Records whether `search` saw its cancellation; everything else is the fake backend.
struct Probe {
    inner: FakeBackend,
    /// 0: not called, 1: called live, 2: called cancelled.
    search: AtomicU8,
}

impl Backend for Probe {
    fn resolve(
        &self,
        path: &[u8],
        force: bool,
        candidate: Option<&ResolveCandidate>,
        cancel: &Cancellation,
    ) -> Resolution {
        self.inner.resolve(path, force, candidate, cancel)
    }
    fn start_mapping(&self, request: &MappingRequest, notify: Notifier) {
        self.inner.start_mapping(request, notify)
    }
    fn stop_mapping(&self, session: SessionId) {
        self.inner.stop_mapping(session)
    }
    fn prioritize(&self, session: SessionId, path: &[u8]) {
        self.inner.prioritize(session, path)
    }
    fn view(&self, request: &ViewRequest, cancel: &Cancellation) -> Result<Option<FolderTree>, String> {
        self.inner.view(request, cancel)
    }
    fn discovered_file_count(&self, s: SessionId, r: &[ScanRemoval], c: &Cancellation) -> Result<u64, String> {
        self.inner.discovered_file_count(s, r, c)
    }
    fn search(&self, request: &SearchRequest, cancel: &Cancellation) -> Result<SearchReply, String> {
        self.search.store(if cancel.is_cancelled() { 2 } else { 1 }, Ordering::SeqCst);
        self.inner.search(request, cancel)
    }
    fn identity(&self, path: &[u8]) -> Option<FileIdentity> {
        self.inner.identity(path)
    }
    fn rejection_reason(&self, path: &[u8], root: &[u8]) -> Option<String> {
        self.inner.rejection_reason(path, root)
    }
    fn remove(&self, request: &RemovalRequest) -> RemovalOutcome {
        self.inner.remove(request)
    }
    fn refresh(&self, request: &RefreshRequest, cancel: &Cancellation) -> Result<SnapshotInfo, String> {
        self.inner.refresh(request, cancel)
    }
    fn monitor(&self, session: SessionId, snapshot: &SnapshotInfo, notify: Notifier) {
        self.inner.monitor(session, snapshot, notify)
    }
    fn stop_monitor(&self, session: SessionId) {
        self.inner.stop_monitor(session)
    }
    fn volumes(&self) -> Vec<Volume> {
        self.inner.volumes()
    }
    fn directories(&self, paths: &[Vec<u8>]) -> Vec<Vec<u8>> {
        self.inner.directories(paths)
    }
}

#[gpui::test]
fn cancel_effect_cancels_the_request_and_drops_its_reply(cx: &mut TestAppContext) {
    let probe = Arc::new(Probe { inner: FakeBackend::new(), search: AtomicU8::new(0) });
    let state = AppState::new(Config::new(b"/home".to_vec()), std::time::Instant::now());
    let host = host_with(state, probe.clone(), cx);
    let id = RequestId(u64::MAX);
    let search = Request::Search(SearchRequest {
        session: SessionId::nil(),
        root: Arc::from(&b"/"[..]),
        source: SearchSource::Preview,
        query: "a".into(),
        removals: Arc::from(Vec::new()),
        limit: 10,
    });
    let handled = cx.update(|cx| {
        host.update(cx, |host, cx| {
            host.perform(vec![Effect::Backend { id, request: search }], cx);
            assert_eq!(host.in_flight(), 1);
            host.perform(vec![Effect::Cancel(id)], cx);
            assert_eq!(host.in_flight(), 0);
            host.handled()
        })
    });
    cx.run_until_parked();
    assert_eq!(probe.search.load(Ordering::SeqCst), 2, "the backend saw the cancellation");
    assert_eq!(cx.read(|cx| host.read(cx).handled()), handled, "the reply was dropped");
}

#[gpui::test]
fn footer_only_notify_rerenders_only_the_footer(cx: &mut TestAppContext) {
    let h = mapped();
    let session = h.state.active_session_id().expect("a session");
    let host = host_with(h.state, h.backend, cx);
    let (_root, cx) = cx.add_window_view({
        let host = host.clone();
        move |window, cx| MainWindow::new(host, None, window, cx)
    });
    cx.run_until_parked();
    let regions = [Regions::MAP, Regions::LIST, Regions::HEADER, Regions::SIDEBAR, Regions::INSPECTOR, Regions::TRAY];
    let before: Vec<u64> = regions.iter().map(|r| frame_stats::render_count(*r)).collect();
    let footer = frame_stats::render_count(Regions::FOOTER);
    assert!(before.iter().all(|&n| n > 0), "every panel rendered once: {before:?}");
    let notified =
        |cx: &mut gpui::VisualTestContext| cx.read(|cx| regions.map(|r| host.signal(r).read(cx).notifications));
    let signals = notified(cx);

    let progress = ProgressSnapshot { revision: 1_000, item_count: 42, ..ProgressSnapshot::default() };
    cx.update(|_, cx| host.send(Event::Mapping { session, event: MappingEvent::Progress(progress) }, cx));
    cx.run_until_parked();

    assert_eq!(notified(cx), signals, "only the footer signal was notified");
    assert_eq!(frame_stats::render_count(Regions::FOOTER), footer + 1);
    let after: Vec<u64> = regions.iter().map(|r| frame_stats::render_count(*r)).collect();
    assert_eq!(after, before, "map, list and the other panels reused their previous frame");
}

#[gpui::test]
fn quit_and_close_are_refused_while_removal_runs(cx: &mut TestAppContext) {
    let mut h = mapped();
    h.send(Event::Collect { paths: vec![Harness::path("/r/file1")] });
    h.run();
    h.send(Event::BeginReview);
    let review = h.state.review_token().expect("a review");
    h.send(Event::ConfirmTrash { review });
    assert!(h.state.removal_active(), "the Remove request stays pending");
    let removing = host_with(h.state, h.backend, cx);
    assert!(!cx.update(|cx| lifecycle::may_quit(&removing, cx)));
    assert_eq!(
        cx.read(|cx| removing.read(cx).notice().map(|n| n.to_string())).as_deref(),
        Some(lifecycle::QUIT_BLOCKED)
    );

    let idle = mapped();
    let idle = host_with(idle.state, idle.backend, cx);
    assert!(cx.update(|cx| lifecycle::may_quit(&idle, cx)));
}

/// Escape stops mapping (the footer's Stop) only when nothing else takes it, in order: an open
/// sheet closes, a focused search field clears (and keeps Escape even when empty), and only
/// then does mapping stop. While a sheet is open the window's actions (and menu items) are off.
#[gpui::test]
fn escape_stops_mapping_only_after_sheets_and_text_input(cx: &mut TestAppContext) {
    use gpui::Focusable;

    use crate::actions::OpenFolder;

    let mut h = Harness::new(Config::new(b"/home".to_vec()));
    h.backend.add_file(b"/r/docs/x.txt", 30);
    h.backend.add_file(b"/r/file1", 10);
    h.open(b"/r");
    h.preview(b"/r");
    h.run();
    let backend = h.backend.clone();
    let host = host_with(h.state, h.backend, cx);
    let (root, cx) = cx.add_window_view({
        let host = host.clone();
        move |window, cx| MainWindow::new(host, None, window, cx)
    });
    cx.run_until_parked();
    let stops = || backend.log().iter().filter(|line| *line == "stop").count();
    let escape = |cx: &mut gpui::VisualTestContext| {
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    };
    let window_actions = |cx: &mut gpui::VisualTestContext| cx.update(|w, cx| w.is_action_available(&OpenFolder, cx));
    assert!(cx.read(|cx| crate::main_window::escape_stops_mapping(host.state(cx))));
    assert!(window_actions(cx));

    // An open sheet takes Escape; meanwhile the window's actions are unavailable.
    let sheets = cx.read(|cx| root.read(cx).panels().sheets.clone());
    cx.update(|_, cx| sheets.update(cx, |layer, cx| layer.open_skipped_items(cx)));
    cx.run_until_parked();
    assert!(!window_actions(cx), "menu items are disabled while a sheet is open");
    escape(cx);
    assert!(!cx.read(|cx| sheets.read(cx).is_open(cx)));
    assert!(window_actions(cx));
    assert_eq!(stops(), 0);

    // The focused search field clears its query, then still keeps Escape.
    let toolbar = cx.read(|cx| root.read(cx).panels().toolbar.clone());
    cx.update(|window, cx| toolbar.update(cx, |toolbar, cx| toolbar.focus_search(window, cx)));
    cx.simulate_input("ab");
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| host.state(cx).query().to_owned()), "ab");
    escape(cx);
    assert_eq!(cx.read(|cx| host.state(cx).query().to_owned()), "");
    escape(cx);
    assert_eq!(stops(), 0, "a focused text input never stops mapping");

    // Nothing else consumes it: mapping stops.
    cx.update(|window, cx| window.focus(&root.read(cx).focus_handle(cx)));
    escape(cx);
    assert_eq!(stops(), 1);
    assert!(!cx.read(|cx| crate::main_window::escape_stops_mapping(host.state(cx))), "the footer offers Resume");
}

/// The frame harness without a display (`cargo nextest run -p orbit-ui --run-ignored only
/// headless`): the fixture app in a test window, the built-in script, and the CPU time of each
/// frame's effects + render + layout + prepaint + paint (no GPU, no vsync). Use it when the live
/// `--frame-harness` cannot run (locked screen, CI).
#[gpui::test]
#[ignore = "measurement, not a test"]
fn headless_frame_harness(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use crate::fixture::{self, FixtureOptions, SharedFixture};
    use crate::frame_stats::Percentiles;
    use crate::harness::{FrameHarness, HarnessScript};

    let options = FixtureOptions { mapping_duration: Duration::from_millis(300), ..FixtureOptions::default() };
    let backend: Arc<dyn Backend> = Arc::new(SharedFixture::new(options));
    let host = cx.update(|cx| {
        Host::new(
            HostOptions {
                config: Config::new(fixture::HOME.to_vec()),
                backend,
                settings: SettingsStore::in_memory(Settings::default()),
            },
            cx,
        )
    });
    let (root, cx) = cx.add_window_view({
        let host = host.clone();
        move |window, cx| MainWindow::new(host, None, window, cx)
    });
    cx.update(|_, cx| host.update(cx, |host, cx| host.launch(Some(fixture::ROOT.to_vec()), cx)));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        cx.run_until_parked();
        let ready = cx.read(|cx| {
            let state = host.state(cx);
            state.snapshot().is_some() && !state.is_scanning() && state.row_count() > 0
        });
        if ready {
            break;
        }
        assert!(Instant::now() < deadline, "the fixture map was not ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let panels = cx.read(|cx| root.read(cx).panels().clone());
    let mut harness = FrameHarness::new(host.clone(), panels, HarnessScript::builtin());
    let mut all = Vec::new();
    for step in HarnessScript::builtin().steps {
        let mut work = Vec::new();
        let handled = cx.read(|cx| host.read(cx).handled());
        let renders = crate::regions::ALL.map(|(region, _)| frame_stats::render_count(region));
        for frame in 0..step.frames() {
            // Test windows draw when the update's effects are flushed: time input + draw.
            let start = Instant::now();
            cx.update(|_, cx| {
                harness.drive(step, frame, cx);
                root.update(cx, |_, cx| cx.notify());
            });
            cx.run_until_parked();
            work.push(start.elapsed());
        }
        let renders: Vec<String> = crate::regions::ALL
            .iter()
            .zip(renders)
            .map(|((region, name), before)| format!("{name}={}", frame_stats::render_count(*region) - before))
            .collect();
        println!(
            "{:<12} frame work (headless CPU): {} | events {} | renders {}",
            step.name(),
            Percentiles::of(&work),
            cx.read(|cx| host.read(cx).handled()) - handled,
            renders.join(" ")
        );
        all.extend(work);
    }
    println!("{:<12} frame work (headless CPU): {}", "overall", Percentiles::of(&all));
}
