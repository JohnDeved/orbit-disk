//! Map panel: base raster caching under hover, collection changes and resizes, raster pixels, hover event economy, click vs
//! drag, the center. Input goes through GPUI's event dispatch (`simulate_mouse_*`), so the
//! window-level listeners, hitbox and typed drag are exercised as in the app.

use std::sync::Arc;

use gpui::{
    AppContext, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, point, px, size,
};
use orbit_app_state::testing::{FakeBackend, Harness};
use orbit_app_state::{AppState, Backend, Config, Event};
use orbit_sunburst::CollectionState;

use super::{MapPanel, layers};
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
    assert_eq!((before.raster_jobs, before.raster_builds), (1, 1), "rasterized once for the first frame");
    assert!(before.raster_bytes > 0);
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
    assert_eq!((after.raster_jobs, after.raster_builds), (1, 1), "hover never rebuilds the base raster");
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
fn a_collection_change_rebuilds_the_raster_once_and_keeps_the_layout(cx: &mut TestAppContext) {
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
        assert_eq!(after.raster_jobs, before.raster_jobs + 1, "one job for the new marks");
        assert_eq!(after.raster_builds, before.raster_builds + 1, "rebuilt once");
    });
}

#[gpui::test]
fn frames_never_wait_for_a_raster_and_newer_bounds_supersede_pending_jobs(cx: &mut TestAppContext) {
    let (_host, map, cx) = open(cx);
    let before = cx.read(|cx| map.read(cx).stats());
    assert!(!cx.read(|cx| map.read(cx).raster_pending()));

    // A resize draws a frame with the old raster (mapped onto the new chart) and queues a job.
    cx.simulate_resize(size(px(700.), px(560.)));
    cx.update(|window, _| window.refresh());
    let resized = cx.read(|cx| map.read(cx).stats());
    assert_eq!(resized.raster_jobs, before.raster_jobs + 1);
    assert_eq!(resized.raster_builds, before.raster_builds, "the frame did not wait for the raster");
    assert!(cx.read(|cx| map.read(cx).raster_pending()));

    // Another resize before the job ran supersedes it: only the latest raster is built.
    cx.simulate_resize(size(px(760.), px(600.)));
    cx.update(|window, _| window.refresh());
    assert_eq!(cx.read(|cx| map.read(cx).stats()).raster_jobs, before.raster_jobs + 2);
    cx.run_until_parked();
    let after = cx.read(|cx| map.read(cx).stats());
    assert_eq!(after.raster_builds, before.raster_builds + 1, "the superseded job was dropped");
    assert!(!cx.read(|cx| map.read(cx).raster_pending()));
    let (bounds, _) = cx.read(|cx| map.read(cx).chart().expect("laid out"));
    let (width, height) = layers::Grid::new(bounds, 2.0).expect("a grid").image_size();
    assert_eq!(after.raster_bytes, width as usize * height as usize * 4, "the new bounds at 2x");
}

/// Synthetic trees for the raster tests and measurements.
mod arena {
    use std::sync::Arc;

    use orbit_sunburst::LayoutSource;

    #[derive(Default)]
    pub struct Arena {
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
        pub fn saturating() -> (Self, u32) {
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

        pub fn flat(count: usize, power: Option<f64>) -> (Self, u32) {
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
}

mod raster {
    use gpui::{Bounds, point, px, size};
    use orbit_sunburst::{CollectionState, Layout, Point as SPoint};

    use super::arena::Arena;
    use crate::panels::map::layers::{self, Grid, Raster};
    use crate::theme;

    /// Straight BGRA of the raster pixel under window point `p`.
    fn pixel(raster: &Raster, p: SPoint) -> [u8; 4] {
        let grid = raster.grid;
        let s = f64::from(grid.scale);
        let (x, y) = ((p.x * s).floor() as i64 - grid.x, (p.y * s).floor() as i64 - grid.y);
        let (width, _) = grid.image_size();
        let offset = (y as usize * width as usize + x as usize) * 4;
        let bytes = raster.image.as_bytes(0).expect("a frame");
        bytes[offset..offset + 4].try_into().expect("4 bytes")
    }

    fn bgra(rgba: [u8; 4]) -> [u8; 4] {
        [rgba[2], rgba[1], rgba[0], rgba[3]]
    }

    fn polar(metrics: &orbit_sunburst::Metrics, radius: f64, theta: f64) -> SPoint {
        SPoint::new(metrics.center.x + radius * theta.sin(), metrics.center.y - radius * theta.cos())
    }

    #[test]
    fn raster_pixels_have_the_gpui_colors_hatching_badges_and_soft_edges() {
        let (arena, root) = Arena::flat(6, Some(1.0));
        let layout = Layout::build(&arena, root, None);
        let mut states = vec![CollectionState::None; layout.len()];
        states[0] = CollectionState::Included;
        states[1] = CollectionState::Partial;
        let bounds = Bounds::new(point(px(10.25), px(20.5)), size(px(400.), px(380.)));
        let metrics = layers::metrics(bounds);
        let grid = Grid::new(bounds, 2.0).expect("a grid");
        let pixels = layers::render(&layout, &states, &metrics, &grid, &|| false).expect("rendered");
        let raster = Raster::from_premultiplied(pixels, grid).expect("converted");
        assert_eq!(raster.image.size(0).width.0 as u32, grid.image_size().0);

        let ink = bgra(layers::expected_rgba(theme::color(theme::INK).into()));
        for (index, segment) in layout.segments().iter().enumerate() {
            let sector = layout.sector(&metrics, index);
            let fill = bgra(layers::expected_rgba(theme::segment_color(segment.color())));
            let theta = (sector.start + sector.end) / 2.0;
            match states[index] {
                CollectionState::None => {
                    assert_eq!(pixel(&raster, polar(&metrics, sector.mid_radius(), theta)), fill, "segment {index}");
                }
                state => {
                    // Inside the badge disc, away from the glyph.
                    let center = sector.badge_center(16.0).expect("a badge fits");
                    let radius = if state == CollectionState::Included { 8.0 } else { 7.0 };
                    assert_eq!(pixel(&raster, SPoint::new(center.x, center.y - radius * 0.7)), ink);
                }
            }
            // Anti-aliased outer edge: a partially covered pixel of the straight fill color (within
            // the precision of 8-bit premultiplied alpha: one step of the composited value).
            let edge: Vec<[u8; 4]> = (0..40)
                .flat_map(|step| {
                    let radius = sector.outer_radius - 1.0 + f64::from(step) * 0.05;
                    [0.02, 0.05, 0.11].map(|offset| pixel(&raster, polar(&metrics, radius, theta + offset)))
                })
                .collect();
            if states[index] == CollectionState::None {
                assert!(
                    edge.iter().any(|p| {
                        p[3] > 0
                            && p[3] < 255
                            && (0..3).all(|k| (i32::from(p[k]) - i32::from(fill[k])).abs() * i32::from(p[3]) <= 255)
                    }),
                    "segment {index} edge {edge:?}"
                );
            }
        }
        // The included sector carries ink hatching between its badge and its edges.
        let sector = layout.sector(&metrics, 0);
        let hatched = (0..400)
            .map(|step| {
                let theta = sector.start + (sector.end - sector.start) * f64::from(step) / 400.0;
                pixel(&raster, polar(&metrics, sector.inner_radius + 4.0, theta))
            })
            .filter(|p| *p == ink)
            .count();
        assert!(hatched > 10, "hatch pixels: {hatched}");
        assert_eq!(pixel(&raster, metrics.center)[3], 0, "the center hole is transparent");
    }

    /// Exact-area coverage: a gentle arc (the top of a ring) ramps through many levels instead of
    /// the few plateaus of a supersampling rasterizer, and a disc's summed coverage is its area.
    #[test]
    fn coverage_is_exact_area_along_gentle_arcs() {
        let (arena, root) = Arena::flat(1, None);
        let layout = Layout::build(&arena, root, None);
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(900.), px(900.)));
        let metrics = layers::metrics(bounds);
        let grid = Grid::new(bounds, 2.0).expect("a grid");
        let pixels = layers::render(&layout, &[], &metrics, &grid, &|| false).expect("rendered");
        let width = pixels.width as usize;
        let sector = layout.sector(&metrics, 0);
        let top = ((metrics.center.y - sector.outer_radius) * 2.0) as usize;
        let center_x = (metrics.center.x * 2.0) as usize;
        let mut levels = std::collections::BTreeSet::new();
        for x in center_x + 10..center_x + 150 {
            for y in top - 2..top + 40 {
                let alpha = pixels.data[(y * width + x) * 4 + 3];
                if alpha > 0 && alpha < 255 {
                    levels.insert(alpha);
                }
            }
        }
        assert!(levels.len() > 100, "partial coverage levels along the arc: {}", levels.len());

        // Summed coverage of the whole ring = its area (device pixels), within flattening error.
        let covered: f64 = pixels.data.as_chunks::<4>().0.iter().map(|p| f64::from(p[3]) / 255.0).sum();
        let (inner, outer) = (sector.inner_radius * 2.0, sector.outer_radius * 2.0);
        let area = std::f64::consts::PI * (outer * outer - inner * inner) * (sector.end - sector.start)
            / std::f64::consts::TAU;
        assert!((covered - area).abs() / area < 3e-4, "covered {covered} vs area {area}");
    }

    #[test]
    fn the_empty_layout_draws_the_placeholder_ring() {
        let (arena, root) = Arena::flat(0, None);
        let layout = Layout::build(&arena, root, None);
        assert!(layout.is_empty());
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(300.), px(300.)));
        let metrics = layers::metrics(bounds);
        let grid = Grid::new(bounds, 2.0).expect("a grid");
        let pixels = layers::render(&layout, &[], &metrics, &grid, &|| false).expect("rendered");
        let raster = Raster::from_premultiplied(pixels, grid).expect("converted");
        let ring = pixel(&raster, polar(&metrics, metrics.inner_radius + 13.0, 0.3));
        assert_eq!(ring, bgra(layers::expected_rgba(theme::white(layers::PLACEHOLDER_ALPHA).into())));
    }

    #[test]
    fn parallel_bands_match_a_single_pass() {
        let (arena, root) = Arena::saturating();
        let layout = Layout::build(&arena, root, None);
        let states: Vec<_> = (0..layout.len())
            .map(|i| match i % 5 {
                0 => CollectionState::Included,
                1 => CollectionState::Partial,
                _ => CollectionState::None,
            })
            .collect();
        let bounds = Bounds::new(point(px(3.5), px(7.25)), size(px(620.), px(580.)));
        let metrics = layers::metrics(bounds);
        let grid = Grid::new(bounds, 2.0).expect("a grid");
        let banded = layers::render(&layout, &states, &metrics, &grid, &|| false).expect("rendered");
        let single = layers::render_unbanded(&layout, &states, &metrics, &grid);
        // Edges crossing a band boundary restart there: float rounding may move a pixel by one
        // 8-bit step, never more.
        let max = banded.data.iter().zip(&single.data).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        assert!(max <= 1, "band seams: max difference {max}");
    }

    #[test]
    fn a_superseded_render_stops_early() {
        let (arena, root) = Arena::saturating();
        let layout = Layout::build(&arena, root, None);
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(300.), px(300.)));
        let grid = Grid::new(bounds, 2.0).expect("a grid");
        assert!(layers::render(&layout, &[], &layers::metrics(bounds), &grid, &|| true).is_none());
    }

    /// GPUI places a sprite at `floor(origin × scale)` with size `ceil(size × scale)` (f32):
    /// [`Grid::paint_bounds`] must land exactly on the raster's pixels, and the grid must cover
    /// the chart.
    #[test]
    fn paint_bounds_land_on_whole_device_pixels() {
        for scale in [1.0f32, 1.25, 1.5, 2.0, 3.0] {
            for origin in [0.0f32, 16.0, 226.3, 1000.77, 2047.5, 3001.1] {
                for extent in [1.0f32, 300.4, 674.0, 1400.0] {
                    let bounds = Bounds::new(point(px(origin), px(origin / 2.0)), size(px(extent), px(extent * 0.7)));
                    let grid = Grid::new(bounds, scale).expect("a grid");
                    let painted = grid.paint_bounds();
                    let (width, height) = grid.image_size();
                    assert_eq!((f32::from(painted.origin.x) * scale).floor() as i64, grid.x, "{scale} {origin}");
                    assert_eq!((f32::from(painted.origin.y) * scale).floor() as i64, grid.y, "{scale} {origin}");
                    assert_eq!((f32::from(painted.size.width) * scale).ceil() as u32, width, "{scale} {extent}");
                    assert_eq!((f32::from(painted.size.height) * scale).ceil() as u32, height, "{scale} {extent}");
                    let s = f64::from(scale);
                    assert!(grid.x as f64 <= f64::from(origin) * s);
                    assert!((grid.x + i64::from(grid.width)) as f64 >= f64::from(origin + extent) * s - 1e-3);
                    assert!(width >= layers::MIN_TEXTURE && height >= layers::MIN_TEXTURE);
                }
            }
        }
    }
}

/// Raster PNGs for visual inspection (`cargo nextest run -p orbit-ui --run-ignored only
/// map_raster_png`): the fixture tree's map at 900 × 900 pt @2x with some collected and partly
/// collected segments, over the canvas color, as `target/map-raster-2x.png`, and a 4× crop of
/// an outer-ring arc edge and gap as `target/map-raster-edge-zoom.png`.
#[gpui::test]
#[ignore = "writes PNGs for inspection"]
fn map_raster_png(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use crate::fixture::{self, FixtureOptions, SharedFixture};
    use crate::host::HostOptions;

    let options = FixtureOptions { mapping_duration: Duration::from_millis(100), ..FixtureOptions::default() };
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
    let (map, cx) = cx.add_window_view({
        let host = host.clone();
        move |_, cx| MapPanel::new(&host, cx)
    });
    cx.update(|_, cx| host.update(cx, |host, cx| host.launch(Some(fixture::ROOT.to_vec()), cx)));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        cx.run_until_parked();
        let ready = cx.read(|cx| {
            let state = host.state(cx);
            state.folder().is_some() && !state.is_scanning() && map.read(cx).presentation().is_some()
        });
        if ready {
            break;
        }
        assert!(Instant::now() < deadline, "the fixture map was not ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The home folder: a fuller map than the volume root.
    cx.update(|_, cx| host.send(Event::Open { path: fixture::HOME.into() }, cx));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        cx.run_until_parked();
        let ready = cx.read(|cx| {
            let state = host.state(cx);
            state.current_path() == Some(fixture::HOME)
                && state.folder().is_some()
                && map.read(cx).presentation().is_some_and(|p| !p.layout.is_empty())
        });
        if ready {
            break;
        }
        let info = cx.read(|cx| {
            let state = host.state(cx);
            let path = String::from_utf8_lossy(state.current_path().unwrap_or_default()).into_owned();
            (path, map.read(cx).presentation().map(|p| p.layout.len()))
        });
        assert!(Instant::now() < deadline, "the fixture home map was not ready: {info:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Collect every 3rd segment of rings 2 and 3 (hatched, check badges; ancestors become partial).
    let paths: Vec<Arc<[u8]>> = cx.read(|cx| {
        let folder = host.state(cx).folder().expect("a folder");
        let presentation = map.read(cx).presentation().expect("a map");
        presentation
            .layout
            .segments()
            .iter()
            .filter(|s| matches!(s.depth, 1 | 2))
            .filter_map(|s| s.node)
            .step_by(3)
            .map(|ix| folder.path(ix).into())
            .collect()
    });
    cx.update(|_, cx| host.send(Event::Collect { paths }, cx));
    cx.run_until_parked();
    let presentation = cx.read(|cx| map.read(cx).presentation().expect("a map").clone());
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    write_raster_pngs(&presentation.layout, &presentation.states, &target, "map-raster");

    // The saturated synthetic map (768 segments, thin gaps everywhere), a third collected.
    let (arena, root) = arena::Arena::saturating();
    let layout = orbit_sunburst::Layout::build(&arena, root, None);
    let states: Vec<_> = (0..layout.len())
        .map(|i| match i % 9 {
            0 => CollectionState::Included,
            4 => CollectionState::Partial,
            _ => CollectionState::None,
        })
        .collect();
    write_raster_pngs(&layout, &states, &target, "map-raster-saturating");
}

/// Writes `<name>-2x.png` (900 × 900 pt at 2x over the canvas color) and `<name>-edge-zoom.png`
/// (a 4× nearest-neighbor crop where two outer-ring segments meet: outer arc edge and gap).
fn write_raster_pngs<Id: Copy + Eq + std::hash::Hash>(
    layout: &orbit_sunburst::Layout<Id>,
    states: &[CollectionState],
    target: &std::path::Path,
    name: &str,
) {
    use gpui::Bounds;
    use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

    use crate::panels::map::layers::Grid;
    use crate::theme;

    let bounds = Bounds::new(point(px(0.), px(0.)), size(px(900.), px(900.)));
    let metrics = layers::metrics(bounds);
    let grid = Grid::new(bounds, 2.0).expect("a grid");
    let raster = layers::render(layout, states, &metrics, &grid, &|| false).expect("rendered");
    let mut png = Pixmap::new(grid.width, grid.height).expect("a pixmap");
    let canvas = theme::color(theme::CANVAS);
    png.fill(tiny_skia::Color::from_rgba(canvas.r, canvas.g, canvas.b, 1.0).expect("a color"));
    let raster =
        Pixmap::from_vec(raster.data, tiny_skia::IntSize::from_wh(raster.width, raster.height).expect("a size"))
            .expect("a pixmap");
    png.draw_pixmap(0, 0, raster.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    let full = target.join(format!("{name}-2x.png"));
    png.save_png(&full).expect("saved");

    let ring = (0..4).rev().map(|depth| layout.ring(depth)).find(|ring| ring.len() > 2).expect("a ring");
    let sector = layout.sector(&metrics, ring.start + ring.len() / 3);
    let radius = sector.outer_radius - 4.0;
    let (x, y) = (
        (metrics.center.x + radius * sector.start.sin()) * 2.0,
        (metrics.center.y - radius * sector.start.cos()) * 2.0,
    );
    let zoom = |x: f64, y: f64, file: &str| {
        let (w, h) = (96.0f32, 64.0f32);
        let mut zoom = Pixmap::new((w * 4.0) as u32, (h * 4.0) as u32).expect("a pixmap");
        let paint = PixmapPaint { quality: FilterQuality::Nearest, ..PixmapPaint::default() };
        let transform =
            Transform::from_scale(4.0, 4.0).pre_translate(-(x as f32 - w / 2.0).round(), -(y as f32 - h / 2.0).round());
        zoom.draw_pixmap(0, 0, png.as_ref(), &paint, transform, None);
        let path = target.join(file);
        zoom.save_png(&path).expect("saved");
        path
    };
    let crop = zoom(x, y, &format!("{name}-edge-zoom.png"));
    // The top of the outermost painted arc: a nearly horizontal edge, where stepping would show.
    let top = layout
        .segments()
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            let sector = layout.sector(&metrics, *index);
            sector.start <= 0.1 && sector.end >= 0.1
        })
        .map(|(index, _)| layout.sector(&metrics, index).outer_radius)
        .fold(0.0, f64::max);
    let arc = zoom(
        (metrics.center.x + top * 0.1f64.sin()) * 2.0,
        (metrics.center.y - top * 0.1f64.cos()) * 2.0,
        &format!("{name}-arc-zoom.png"),
    );
    println!(
        "wrote {}, {} and {} ({} segments, {} collected or partial)",
        full.display(),
        crop.display(),
        arc.display(),
        layout.len(),
        states.iter().filter(|s| **s != CollectionState::None).count()
    );
}

/// Raster build and hover costs (`cargo nextest run -p orbit-ui --release --run-ignored only
/// --no-capture map_measurements`). Prints p50/p99; asserts nothing about time.
#[test]
#[ignore = "measurement, not a test"]
fn map_measurements() {
    measure::run();
}

mod measure {
    use std::time::{Duration, Instant};

    use gpui::{Bounds, point, px, size};
    use orbit_sunburst::{CollectionState, Layout, Point as SPoint};

    use super::arena::Arena;
    use crate::frame_stats::Percentiles;
    use crate::panels::map::layers::{self, Grid, Raster};

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
        let (arena, root) = Arena::saturating();
        let layout = Layout::build(&arena, root, None);
        let none = vec![CollectionState::None; layout.len()];
        let included = vec![CollectionState::Included; layout.len()];
        let mixed: Vec<_> = (0..layout.len())
            .map(|i| match i % 3 {
                0 => CollectionState::Included,
                1 => CollectionState::Partial,
                _ => CollectionState::None,
            })
            .collect();
        for side in [900.0f32, 1400.0] {
            let bounds = Bounds::new(point(px(16.), px(60.)), size(px(side), px(side)));
            let metrics = layers::metrics(bounds);
            let grid = Grid::new(bounds, 2.0).expect("a grid");
            for (name, states) in [("none", &none), ("all included", &included), ("mixed", &mixed)] {
                let render = time(12, || drop(layers::render(&layout, states, &metrics, &grid, &|| false)));
                let total = time(12, || {
                    let pixels = layers::render(&layout, states, &metrics, &grid, &|| false).expect("rendered");
                    drop(Raster::from_premultiplied(pixels, grid));
                });
                let single = time(6, || drop(layers::render_unbanded(&layout, states, &metrics, &grid)));
                let (width, height) = grid.image_size();
                println!(
                    "raster {side}x{side}@2x segments={} collected={name:<12} | render {render} | render+convert {total} \
                     | render on one thread {single} | {width}x{height} px, {:.1} MB",
                    layout.len(),
                    (width as usize * height as usize * 4) as f64 / 1e6,
                );
            }
        }

        // Per pointer move: one hit test, and when the segment changes, its outline (2x).
        let bounds = Bounds::new(point(px(226.), px(100.)), size(px(674.), px(464.)));
        let metrics = layers::metrics(bounds);
        let cases = [
            ("saturating", Arena::saturating()),
            ("flat_1m", Arena::flat(1_000_000, None)),
            ("power_law_1m", Arena::flat(1_000_000, Some(1.1))),
        ];
        for (name, (arena, root)) in &cases {
            let layout = Layout::build(arena, *root, None);
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
            let started = Instant::now();
            let hover = time(5_000, || {
                let point = points.next().expect("cycle");
                if let Some(index) = layout.hit_test(&metrics, point) {
                    drop(layers::outline(&layout, &metrics, index, 2.0));
                }
            });
            let mean = started.elapsed().as_secs_f64() * 1e6 / 5_000.0;
            println!(
                "{name:<13} segments={:<4} hover move (hit test + outline build @2x): {hover} mean={mean:.2} us",
                layout.len()
            );
            // The outline alone, for the largest outer-ring sector (most points).
            let widest = (0..layout.len())
                .max_by(|&a, &b| {
                    let sweep = |i: usize| layout.sector(&metrics, i).end - layout.sector(&metrics, i).start;
                    sweep(a).total_cmp(&sweep(b))
                })
                .expect("a segment");
            let outline = time(2_000, || drop(layers::outline(&layout, &metrics, widest, 2.0)));
            println!("{name:<13} widest segment outline build @2x: {outline}");
        }
    }
}
