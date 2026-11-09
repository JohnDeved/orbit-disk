//! Contents list and inspector behavior in a test window: real pointer events through GPUI hit
//! testing, the host's executors, and a manual clock for the hover settle timer.

use std::time::Duration;

use gpui::{
    AppContext, Entity, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    TestAppContext, VisualTestContext, point, px, size,
};
use orbit_app_state::testing::Harness;
use orbit_app_state::{Config, Event, SortOrder};
use orbit_core::view::FolderTree;

use super::{ContentsPanel, SETTLE_DELAY};
use crate::host::{AppHost, Host};
use crate::main_window::MainWindow;
use crate::panels::inspector::InspectorModel;
use crate::settings::{Settings, SettingsStore};
use crate::theme;

/// `/r` with `files` (name, size), mapped and settled.
fn mapped(files: impl IntoIterator<Item = (String, u64)>) -> Harness {
    let mut h = Harness::new(Config::new(b"/home".to_vec()));
    for (name, size) in files {
        h.backend.add_file(format!("/r/{name}").as_bytes(), size);
    }
    h.open_mapped(b"/r");
    h.settle();
    h
}

/// `count` files `f000…` in size order (largest first = name order).
fn numbered(count: usize) -> Harness {
    mapped((0..count).map(|i| (format!("f{i:03}"), 10_000 - i as u64)))
}

struct Window<'a> {
    host: Host,
    root: Entity<MainWindow>,
    list: Entity<ContentsPanel>,
    cx: &'a mut VisualTestContext,
}

fn open(h: Harness, cx: &mut TestAppContext) -> Window<'_> {
    let host = cx.update(|cx| {
        let entity =
            cx.new(|cx| AppHost::with_state(h.state, h.backend, SettingsStore::in_memory(Settings::default()), cx));
        Host::from_entity(entity, cx)
    });
    let (root, cx) = cx.add_window_view({
        let host = host.clone();
        move |window, cx| MainWindow::new(host, None, window, cx)
    });
    cx.simulate_resize(size(px(1280.), px(810.)));
    cx.run_until_parked();
    let list = cx.read(|cx| root.read(cx).panels().contents.clone());
    Window { host, root, list, cx }
}

impl Window<'_> {
    /// Center-left of display row `row` in window coordinates (clear of the folder arrow).
    fn row_point(&mut self, row: usize) -> Point<Pixels> {
        self.cx.read(|cx| {
            let base = self.list.read(cx).base();
            let bounds = base.bounds();
            let y = bounds.top() + base.offset().y + theme::ROW_HEIGHT * row + theme::ROW_HEIGHT / 2.;
            point(bounds.left() + px(60.), y)
        })
    }

    /// Center-left of the `k`-th row slot from the top of the viewport.
    fn slot_point(&mut self, k: usize) -> Point<Pixels> {
        self.cx.read(|cx| {
            let bounds = self.list.read(cx).base().bounds();
            point(bounds.left() + px(60.), bounds.top() + theme::ROW_HEIGHT * k + theme::ROW_HEIGHT / 2.)
        })
    }

    fn viewport(&mut self) -> Pixels {
        self.cx.read(|cx| self.list.read(cx).base().bounds().size.height)
    }

    fn offset(&mut self) -> Pixels {
        self.cx.read(|cx| self.list.read(cx).scroll_offset())
    }

    fn row_hover(&mut self) -> Option<String> {
        self.cx.read(|cx| self.host.state(cx).row_hover().map(theme::display))
    }

    fn current(&mut self) -> String {
        self.cx.read(|cx| theme::display(self.host.state(cx).current_path().unwrap_or_default()))
    }

    fn selected(&mut self) -> Option<String> {
        self.cx.read(|cx| self.host.state(cx).selected().map(|item| theme::display(&item.path)))
    }

    fn move_to(&mut self, position: Point<Pixels>, pressed: Option<MouseButton>) {
        self.cx.simulate_event(MouseMoveEvent { position, modifiers: Modifiers::none(), pressed_button: pressed });
    }

    fn click(&mut self, position: Point<Pixels>, click_count: usize) {
        self.cx.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        self.cx.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
        });
    }

    fn advance(&mut self, by: Duration) {
        self.cx.executor().advance_clock(by);
        self.cx.run_until_parked();
    }

    fn send(&mut self, event: Event) {
        let host = self.host.clone();
        self.cx.update(|_, cx| host.send(event, cx));
        self.cx.run_until_parked();
    }
}

#[gpui::test]
fn scrolling_suppresses_row_hover_until_the_offset_settles_then_hovers_the_row_under_the_pointer(
    cx: &mut TestAppContext,
) {
    let mut w = open(numbered(60), cx);
    let pointer = w.row_point(2);
    w.move_to(pointer, None);
    assert_eq!(w.row_hover().as_deref(), Some("/r/f002"));

    let list = w.list.clone();
    w.cx.update(|_, cx| list.update(cx, |list, cx| list.scroll_by(theme::ROW_HEIGHT * -3., cx)));
    w.cx.run_until_parked();
    assert_eq!(w.row_hover(), None, "the first scroll clears the hover once");
    w.move_to(pointer + point(px(2.), px(0.)), None);
    assert_eq!(w.row_hover(), None, "pointer movement does not hover while scrolling");

    // Momentum: the offset keeps changing, each change re-arms the settle period.
    w.advance(SETTLE_DELAY - Duration::from_millis(30));
    w.cx.update(|_, cx| list.update(cx, |list, cx| list.scroll_by(theme::ROW_HEIGHT * -1., cx)));
    w.advance(SETTLE_DELAY - Duration::from_millis(30));
    assert_eq!(w.row_hover(), None, "still inside the settle period of the last offset change");
    assert!(w.cx.read(|cx| w.list.read(cx).hover_suppressed()));

    w.advance(Duration::from_millis(40));
    assert_eq!(w.row_hover().as_deref(), Some("/r/f006"), "the row now under the pointer, not the old one");
    let next = w.row_point(7);
    w.move_to(next, None);
    assert_eq!(w.row_hover().as_deref(), Some("/r/f007"), "hover follows the pointer again");
}

#[gpui::test]
fn a_double_click_opens_a_folder_once_and_a_dragged_press_never_opens(cx: &mut TestAppContext) {
    let h = mapped([("big/a.bin".into(), 100), ("docs/x.txt".into(), 30), ("file1".into(), 10)]);
    let mut w = open(h, cx);
    let big = w.row_point(0);

    w.click(big, 1);
    assert_eq!((w.current(), w.selected()), ("/r".into(), Some("/r/big".into())), "one click selects");
    w.click(big, 2);
    assert_eq!(w.current(), "/r/big", "the second click of a double click opens");
    assert!(!w.cx.read(|cx| w.host.state(cx).gesture_active()), "the release ended the gesture");

    w.send(Event::GoUp);
    assert_eq!((w.current(), w.selected()), ("/r".into(), Some("/r/big".into())));
    w.click(big, 3);
    assert_eq!(w.current(), "/r", "a third click does not open again");

    // A press that moved beyond the drag threshold is not a click of a double click.
    w.click(big, 1);
    w.cx.simulate_event(MouseDownEvent {
        position: big,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    w.move_to(big + point(px(0.), px(8.)), Some(MouseButton::Left));
    w.cx.simulate_event(MouseUpEvent {
        position: big,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
    });
    assert_eq!(w.current(), "/r", "a dragged press never opens");
}

#[gpui::test]
fn row_drags_start_beyond_the_threshold_and_escape_cancels_them(cx: &mut TestAppContext) {
    let h = mapped([("big/a.bin".into(), 100), ("docs/x.txt".into(), 30), ("file1".into(), 10)]);
    let mut w = open(h, cx);
    let docs = w.row_point(1);
    let drag_id = |w: &mut Window| w.cx.read(|cx| w.host.state(cx).drag_id());
    let gpui_drag = |w: &mut Window| w.cx.update(|_, cx| cx.has_active_drag());

    w.cx.simulate_mouse_down(docs, MouseButton::Left, Modifiers::none());
    w.move_to(docs + point(px(3.), px(3.9)), Some(MouseButton::Left));
    assert_eq!(drag_id(&mut w), None, "below 5 points the press is still a click");
    assert!(!gpui_drag(&mut w), "GPUI's own 2-point threshold did not start a drag");
    w.move_to(docs + point(px(3.), px(4.)), Some(MouseButton::Left));
    assert!(drag_id(&mut w).is_some(), "5 points start the collection drag");
    assert_eq!(w.cx.read(|cx| w.host.state(cx).drag_label().map(str::to_owned)).as_deref(), Some("docs"));
    w.move_to(docs + point(px(3.), px(40.)), Some(MouseButton::Left));
    assert!(gpui_drag(&mut w), "the pointer carries the typed payload");

    w.cx.simulate_keystrokes("escape");
    assert_eq!(drag_id(&mut w), None, "Escape ends the drag without a drop");
    assert!(!gpui_drag(&mut w));
    w.cx.simulate_mouse_up(docs + point(px(3.), px(40.)), MouseButton::Left, Modifiers::none());
    assert!(!w.cx.read(|cx| w.host.state(cx).gesture_active()));
    assert_eq!((w.current(), w.selected()), ("/r".into(), Some("/r/docs".into())), "neither opened nor reselected");

    // Released elsewhere: the drag ends with the gesture.
    w.cx.simulate_mouse_down(docs, MouseButton::Left, Modifiers::none());
    w.move_to(docs + point(px(0.), px(10.)), Some(MouseButton::Left));
    w.move_to(docs + point(px(0.), px(30.)), Some(MouseButton::Left));
    assert!(drag_id(&mut w).is_some());
    w.cx.simulate_mouse_up(docs + point(px(0.), px(30.)), MouseButton::Left, Modifiers::none());
    assert_eq!(drag_id(&mut w), None);
    assert!(!gpui_drag(&mut w));
}

#[gpui::test]
fn map_hover_reveals_the_related_row_only_when_it_is_off_screen(cx: &mut TestAppContext) {
    let mut w = open(numbered(40), cx);
    let viewport = w.viewport();
    let visible_rows = (viewport / theme::ROW_HEIGHT).floor() as usize;
    assert!((3..30).contains(&visible_rows), "test window shows a few rows: {visible_rows}");
    let host = w.host.clone();
    let hover = |w: &mut Window, row: usize| {
        let (segment, layout_revision) = w.cx.update(|_, cx| {
            host.update(cx, |host, _| {
                let presentation = host.presentation().expect("a map");
                let ix = host.state().folder().expect("a folder").rows()[row];
                let segment = presentation.layout.index_of_child(ix, FolderTree::FOLDER).expect("own segment");
                (segment, presentation.layout_revision)
            })
        });
        w.send(Event::HoverSegment { segment, layout_revision });
    };

    hover(&mut w, 1);
    assert_eq!(w.offset(), px(0.), "a visible row does not scroll the list");
    let far = visible_rows + 20;
    hover(&mut w, far);
    let offset = w.offset();
    let top = theme::ROW_HEIGHT * far;
    assert!(-offset <= top && top + theme::ROW_HEIGHT <= -offset + viewport, "row {far} revealed at {offset:?}");
    assert_eq!(top + theme::ROW_HEIGHT, -offset + viewport, "with the least movement (bottom edge)");
    hover(&mut w, far - 2);
    assert_eq!(w.offset(), offset, "the next related row is visible: no scrolling");
    assert!(w.cx.read(|cx| w.host.state(cx).hover_description().is_some()));
}

#[gpui::test]
fn the_first_visible_row_stays_in_place_across_a_background_name_sort(cx: &mut TestAppContext) {
    // Size order and name order differ; 2,500 rows sort in the background.
    let rows = 2_500;
    let h = mapped((0..rows).map(|i| (format!("n{:04}", (i * 7_919) % rows), 1_000_000 - i as u64)));
    let mut w = open(h, cx);
    let top = w.row_point(0);
    w.click(top, 1);
    let list = w.list.clone();
    let intra = px(15.);
    w.cx.update(|_, cx| list.update(cx, |list, cx| list.scroll_by(-(theme::ROW_HEIGHT * 1_000 + intra), cx)));
    w.cx.run_until_parked();
    let first = |w: &mut Window| {
        w.cx.read(|cx| {
            let state = w.host.state(cx);
            let offset = w.list.read(cx).scroll_offset();
            let row = (-offset / theme::ROW_HEIGHT).floor() as usize;
            (theme::display(state.row(row).expect("a row").name()), -offset - theme::ROW_HEIGHT * row)
        })
    };
    let (anchor, before) = first(&mut w);
    assert_eq!(before, intra);

    w.send(Event::SortChanged(SortOrder::Name));
    let state = |w: &mut Window| w.cx.read(|cx| (w.host.state(cx).sort_order(), w.host.state(cx).is_sorting_rows()));
    assert_eq!(state(&mut w), (SortOrder::Name, false), "the background sort finished");
    assert_eq!(w.cx.read(|cx| theme::display(w.host.state(cx).row(0).expect("a row").name())), "n0000");
    assert_eq!(first(&mut w), (anchor, intra), "the anchor row keeps its place on screen");
    let focused = w.cx.update(|window, cx| list.read(cx).focus.is_focused(window));
    assert!(focused, "the list keeps keyboard focus");
}

#[test]
fn inspector_shows_disabled_actions_before_a_selection() {
    let mut h = mapped([("big/a.bin".into(), 100), ("docs/x.txt".into(), 30), ("file1".into(), 10)]);
    let none = InspectorModel::of(&h.state);
    assert_eq!(none.name.as_ref(), "Select an item");
    assert!(!none.has_selection && !none.can_collect && !none.collected);
    assert!(!none.opens_folder, "the Preview slot is shown (disabled), keeping the layout");

    h.send(Event::Select { path: Some(Harness::path("/r/docs")) });
    let folder = InspectorModel::of(&h.state);
    assert!(folder.has_selection && folder.opens_folder && folder.can_collect);
    h.send(Event::Collect { paths: vec![Harness::path("/r/docs")] });
    h.settle();
    let collected = InspectorModel::of(&h.state);
    assert!(collected.collected && !collected.can_collect, "“In Collection”, disabled");
    assert_eq!(collected.path.as_deref(), Some(&b"/r/docs"[..]));
}

/// The Contents list over a 1,000,000-row folder, headless (`cargo nextest run -p orbit-ui
/// --run-ignored only --no-capture headless`): CPU time per frame of input + render + layout +
/// prepaint + paint, and the list region's share (its view's render, layout, prepaint and paint).
#[gpui::test]
#[ignore = "measurement, not a test"]
fn headless_contents_one_million_rows(cx: &mut TestAppContext) {
    use std::time::Instant;

    use crate::frame_stats::{self, Percentiles};
    use crate::regions;

    let rows: u64 = std::env::var("ORBIT_LIST_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000_000);
    let setup = Instant::now();
    // Sizes are a hash of the index, so size order and name order differ. Ten large files get
    // their own map segments and sort last by name.
    let large = (0..10).map(|k| (format!("zz-large-{k}.dat"), 1_000_000_000_000));
    let small = (10..rows).map(|i| (format!("item{i:07}.dat"), 1 + i.wrapping_mul(2_654_435_761) % 1_000_003));
    let h = mapped(large.chain(small));
    let mut w = open(h, cx);
    println!("setup: {rows} rows in {:.1}s", setup.elapsed().as_secs_f64());
    assert_eq!(w.cx.read(|cx| w.host.state(cx).row_count()) as u64, rows);
    let list_region = regions::index(super::REGIONS);
    let visible = (w.viewport() / theme::ROW_HEIGHT).floor() as usize;

    let measure = |w: &mut Window, name: &str, frames: u32, drive: &mut dyn FnMut(&mut Window, u32)| {
        let renders = regions::ALL.map(|(region, _)| frame_stats::render_count(region));
        let formatted = w.cx.read(|cx| w.list.read(cx).rows_formatted());
        let (mut work, mut list) = (Vec::new(), Vec::new());
        for frame in 0..frames {
            frame_stats::reset();
            let start = Instant::now();
            drive(w, frame);
            let root = w.root.clone();
            w.cx.update(|_, cx| root.update(cx, |_, cx| cx.notify()));
            w.cx.run_until_parked();
            work.push(start.elapsed());
            let report = frame_stats::take_report();
            list.push(Duration::from_secs_f64(report.regions[list_region].3 / 1e3));
        }
        let renders: Vec<String> = regions::ALL
            .iter()
            .zip(renders)
            .map(|((region, name), before)| format!("{name}={}", frame_stats::render_count(*region) - before))
            .filter(|entry| !entry.ends_with("=0"))
            .collect();
        let formatted = w.cx.read(|cx| w.list.read(cx).rows_formatted()) - formatted;
        println!("== {name}");
        println!("frame work (headless CPU): {}", Percentiles::of(&work));
        println!("list region (render+layout+prepaint+paint): {}", Percentiles::of(&list));
        println!("renders: {} | rows formatted: {formatted}", renders.join(" "));
    };
    let list = w.list.clone();

    measure(&mut w, "scroll 1M rows (24 pt/frame, 100k-row jump every 60 frames)", 240, &mut |w, frame| {
        let dy = if frame % 60 == 59 { -(theme::ROW_HEIGHT * 100_000) } else { px(-24.) };
        w.cx.update(|_, cx| list.update(cx, |list, cx| list.scroll_by(dy, cx)));
    });
    w.advance(SETTLE_DELAY * 2);
    measure(&mut w, "hover rows (pointer moves one row per frame)", 240, &mut |w, frame| {
        let target = w.slot_point(frame as usize % visible.max(1));
        w.cx.update(|_, cx| list.update(cx, |list, cx| list.pointer_moved(target, cx)));
    });
    let sorting = Instant::now();
    w.send(Event::SortChanged(SortOrder::Name));
    let state = w.cx.read(|cx| (w.host.state(cx).sort_order(), w.host.state(cx).is_sorting_rows()));
    assert_eq!(state, (SortOrder::Name, false));
    println!("== name sort of {rows} rows (background) applied in {:.0} ms", sorting.elapsed().as_secs_f64() * 1e3);
    measure(&mut w, "scroll in name order", 120, &mut |w, _| {
        w.cx.update(|_, cx| list.update(cx, |list, cx| list.scroll_by(px(-24.), cx)));
    });
    // Map hover on a large file's segment (last rows by name) while the list shows the top: the
    // worst case of the reveal, a scan of the name order.
    let host = w.host.clone();
    let (segments, layout_revision) = w.cx.update(|_, cx| {
        host.update(cx, |host, _| {
            let presentation = host.presentation().expect("a map");
            let rows = host.state().folder().expect("a folder").rows().clone();
            let segments: Vec<_> = (0..10)
                .map(|row| presentation.layout.index_of_child(rows[row], FolderTree::FOLDER).expect("own segment"))
                .collect();
            (segments, presentation.layout_revision)
        })
    });
    let step = "map hover → reveal a far row (name order); every other frame the list jumps back to the top";
    measure(&mut w, step, 120, &mut |w, frame| {
        if frame % 2 == 0 {
            w.cx.update(|_, cx| list.update(cx, |list, cx| list.scroll_by(px(1e9), cx)));
        } else {
            let segment = segments[(frame as usize / 2) % segments.len()];
            let host = w.host.clone();
            w.cx.update(|_, cx| host.send(Event::HoverSegment { segment, layout_revision }, cx));
        }
    });
    assert!(w.offset() < px(0.), "the last hover revealed a row at the end");
}
