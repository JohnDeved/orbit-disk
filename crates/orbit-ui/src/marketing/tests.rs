//! Each screenshot state is reached on the sample fixture through the real host and main window,
//! and the ready callback fires only once it is.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{TestAppContext, WindowHandle};
use orbit_app_state::{AppState, Backend, Config};

use super::*;
use crate::fixture::{FixtureOptions, SharedFixture};
use crate::host::{Host, HostOptions};
use crate::settings::{Settings, SettingsStore};

/// Launches the app on the sample fixture, drives it to `target` and returns the window once the
/// ready callback fired.
fn reach(target: MarketingState, cx: &mut TestAppContext) -> WindowHandle<MainWindow> {
    let backend: Arc<dyn Backend> = Arc::new(SharedFixture::new(FixtureOptions::sample()));
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
    let window = cx.add_window({
        let host = host.clone();
        move |window, cx| MainWindow::new(host, None, window, cx)
    });
    let ready = Rc::new(Cell::new(false));
    let run = MarketingRun { state: target, window_size: None, await_presented_frame: false };
    let task = cx.update(|cx| {
        host.update(cx, |host, cx| host.launch(Some(fixture::HOME.to_vec()), cx));
        let ready = Rc::clone(&ready);
        drive(window, run, move |_| ready.set(true), |message, _| panic!("{message}"), cx)
    });
    // The saved map is published from the fixture's mapping thread; everything else runs on the
    // test executor, whose clock the polls advance.
    for _ in 0..MAX_POLLS + 2 {
        cx.run_until_parked();
        if ready.get() {
            break;
        }
        cx.executor().advance_clock(POLL);
        std::thread::yield_now();
    }
    drop(task);
    assert!(ready.get(), "{target} was not reported ready");
    cx.read(|cx| assert!(target.reached(host.state(cx)), "ready fired before {target} was reached"));
    window
}

fn state<R>(window: WindowHandle<MainWindow>, cx: &mut TestAppContext, read: impl FnOnce(&AppState) -> R) -> R {
    window.update(cx, |main, _, cx| read(main.host().state(cx))).expect("window")
}

fn selected(state: &AppState) -> Option<Vec<u8>> {
    state.selected().map(|item| item.path.to_vec())
}

fn collected(state: &AppState) -> Vec<Vec<u8>> {
    state.collection().map(|c| c.items().iter().map(|item| item.path.to_vec()).collect()).unwrap_or_default()
}

fn sheet_open(window: WindowHandle<MainWindow>, cx: &mut TestAppContext) -> bool {
    window.update(cx, |main, _, cx| main.panels().sheets.read(cx).is_open(cx)).expect("window")
}

#[gpui::test]
fn overview_selects_projects_on_the_home_map(cx: &mut TestAppContext) {
    let window = reach(MarketingState::Overview, cx);
    state(window, cx, |state| {
        assert_eq!(state.current_path(), Some(fixture::HOME));
        assert_eq!(selected(state), Some(sample::path(sample::PROJECTS)));
        // Projects is the largest Home folder: the first row and the first map sector.
        assert_eq!(state.row(0).map(|row| row.path()), Some(sample::path(sample::PROJECTS)));
        assert!(collected(state).is_empty());
        assert!(!state.show_review());
    });
    assert!(!sheet_open(window, cx));
    assert!(cx.read(theme::reduce_motion), "animations are off");
}

#[gpui::test]
fn collection_collects_the_selected_downloads(cx: &mut TestAppContext) {
    let window = reach(MarketingState::Collection, cx);
    state(window, cx, |state| {
        assert_eq!(state.current_path(), Some(fixture::HOME));
        assert_eq!(selected(state), Some(sample::path(sample::DOWNLOADS)));
        assert_eq!(collected(state), vec![sample::path(sample::DOWNLOADS)]);
        assert!(!state.show_review());
    });
    assert!(!sheet_open(window, cx));
}

#[gpui::test]
fn projects_opens_projects_and_selects_studio(cx: &mut TestAppContext) {
    let window = reach(MarketingState::Projects, cx);
    state(window, cx, |state| {
        assert_eq!(state.root_path(), Some(fixture::HOME));
        assert_eq!(state.current_path(), Some(&sample::path(sample::PROJECTS)[..]));
        assert_eq!(selected(state), Some(sample::path(sample::STUDIO)));
        assert_eq!(state.row(0).map(|row| row.path()), Some(sample::path(sample::STUDIO)));
        assert!(collected(state).is_empty());
    });
    assert!(!sheet_open(window, cx));
}

#[gpui::test]
fn review_opens_the_review_of_collected_downloads(cx: &mut TestAppContext) {
    let window = reach(MarketingState::Review, cx);
    state(window, cx, |state| {
        assert_eq!(collected(state), vec![sample::path(sample::DOWNLOADS)]);
        assert!(state.show_review());
        assert!(state.review_token().is_some(), "the review has an owner, so its buttons are enabled");
    });
    assert!(sheet_open(window, cx));
}

#[gpui::test]
fn downloads_selects_downloads_without_collecting(cx: &mut TestAppContext) {
    let window = reach(MarketingState::Downloads, cx);
    state(window, cx, |state| {
        assert_eq!(state.current_path(), Some(fixture::HOME));
        assert_eq!(selected(state), Some(sample::path(sample::DOWNLOADS)));
        assert!(collected(state).is_empty());
    });
    assert!(!sheet_open(window, cx));
}

#[gpui::test]
fn projects_open_opens_projects_without_a_selection(cx: &mut TestAppContext) {
    let window = reach(MarketingState::ProjectsOpen, cx);
    state(window, cx, |state| {
        assert_eq!(state.current_path(), Some(&sample::path(sample::PROJECTS)[..]));
        assert_eq!(selected(state), None);
        assert!(collected(state).is_empty());
    });
    assert!(!sheet_open(window, cx));
}

#[gpui::test]
fn hero_selects_projects_with_downloads_collected(cx: &mut TestAppContext) {
    let window = reach(MarketingState::Hero, cx);
    state(window, cx, |state| {
        assert_eq!(state.current_path(), Some(fixture::HOME));
        assert_eq!(selected(state), Some(sample::path(sample::PROJECTS)));
        assert_eq!(collected(state), vec![sample::path(sample::DOWNLOADS)]);
        assert!(!state.show_review());
    });
    assert!(!sheet_open(window, cx));
}
