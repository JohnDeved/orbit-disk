//! Sheet input rules: keyboard defaults, outside clicks, review tokens, error timing. The domain
//! rules behind them (token matching, leases) are tested in `orbit-app-state`.

use std::cell::RefCell;
use std::sync::Arc;

use gpui::{AppContext, Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use orbit_app_state::testing::Harness;
use orbit_app_state::{AppState, Backend, Config, Event};

use super::{ConfirmKind, Layer, SheetsLayer};
use crate::host::{AppHost, Host};
use crate::main_window::MainWindow;
use crate::settings::{Settings, SettingsStore};

thread_local! {
    static SENT: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn record(event: &Event) {
    SENT.with(|sent| sent.borrow_mut().push(event.clone()));
}

/// What the sheets sent since the last call.
fn take_sent() -> Vec<Event> {
    SENT.with(|sent| std::mem::take(&mut *sent.borrow_mut()))
}

/// `/r` mapped, `/r/file1` and `/r/docs` collected.
fn collected() -> Harness {
    let mut h = Harness::new(Config::new(b"/home".to_vec()));
    h.backend.add_file(b"/r/big/a.bin", 100);
    h.backend.add_file(b"/r/docs/x.txt", 30);
    h.backend.add_file(b"/r/file1", 10);
    h.open_mapped(b"/r");
    h.settle();
    h.send(Event::Collect { paths: vec![Harness::path("/r/file1"), Harness::path("/r/docs")] });
    h.run();
    assert_eq!(h.state.collection().map(|c| c.len()), Some(2));
    h
}

fn window(
    state: AppState,
    backend: Arc<dyn Backend>,
    cx: &mut TestAppContext,
) -> (Host, Entity<MainWindow>, &mut VisualTestContext) {
    let host = cx.update(|cx| {
        let entity =
            cx.new(|cx| AppHost::with_state(state, backend, SettingsStore::in_memory(Settings::default()), cx));
        Host::from_entity(entity, cx)
    });
    let (root, cx) = cx.add_window_view({
        let host = host.clone();
        move |window, cx| MainWindow::new(host, None, window, cx)
    });
    cx.run_until_parked();
    take_sent();
    (host, root, cx)
}

fn send(host: &Host, event: Event, cx: &mut VisualTestContext) {
    cx.update(|_, cx| host.send(event, cx));
    cx.run_until_parked();
}

/// Whether `selector` was drawn. Only for presence: GPUI keeps debug bounds of older frames.
fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    // Background replies (e.g. a removal outcome) invalidate the window without drawing it;
    // draw a frame first so the assertion sees what the user would.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(selector).is_some()
}

/// The open cards, bottom to top, as the layer draws them.
fn open(root: &Entity<MainWindow>, cx: &mut VisualTestContext) -> Vec<Layer> {
    cx.read(|cx| {
        let layer = root.read(cx).panels().sheets.read(cx);
        layer.layers(layer.host.state(cx))
    })
}

const REVIEW: Layer = Layer::Review;
const TRASH: Layer = Layer::Confirmation(ConfirmKind::Trash);
const PERMANENT: Layer = Layer::Confirmation(ConfirmKind::Permanent);

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is on screen"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn keys(cx: &mut VisualTestContext, keystrokes: &str) {
    cx.simulate_keystrokes(keystrokes);
    cx.run_until_parked();
}

fn handled(host: &Host, cx: &mut VisualTestContext) -> u64 {
    cx.read(|cx| host.read(cx).handled())
}

#[gpui::test]
fn return_never_deletes_permanently_and_escape_closes_only_the_top_sheet(cx: &mut TestAppContext) {
    let h = collected();
    let (host, root, cx) = window(h.state, h.backend.clone(), cx);
    send(&host, Event::BeginReview, cx);
    click(cx, "review-delete");
    assert_eq!(open(&root, cx), [REVIEW, PERMANENT]);
    assert!(cx.read(|cx| host.state(cx).confirmation_lease()).is_some());
    take_sent();

    keys(cx, "enter");
    assert!(take_sent().is_empty(), "Return sends nothing from the permanent-delete sheet");
    assert!(!cx.read(|cx| host.state(cx).removal_active()));
    assert!(!h.backend.log().iter().any(|line| line.starts_with("remove")), "{:?}", h.backend.log());
    assert_eq!(open(&root, cx), [REVIEW, PERMANENT], "the sheet stays open");

    keys(cx, "escape");
    assert!(matches!(take_sent().as_slice(), [Event::EndConfirmation { .. }]));
    assert!(cx.read(|cx| host.state(cx).confirmation_lease()).is_none(), "the lease was returned");
    assert_eq!(open(&root, cx), [REVIEW], "the review below stays open");

    keys(cx, "escape");
    assert!(matches!(take_sent().as_slice(), [Event::DismissReview { .. }]));
    assert!(!cx.read(|cx| host.state(cx).show_review()));
    assert_eq!(open(&root, cx), []);
}

#[gpui::test]
fn outside_click_dismisses_only_the_top_sheet_and_is_consumed(cx: &mut TestAppContext) {
    let h = collected();
    let (host, root, cx) = window(h.state, h.backend, cx);
    send(&host, Event::BeginReview, cx);
    click(cx, "review-trash");
    assert_eq!(open(&root, cx), [REVIEW, TRASH]);
    take_sent();

    // Inside the review card but outside the alert: only the alert closes.
    let review = cx.debug_bounds("review-sheet").expect("the review is on screen");
    let before = handled(&host, cx);
    cx.simulate_click(review.origin + point(px(8.), px(8.)), Modifiers::none());
    cx.run_until_parked();
    assert!(matches!(take_sent().as_slice(), [Event::EndConfirmation { .. }]));
    assert_eq!(handled(&host, cx), before + 1, "nothing below handled the click");
    assert_eq!(open(&root, cx), [REVIEW]);
    assert!(cx.read(|cx| host.state(cx).show_review()), "the review stays open");

    // Over the sidebar and over the map: the review closes, no panel below reacts.
    for position in [point(px(40.), px(300.)), point(px(300.), px(420.))] {
        assert!(!review.contains(&position));
        let before = handled(&host, cx);
        cx.simulate_click(position, Modifiers::none());
        cx.run_until_parked();
        assert!(matches!(take_sent().as_slice(), [Event::DismissReview { .. }]), "at {position:?}");
        assert_eq!(handled(&host, cx), before + 1, "nothing below handled the click at {position:?}");
        assert!(!cx.read(|cx| host.state(cx).show_review()));
        send(&host, Event::BeginReview, cx);
    }
}

#[gpui::test]
fn review_actions_carry_the_rendered_token(cx: &mut TestAppContext) {
    let h = collected();
    let (host, root, cx) = window(h.state, h.backend, cx);
    send(&host, Event::BeginReview, cx);
    let first = cx.read(|cx| host.state(cx).review_token()).expect("a review");

    click(cx, "review-remove-0");
    match take_sent().as_slice() {
        [Event::Uncollect { review: Some(review), .. }] => assert_eq!(review, &first),
        other => panic!("expected one Uncollect with the review token, got {other:?}"),
    }
    assert_eq!(cx.read(|cx| host.state(cx).collection().map(|c| c.len())), Some(1));

    // The first token is stale now: the app state ignores actions that still carry it.
    let remaining = cx.read(|cx| host.state(cx).collection().map(|c| c.items()[0].path.clone())).expect("an item");
    send(&host, Event::Uncollect { path: remaining, review: Some(first.clone()) }, cx);
    send(&host, Event::ClearCollection { review: first.clone() }, cx);
    assert_eq!(cx.read(|cx| host.state(cx).collection().map(|c| c.len())), Some(1));

    // The sheet re-rendered with the current token: Move to Trash… and Return carry it.
    let current = cx.read(|cx| host.state(cx).review_token()).expect("a review");
    assert_ne!(current, first);
    click(cx, "review-trash");
    keys(cx, "enter");
    match take_sent().as_slice() {
        [
            Event::BeginConfirmation { review: begun },
            Event::ConfirmTrash { review: confirmed },
            Event::EndConfirmation { .. },
        ] => {
            assert_eq!(begun, &current);
            assert_eq!(confirmed, &current);
        }
        other => panic!("unexpected events {other:?}"),
    }
    // Trashing the last item emptied the collection, which closes the review.
    assert_eq!(cx.read(|cx| host.state(cx).collection().map(|c| c.len())), Some(0));
    assert_eq!(open(&root, cx), []);
}

#[gpui::test]
fn errors_wait_until_sheets_close(cx: &mut TestAppContext) {
    let h = collected();
    // Removing `/r/docs` will fail; mapping `/elsewhere` is refused.
    h.backend.protect(b"/r/docs");
    h.backend.exclude(b"/elsewhere", "Orbit Disk can't map this folder.");
    let (host, root, cx) = window(h.state, h.backend, cx);

    // A removal error while the review is open replaces its size text; no alert covers it.
    send(&host, Event::BeginReview, cx);
    click(cx, "review-trash");
    click(cx, "trash-confirm");
    assert!(cx.read(|cx| host.state(cx).error().is_some()), "the removal reported an error");
    assert!(drawn(cx, "review-error"));
    assert_eq!(open(&root, cx), [REVIEW], "no error alert over the review");
    // Done takes the error it showed with it.
    click(cx, "review-done");
    assert!(cx.read(|cx| host.state(cx).error().is_none()));
    assert_eq!(open(&root, cx), []);

    // An error raised while the details sheet is open waits for it to close.
    let layer: Entity<SheetsLayer> = cx.read(|cx| root.read(cx).panels().sheets.clone());
    cx.update(|_, cx| layer.update(cx, |layer, cx| layer.open_skipped_items(cx)));
    cx.run_until_parked();
    send(&host, Event::OpenLocation { path: b"/elsewhere".to_vec(), force: false }, cx);
    assert!(cx.read(|cx| host.state(cx).error().is_some()));
    assert_eq!(open(&root, cx), [Layer::Skipped], "no error alert over the details");

    keys(cx, "escape");
    assert_eq!(open(&root, cx), [Layer::Error], "the error shows once the sheet closed");
    assert!(drawn(cx, "error-alert"));
    keys(cx, "enter");
    assert!(cx.read(|cx| host.state(cx).error().is_none()));
    assert_eq!(open(&root, cx), []);
}
