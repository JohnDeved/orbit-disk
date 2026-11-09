//! Frame-time spike: paints a saturated (768-segment) sunburst with collection hatching and a
//! hover outline that sweeps every frame, then prints frame statistics and quits.
//!
//! `cargo run -p orbit-ui --example sunburst_spike --release`
//!
//! What is measured per frame: the time spent inside our canvas prepaint+paint (CPU work we
//! own) and the interval between consecutive frames (what the user sees).

use gpui::{
    App, Application, Bounds, Context, Hsla, Path, PathBuilder, Pixels, Render, Window, WindowBounds, WindowOptions,
    canvas, div, point, prelude::*, px, size,
};
use orbit_sunburst::{Hit, Layout, LayoutSource, Metrics, Point as SPoint, SegmentColor};
use std::time::{Duration, Instant};

const FRAMES: usize = 600;

/// A deterministic power-law tree large enough to hit the 768-segment cap.
struct Tree {
    sizes: Vec<u64>,
    children: Vec<Vec<u32>>,
}

impl Tree {
    fn generate() -> Self {
        let mut tree = Tree { sizes: vec![0], children: vec![vec![]] };
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        fn build(tree: &mut Tree, parent: u32, depth: u32, next: &mut impl FnMut() -> u64) -> u64 {
            let count = if depth >= 4 { 0 } else { 12 + (next() % 40) as u32 };
            let mut total = 0;
            for i in 0..count {
                let id = tree.sizes.len() as u32;
                tree.sizes.push(0);
                tree.children.push(vec![]);
                tree.children[parent as usize].push(id);
                let size = if depth >= 3 || next().is_multiple_of(3) {
                    1_000_000_000 / u64::from(i + 1) + next() % 1000
                } else {
                    build(tree, id, depth + 1, next)
                };
                tree.sizes[id as usize] = size;
                total += size;
            }
            let mut kids = std::mem::take(&mut tree.children[parent as usize]);
            kids.sort_by_key(|k| std::cmp::Reverse(tree.sizes[*k as usize]));
            tree.children[parent as usize] = kids;
            total
        }
        let total = build(&mut tree, 0, 0, &mut next);
        tree.sizes[0] = total;
        tree
    }
}

impl LayoutSource for Tree {
    type Id = u32;
    fn size(&self, id: u32) -> u64 {
        self.sizes[id as usize]
    }
    fn children(&self, id: u32) -> &[u32] {
        &self.children[id as usize]
    }
}

fn color(c: SegmentColor) -> Hsla {
    let h = c.hsla();
    Hsla { h: h.h, s: h.s, l: h.l, a: h.a }
}

fn gp(p: SPoint) -> gpui::Point<Pixels> {
    point(px(p.x as f32), px(p.y as f32))
}

/// Geometry cached per (layout, bounds): rebuilt only on resize or new data, never on hover.
struct Prepared {
    bounds: Bounds<Pixels>,
    metrics: Metrics,
    fills: Vec<(Path<Pixels>, Hsla)>,
    hatches: Vec<Path<Pixels>>,
    outlines: Vec<Vec<gpui::Point<Pixels>>>,
}

struct Spike {
    layout: Layout<u32>,
    prepared: Option<Prepared>,
    frame: usize,
    started: Instant,
    last_frame: Option<Instant>,
    intervals: Vec<Duration>,
    work: Vec<Duration>,
    rebuilds: usize,
}

impl Spike {
    fn prepare(&mut self, bounds: Bounds<Pixels>) {
        if self.prepared.as_ref().is_some_and(|p| p.bounds == bounds) {
            return;
        }
        self.rebuilds += 1;
        let metrics = Metrics::from_bounds(
            f64::from(bounds.origin.x),
            f64::from(bounds.origin.y),
            f64::from(bounds.size.width),
            f64::from(bounds.size.height),
        );
        let mut fills = Vec::with_capacity(self.layout.len());
        let mut hatches = Vec::new();
        let mut outlines = Vec::with_capacity(self.layout.len());
        let (mut poly, mut lines) = (Vec::new(), Vec::new());
        for (i, segment) in self.layout.segments().iter().enumerate() {
            let sector = segment.sector(&metrics);
            poly.clear();
            sector.flatten(0.25, &mut poly);
            let points: Vec<_> = poly.iter().copied().map(gp).collect();
            let mut builder = PathBuilder::fill();
            builder.add_polygon(&points, true);
            if let Ok(path) = builder.build() {
                fills.push((path, color(segment.color())));
            }
            outlines.push(points);
            // Pretend every 5th segment is collected.
            if i.is_multiple_of(5) {
                lines.clear();
                sector.hatch(orbit_sunburst::HATCH_SPACING, &mut lines);
                if !lines.is_empty() {
                    let mut builder = PathBuilder::stroke(px(1.5));
                    for line in &lines {
                        builder.move_to(gp(line.from));
                        builder.line_to(gp(line.to));
                    }
                    if let Ok(path) = builder.build() {
                        hatches.push(path);
                    }
                }
            }
        }
        self.prepared = Some(Prepared { bounds, metrics, fills, hatches, outlines });
    }

    fn paint(&mut self, window: &mut Window) {
        let t0 = Instant::now();
        let Some(prepared) = &self.prepared else { return };
        for (path, color) in &prepared.fills {
            window.paint_path(path.clone(), *color);
        }
        let hatch = Hsla { h: 0.0, s: 0.0, l: 0.08, a: 0.55 };
        for path in &prepared.hatches {
            window.paint_path(path.clone(), hatch);
        }
        // Simulated pointer sweeping around the rings; analytic hit test each frame.
        let m = &prepared.metrics;
        let angle = self.frame as f64 * 0.05;
        let radius = m.inner_radius + m.ring_width * (0.5 + (self.frame / 40 % 4) as f64);
        let pointer = SPoint::new(m.center.x + radius * angle.sin(), m.center.y - radius * angle.cos());
        if let Hit::Segment(index) = self.layout.hit(m, pointer) {
            let mut builder = PathBuilder::stroke(px(2.0));
            builder.add_polygon(&prepared.outlines[index], true);
            if let Ok(path) = builder.build() {
                window.paint_path(path, gpui::white());
            }
        }
        self.work.push(t0.elapsed());
    }
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        if let Some(last) = self.last_frame.replace(now) {
            self.intervals.push(now - last);
        }
        self.frame += 1;
        if self.frame > FRAMES {
            report(self);
            cx.quit();
        }
        window.request_animation_frame();
        let entity = cx.entity();
        div().size_full().bg(gpui::rgb(0x1c1d21)).child(
            canvas(
                {
                    let entity = entity.clone();
                    move |bounds, _window, cx: &mut App| entity.update(cx, |spike, _| spike.prepare(bounds))
                },
                move |_bounds, (), window, cx| entity.update(cx, |spike, _| spike.paint(window)),
            )
            .size_full(),
        )
    }
}

fn percentile(values: &mut [Duration], p: f64) -> f64 {
    values.sort();
    let i = ((values.len() as f64 - 1.0) * p).round() as usize;
    values[i].as_secs_f64() * 1000.0
}

fn report(spike: &mut Spike) {
    let mut work = spike.work.clone();
    let mut intervals = spike.intervals[10..].to_vec(); // skip warm-up
    println!(
        "segments={} frames={} geometry_rebuilds={} elapsed={:.2}s",
        spike.layout.len(),
        spike.frame,
        spike.rebuilds,
        spike.started.elapsed().as_secs_f64()
    );
    println!(
        "paint work ms: p50={:.3} p95={:.3} p99={:.3} max={:.3}",
        percentile(&mut work, 0.5),
        percentile(&mut work, 0.95),
        percentile(&mut work, 0.99),
        percentile(&mut work, 1.0)
    );
    println!(
        "frame interval ms: p50={:.3} p95={:.3} p99={:.3} max={:.3}",
        percentile(&mut intervals, 0.5),
        percentile(&mut intervals, 0.95),
        percentile(&mut intervals, 0.99),
        percentile(&mut intervals, 1.0)
    );
}

fn main() {
    let tree = Tree::generate();
    let t = Instant::now();
    let layout = Layout::build(&tree, 0, None);
    println!("layout: {} nodes -> {} segments in {:?}", tree.sizes.len(), layout.len(), t.elapsed());
    Application::new().run(move |cx| {
        let bounds = Bounds::centered(None, size(px(1280.), px(810.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), focus: true, ..Default::default() },
            |_, cx| {
                cx.new(|_| Spike {
                    layout,
                    prepared: None,
                    frame: 0,
                    started: Instant::now(),
                    last_frame: None,
                    intervals: Vec::new(),
                    work: Vec::new(),
                    rebuilds: 0,
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
