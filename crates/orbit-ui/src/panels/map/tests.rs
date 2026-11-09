//! Map panel: layer caching under hover and collection changes, hover event economy, click vs
//! drag, the center. Input goes through GPUI's event dispatch (`simulate_mouse_*`), so the
//! window-level listeners, hitbox and typed drag are exercised as in the app.

use std::sync::Arc;

use gpui::{AppContext, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, point, px};
use orbit_app_state::testing::{FakeBackend, Harness};
use orbit_app_state::{AppState, Backend, Config, Event};
use orbit_sunburst::CollectionState;

use super::MapPanel;
use crate::host::{AppHost, Host};
use crate::settings::{Settings, SettingsStore};

/// `/r`: `big` (100: `a.bin`), `docs` (30: `x.txt`), `file1` (10); mapped and settled.
fn mapped() -> Harness {
    let mut h = Harness::new(Config::new(b"/home".to_vec()));
    h.backend.add_file(b"/r/big/a.bin", 100);
    h.backend.add_file(b"/r/docs/x.txt", 30);
    h.backend.add_file(b"/r/file1", 10);
    h.open_mapped(b"/r");
    h.settle();
    h
}

fn open(cx: &mut TestAppContext) -> (Host, Entity<MapPanel>, &mut VisualTestContext) {
    let h = mapped();
    let (state, backend): (AppState, Arc<dyn Backend>) = (h.state, h.backend as Arc<FakeBackend>);
    let host = cx.update(|cx| {
        let entity =
            cx.new(|cx| AppHost::with_state(state, backend, SettingsStore::in_memory(Settings::default()), cx));
        Host::from_entity(entity, cx)
    });
    let (map, cx) = cx.add_window_view({
        let host = host.clone();
        move |_, cx| MapPanel::new(&host, cx)
    });
    cx.run_until_parked();
    (host, map, cx)
}

/// A point inside segment `index` (middle of its painted sector).
fn inside(map: &Entity<MapPanel>, index: usize, cx: &mut VisualTestContext) -> Point<Pixels> {
    cx.read(|cx| {
        let map = map.read(cx);
        let (_, metrics) = map.chart().expect("laid out");
        let sector = map.presentation().expect("a map").layout.sector(&metrics, index);
        let (theta, radius) = ((sector.start + sector.end) / 2.0, sector.mid_radius());
        point(
            px((metrics.center.x + radius * theta.sin()) as f32),
            px((metrics.center.y - radius * theta.cos()) as f32),
        )
    })
}

/// The segment showing the child `name` of the current folder.
fn segment(host: &Host, map: &Entity<MapPanel>, name: &str, cx: &mut VisualTestContext) -> usize {
    cx.read(|cx| {
        let folder = host.state(cx).folder().expect("a folder").clone();
        let presentation = map.read(cx).presentation().expect("a map").clone();
        presentation
            .layout
            .segments()
            .iter()
            .position(|s| s.node.is_some_and(|ix| folder.name(ix) == name.as_bytes()))
            .unwrap_or_else(|| panic!("no segment for {name}"))
    })
}

fn center(map: &Entity<MapPanel>, cx: &mut VisualTestContext) -> Point<Pixels> {
    cx.read(|cx| {
        let (_, metrics) = map.read(cx).chart().expect("laid out");
        point(px(metrics.center.x as f32), px(metrics.center.y as f32))
    })
}

fn handled(host: &Host, cx: &mut VisualTestContext) -> u64 {
    cx.read(|cx| host.read(cx).handled())
}

fn current(host: &Host, cx: &mut VisualTestContext) -> String {
    cx.read(|cx| String::from_utf8_lossy(host.state(cx).current_path().unwrap_or_default()).into_owned())
}

fn move_to(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_mouse_move(at, None, Modifiers::none());
}

#[gpui::test]
fn hover_sweep_repaints_outlines_but_never_rebuilds_the_base_layer(cx: &mut TestAppContext) {
    let (_host, map, cx) = open(cx);
    let before = cx.read(|cx| map.read(cx).stats());
    assert_eq!((before.geometry_builds, before.marks_builds), (1, 1), "built once for the first frame");
    let (_, metrics) = cx.read(|cx| map.read(cx).chart().expect("laid out"));
    for step in 0..240 {
        let angle = f64::from(step) * 0.11;
        let radius = metrics.inner_radius * 0.5 + metrics.ring_width * f64::from(step % 5);
        let at = point(
            px((metrics.center.x + radius * angle.sin()) as f32),
            px((metrics.center.y - radius * angle.cos()) as f32),
        );
        move_to(cx, at);
    }
    let after = cx.read(|cx| map.read(cx).stats());
    assert_eq!((after.geometry_builds, after.marks_builds), (1, 1), "hover never rebuilds fills or marks");
    assert!(after.outline_builds > before.outline_builds + 10, "the outline layer followed the pointer");
}

#[gpui::test]
fn hover_segment_is_sent_only_when_the_hovered_segment_changes(cx: &mut TestAppContext) {
    let (host, map, cx) = open(cx);
    let big = segment(&host, &map, "big", cx);
    let docs = segment(&host, &map, "docs", cx);
    let at = inside(&map, big, cx);
    let events = handled(&host, cx);
    move_to(cx, at);
    assert_eq!(handled(&host, cx), events + 1, "entering a segment");
    move_to(cx, at + point(px(1.), px(1.)));
    move_to(cx, at - point(px(1.), px(0.)));
    assert_eq!(handled(&host, cx), events + 1, "moving within the segment sends nothing");
    let at = inside(&map, docs, cx);
    move_to(cx, at);
    assert_eq!(handled(&host, cx), events + 2, "another segment");
    let hole = center(&map, cx);
    move_to(cx, hole);
    assert_eq!(handled(&host, cx), events + 3, "the center clears the map hover");
    move_to(cx, point(px(1.), px(1.)));
    assert_eq!(handled(&host, cx), events + 3, "nothing to clear outside");
}

#[gpui::test]
fn a_short_press_opens_and_a_press_beyond_the_threshold_drags_the_pressed_segment(cx: &mut TestAppContext) {
    let (host, map, cx) = open(cx);
    // Under the threshold: still a click, and the folder opens on release.
    let big = inside(&map, segment(&host, &map, "big", cx), cx);
    cx.simulate_mouse_down(big, MouseButton::Left, Modifiers::none());
    assert!(cx.read(|cx| host.state(cx).gesture_active()), "mouse-down holds updates");
    cx.simulate_mouse_move(big + point(px(3.), px(3.)), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(big + point(px(3.), px(3.)), MouseButton::Left, Modifiers::none());
    assert_eq!(current(&host, cx), "/r/big");
    assert!(!cx.read(|cx| host.state(cx).gesture_active()), "mouse-up releases the hold");

    cx.update(|_, cx| host.send(Event::GoUp, cx));
    cx.run_until_parked();
    assert_eq!(current(&host, cx), "/r");

    // Beyond the threshold: a collection drag of the pressed segment, even after crossing
    // another one; releasing outside a drop target collects nothing and does not navigate.
    let docs = inside(&map, segment(&host, &map, "docs", cx), cx);
    let big = inside(&map, segment(&host, &map, "big", cx), cx);
    cx.simulate_mouse_down(docs, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(docs + point(px(4.), px(4.)), MouseButton::Left, Modifiers::none());
    assert!(cx.read(|cx| host.state(cx).drag_id().is_some()), "5 points start the drag");
    assert_eq!(cx.read(|cx| host.state(cx).drag_label().map(str::to_owned)).as_deref(), Some("docs"));
    cx.simulate_mouse_move(big, MouseButton::Left, Modifiers::none());
    assert!(cx.read(|cx| cx.has_active_drag()), "GPUI carries the typed drag");
    assert_eq!(cx.read(|cx| host.state(cx).drag_label().map(str::to_owned)).as_deref(), Some("docs"));
    cx.simulate_mouse_up(big, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.read(|cx| {
        let state = host.state(cx);
        assert!(state.drag_id().is_none() && !state.gesture_active());
        assert!(state.collection().is_none_or(|c| c.is_empty()));
    });
    assert_eq!(current(&host, cx), "/r", "a drag never clicks");

    // Escape cancels a drag in flight.
    cx.simulate_mouse_down(docs, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(docs + point(px(6.), px(0.)), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(docs + point(px(12.), px(0.)), MouseButton::Left, Modifiers::none());
    assert!(cx.read(|cx| host.state(cx).drag_id().is_some()));
    cx.simulate_keystrokes("escape");
    cx.read(|cx| {
        assert!(host.state(cx).drag_id().is_none() && !host.state(cx).gesture_active());
        assert!(!cx.has_active_drag());
    });
    cx.simulate_mouse_up(docs, MouseButton::Left, Modifiers::none());
    assert_eq!(current(&host, cx), "/r");
}

#[gpui::test]
fn the_center_goes_up_except_at_the_root(cx: &mut TestAppContext) {
    let (host, map, cx) = open(cx);
    let hole = center(&map, cx);
    move_to(cx, hole);
    let events = handled(&host, cx);
    cx.simulate_click(hole, Modifiers::none());
    assert_eq!(handled(&host, cx), events, "the root center is inert");
    assert_eq!(current(&host, cx), "/r");

    cx.update(|_, cx| host.send(Event::Open { path: Harness::path("/r/big") }, cx));
    cx.run_until_parked();
    assert_eq!(current(&host, cx), "/r/big");
    let hole = center(&map, cx);
    cx.simulate_click(hole, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(current(&host, cx), "/r");
}

#[gpui::test]
fn a_collection_change_rebuilds_the_marks_but_not_the_geometry(cx: &mut TestAppContext) {
    let (host, map, cx) = open(cx);
    let before = cx.read(|cx| map.read(cx).stats());
    let revision = cx.read(|cx| map.read(cx).presentation().expect("a map").layout_revision);
    cx.update(|_, cx| host.send(Event::Collect { paths: vec![Harness::path("/r/file1")] }, cx));
    cx.run_until_parked();
    let file1 = segment(&host, &map, "file1", cx);
    cx.read(|cx| {
        let map = map.read(cx);
        let presentation = map.presentation().expect("a map");
        assert_eq!(presentation.layout_revision, revision, "same layout");
        assert_eq!(presentation.state(file1), CollectionState::Included);
        let after = map.stats();
        assert_eq!(after.geometry_builds, before.geometry_builds, "fills are reused");
        assert_eq!(after.marks_builds, before.marks_builds + 1, "marks are rebuilt once");
    });
}

/// Layer build and hover costs (`cargo nextest run -p orbit-ui --release --run-ignored only
/// --no-capture map_measurements`). Prints p50/p99; asserts nothing about time.
#[test]
#[ignore = "measurement, not a test"]
fn map_measurements() {
    measure::run();
}

mod measure {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use gpui::{Bounds, point, px, size};
    use orbit_sunburst::{CollectionState, Layout, LayoutSource, Point as SPoint};

    use crate::frame_stats::Percentiles;
    use crate::panels::map::layers::{self, Fills, Marks};

    #[derive(Default)]
    struct Arena {
        sizes: Vec<u64>,
        children: Vec<Arc<[u32]>>,
    }

    impl Arena {
        fn node(&mut self, size: u64) -> u32 {
            self.sizes.push(size);
            self.children.push(Arc::from([]));
            (self.sizes.len() - 1) as u32
        }

        fn adopt(&mut self, parent: u32, mut ids: Vec<u32>) {
            ids.sort_by_key(|&id| std::cmp::Reverse(self.sizes[id as usize]));
            self.sizes[parent as usize] = ids.iter().map(|&id| self.sizes[id as usize]).sum();
            self.children[parent as usize] = ids.into();
        }

        /// 4 × 48 × 2 × 1 with jitter: more than the 768-segment cap (as in the sunburst bench).
        fn saturating() -> (Self, u32) {
            let mut arena = Self::default();
            let mut seed = 0x9e37_79b9_7f4a_7c15u64;
            let mut jitter = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                900 + seed % 200
            };
            let root = arena.node(0);
            let folders = (0..4)
                .map(|_| {
                    let folder = arena.node(0);
                    let subfolders = (0..48)
                        .map(|_| {
                            let subfolder = arena.node(0);
                            let pairs = (0..2)
                                .map(|_| {
                                    let inner = arena.node(0);
                                    let file = arena.node(jitter());
                                    arena.adopt(inner, vec![file]);
                                    inner
                                })
                                .collect();
                            arena.adopt(subfolder, pairs);
                            subfolder
                        })
                        .collect();
                    arena.adopt(folder, subfolders);
                    folder
                })
                .collect();
            arena.adopt(root, folders);
            (arena, root)
        }

        fn flat(count: usize, power: Option<f64>) -> (Self, u32) {
            let mut arena = Self::default();
            let root = arena.node(0);
            let ids = (0..count)
                .map(|i| arena.node(power.map_or(4_096, |p| (1e12 / (i as f64 + 1.0).powf(p)) as u64 + 1)))
                .collect();
            arena.adopt(root, ids);
            (arena, root)
        }
    }

    impl LayoutSource for Arena {
        type Id = u32;
        fn size(&self, id: u32) -> u64 {
            self.sizes[id as usize]
        }
        fn children(&self, id: u32) -> &[u32] {
            &self.children[id as usize]
        }
        fn shared_children(&self, id: u32) -> Option<Arc<[u32]>> {
            Some(Arc::clone(&self.children[id as usize]))
        }
    }

    fn time(samples: usize, mut f: impl FnMut()) -> Percentiles {
        let durations: Vec<Duration> = (0..samples)
            .map(|_| {
                let start = Instant::now();
                f();
                start.elapsed()
            })
            .collect();
        Percentiles::of(&durations)
    }

    pub fn run() {
        // The default window's map column: (1280 − 210 − 340 − 24 − 32) × (810 − 52 − 48 − 51 − 105 − 90).
        let bounds = Bounds::new(point(px(226.), px(100.)), size(px(674.), px(464.)));
        let metrics = layers::metrics(bounds);
        let cases = [
            ("saturating", Arena::saturating()),
            ("flat_1m", Arena::flat(1_000_000, None)),
            ("power_law_1m", Arena::flat(1_000_000, Some(1.1))),
        ];
        for (name, (arena, root)) in &cases {
            let layout = Layout::build(arena, *root, None);
            let included = vec![CollectionState::Included; layout.len()];
            let mixed: Vec<_> = (0..layout.len())
                .map(|i| match i % 3 {
                    0 => CollectionState::Included,
                    1 => CollectionState::Partial,
                    _ => CollectionState::None,
                })
                .collect();
            let fills = time(40, || drop(Fills::build(&layout, &metrics)));
            let marks_all = time(40, || drop(Marks::build(&layout, &included, &metrics)));
            let marks_mixed = time(40, || drop(Marks::build(&layout, &mixed, &metrics)));
            let paths = Fills::build(&layout, &metrics).paths.len();
            let marks = Marks::build(&layout, &included, &metrics);
            println!(
                "{name:<13} segments={:<4} fill paths={paths:<3} hatch paths={} | fills {fills} | marks (all included) \
                 {marks_all} | marks (mixed) {marks_mixed}",
                layout.len(),
                marks.hatch.len(),
            );
            // Per pointer move: one hit test, and when the segment changes, its outline.
            let mut seed = 0x2545_f491_4f6c_dd1du64;
            let mut unit = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 11) as f64 / (1u64 << 53) as f64
            };
            let (x, y) = (f64::from(bounds.origin.x), f64::from(bounds.origin.y));
            let (w, h) = (f64::from(bounds.size.width), f64::from(bounds.size.height));
            let points: Vec<SPoint> = (0..5_000).map(|_| SPoint::new(x + unit() * w, y + unit() * h)).collect();
            let mut points = points.into_iter().cycle();
            let hover = time(5_000, || {
                let point = points.next().expect("cycle");
                if let Some(index) = layout.hit_test(&metrics, point) {
                    drop(layers::outline(&layout, &metrics, index));
                }
            });
            println!("{name:<13} hover move (hit test + outline build): {hover}");
        }
    }
}
