//! Chart drawing layers, built from a [`Layout`] and [`Metrics`] into GPUI [`Path`]s.
//!
//! - [`Fills`]: one merged fill path per paint color (≤ 33 colors), so a saturated 768-segment map
//!   is a few dozen primitives. Keyed by (layout revision, bounds): rebuilt for a new folder
//!   revision or a resize, never for hover or collection changes.
//! - [`Marks`]: collection hatching (dark diagonals, 8 pt spacing, 1.5 pt), check badges
//!   (16 pt) and minus badges (14 pt). Keyed additionally by the presentation revision, so a
//!   collection change rebuilds only these. Badges are skipped when they do not fit; hatching stays.
//! - [`outline`]: the 2 pt white stroke of one segment (hover, pinned group, selection).
//!
//! Everything here is pure geometry: no window, no state. The panel caches the results and only
//! clones the paths into the scene while painting.

use gpui::{Bounds, FillOptions, Hsla, Path, PathBuilder, PathStyle, Pixels, point, px};
use orbit_sunburst::{CollectionState, HATCH_SPACING, Layout, LineSegment, Metrics, Point as SPoint, SegmentColor};

use crate::theme;

/// Maximum chord deviation (points) when flattening arcs.
pub const FLATTEN_TOLERANCE: f64 = 0.25;
pub const OUTLINE_WIDTH: f32 = 2.0;
/// Outline color: white at 95 % (Swift `.white.opacity(0.95)`).
pub const OUTLINE_ALPHA: f32 = 0.95;
pub const HATCH_WIDTH: f32 = 1.5;
pub const INCLUDED_BADGE: f64 = 16.0;
pub const PARTIAL_BADGE: f64 = 14.0;
const GLYPH_WIDTH: f32 = 1.8;
const BADGE_SIDES: usize = 24;
/// Placeholder ring (no data): radius `inner + 13`, 18 pt wide, white at 12 %.
const PLACEHOLDER_OFFSET: f64 = 13.0;
const PLACEHOLDER_WIDTH: f32 = 18.0;
pub const PLACEHOLDER_ALPHA: f32 = 0.12;
/// GPUI tessellates into `u16`-indexed buffers: merged paths stay well below 65,536 vertices.
const MAX_BATCH_VERTICES: usize = 24_000;
/// Estimated tessellated vertices per input point.
const FILL_VERTICES_PER_POINT: usize = 1;
const STROKE_VERTICES_PER_POINT: usize = 4;

/// Chart metrics for a canvas laid out at `bounds` (window coordinates).
pub fn metrics(bounds: Bounds<Pixels>) -> Metrics {
    Metrics::from_bounds(
        f64::from(bounds.origin.x),
        f64::from(bounds.origin.y),
        f64::from(bounds.size.width),
        f64::from(bounds.size.height),
    )
}

#[inline]
fn gp(p: SPoint) -> gpui::Point<Pixels> {
    point(px(p.x as f32), px(p.y as f32))
}

fn fill_builder() -> PathBuilder {
    // Non-zero: sectors share one orientation, so bands that overlap on tiny charts union instead
    // of cancelling.
    PathBuilder::fill().with_style(PathStyle::Fill(FillOptions::non_zero()))
}

/// Accumulates sub-paths into one builder and starts another before the vertex budget is hit.
struct Batch {
    make: fn() -> PathBuilder,
    builder: PathBuilder,
    vertices: usize,
    out: Vec<Path<Pixels>>,
}

impl Batch {
    fn new(make: fn() -> PathBuilder) -> Self {
        Self { make, builder: make(), vertices: 0, out: Vec::new() }
    }

    fn reserve(&mut self, vertices: usize) {
        if self.vertices > 0 && self.vertices + vertices > MAX_BATCH_VERTICES {
            self.flush();
        }
        self.vertices += vertices;
    }

    fn polygon(&mut self, points: &[gpui::Point<Pixels>], per_point: usize) {
        if points.len() < 3 {
            return;
        }
        self.reserve(points.len() * per_point);
        self.builder.add_polygon(points, true);
    }

    fn polyline(&mut self, points: &[gpui::Point<Pixels>]) {
        let Some((first, rest)) = points.split_first() else { return };
        self.reserve(points.len() * STROKE_VERTICES_PER_POINT);
        self.builder.move_to(*first);
        for point in rest {
            self.builder.line_to(*point);
        }
    }

    fn flush(&mut self) {
        let builder = std::mem::replace(&mut self.builder, (self.make)());
        if self.vertices > 0
            && let Ok(path) = builder.build()
        {
            self.out.push(path);
        }
        self.vertices = 0;
    }

    fn finish(mut self) -> Vec<Path<Pixels>> {
        self.flush();
        self.out
    }
}

/// Merged fill paths with their colors, plus the placeholder ring when the layout is empty.
pub struct Fills {
    pub paths: Vec<(Path<Pixels>, Hsla)>,
}

impl Fills {
    pub fn build<Id: Copy + Eq + std::hash::Hash>(layout: &Layout<Id>, metrics: &Metrics) -> Self {
        if layout.is_empty() {
            return Self { paths: placeholder_ring(metrics) };
        }
        let mut batches: Vec<(SegmentColor, Batch)> = Vec::new();
        let mut flat = Vec::new();
        let mut points = Vec::new();
        for segment in layout.segments() {
            flat.clear();
            segment.sector(metrics).flatten(FLATTEN_TOLERANCE, &mut flat);
            points.clear();
            points.extend(flat.iter().copied().map(gp));
            let color = segment.color();
            let batch = match batches.iter().position(|(c, _)| *c == color) {
                Some(index) => &mut batches[index].1,
                None => {
                    batches.push((color, Batch::new(fill_builder)));
                    &mut batches.last_mut().expect("just pushed").1
                }
            };
            batch.polygon(&points, FILL_VERTICES_PER_POINT);
        }
        let paths = batches
            .into_iter()
            .flat_map(|(color, batch)| {
                let color = theme::segment_color(color);
                batch.finish().into_iter().map(move |path| (path, color))
            })
            .collect();
        Self { paths }
    }
}

fn placeholder_ring(metrics: &Metrics) -> Vec<(Path<Pixels>, Hsla)> {
    let radius = metrics.inner_radius + PLACEHOLDER_OFFSET;
    let points = circle(metrics.center, radius, 96);
    let mut builder = PathBuilder::stroke(px(PLACEHOLDER_WIDTH));
    builder.add_polygon(&points, true);
    builder.build().map(|path| vec![(path, theme::white(PLACEHOLDER_ALPHA).into())]).unwrap_or_default()
}

fn circle(center: SPoint, radius: f64, sides: usize) -> Vec<gpui::Point<Pixels>> {
    (0..sides)
        .map(|i| {
            let angle = std::f64::consts::TAU * i as f64 / sides as f64;
            gp(SPoint::new(center.x + radius * angle.cos(), center.y + radius * angle.sin()))
        })
        .collect()
}

/// Collection marks: hatching over included segments, badge discs and their glyphs.
#[derive(Default)]
pub struct Marks {
    pub hatch: Vec<Path<Pixels>>,
    pub badges: Vec<Path<Pixels>>,
    pub glyphs: Vec<Path<Pixels>>,
}

impl Marks {
    pub fn build<Id: Copy + Eq + std::hash::Hash>(
        layout: &Layout<Id>,
        states: &[CollectionState],
        metrics: &Metrics,
    ) -> Self {
        if states.iter().all(|state| *state == CollectionState::None) {
            return Self::default();
        }
        let mut hatch = Batch::new(|| PathBuilder::stroke(px(HATCH_WIDTH)));
        let mut badges = Batch::new(fill_builder);
        let mut glyphs = Batch::new(|| PathBuilder::stroke(px(GLYPH_WIDTH)));
        let mut lines: Vec<LineSegment> = Vec::new();
        for (segment, state) in layout.segments().iter().zip(states) {
            let diameter = match state {
                CollectionState::None => continue,
                CollectionState::Included => INCLUDED_BADGE,
                CollectionState::Partial => PARTIAL_BADGE,
            };
            let sector = segment.sector(metrics);
            if *state == CollectionState::Included {
                lines.clear();
                sector.hatch(HATCH_SPACING, &mut lines);
                for line in &lines {
                    hatch.polyline(&[gp(line.from), gp(line.to)]);
                }
            }
            let Some(center) = sector.badge_center(diameter) else { continue };
            let radius = diameter / 2.0;
            badges.polygon(&circle(center, radius, BADGE_SIDES), FILL_VERTICES_PER_POINT);
            let at = |dx: f64, dy: f64| gp(SPoint::new(center.x + radius * dx, center.y + radius * dy));
            if *state == CollectionState::Included {
                glyphs.polyline(&[at(-0.45, 0.0), at(-0.1, 0.35), at(0.48, -0.35)]);
            } else {
                glyphs.polyline(&[at(-0.42, 0.0), at(0.42, 0.0)]);
            }
        }
        Self { hatch: hatch.finish(), badges: badges.finish(), glyphs: glyphs.finish() }
    }

    pub fn is_empty(&self) -> bool {
        self.hatch.is_empty() && self.badges.is_empty()
    }
}

/// The 2 pt outline of segment `index`, or `None` for an unknown index.
pub fn outline<Id: Copy + Eq + std::hash::Hash>(
    layout: &Layout<Id>,
    metrics: &Metrics,
    index: usize,
) -> Option<Path<Pixels>> {
    let segment = layout.segments().get(index)?;
    let mut flat = Vec::new();
    segment.sector(metrics).flatten(FLATTEN_TOLERANCE, &mut flat);
    let points: Vec<_> = flat.into_iter().map(gp).collect();
    let mut builder = PathBuilder::stroke(px(OUTLINE_WIDTH));
    builder.add_polygon(&points, true);
    builder.build().ok()
}
