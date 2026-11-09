//! Chart drawing layers, built from a [`Layout`] and [`Metrics`].
//!
//! - [`render`] / [`Raster`]: the BASE layer (segment fills, collection hatching, check and minus
//!   badges, the empty placeholder ring) rasterized on the CPU at exact device resolution with
//!   exact-area anti-aliasing ([`super::coverage`]). GPUI's own path rasterizer uses 4× MSAA on
//!   geometry flattened in points, which leaves long gentle arcs and the thin gaps between
//!   segments faceted and stair-stepped compared with Core Graphics; this raster, sampled 1:1 by
//!   the GPU, is not. Built off the UI thread (see the panel), keyed by layout, presentation,
//!   bounds and scale.
//! - [`outline`]: the 2 pt white stroke of one segment (hover, pinned group, selection), still a
//!   GPUI path (it changes per hover), flattened to [`OUTLINE_TOLERANCE`] device pixels.
//!
//! Everything here is pure geometry and pixels: no window, no state.
//!
//! # Pixel grid
//!
//! [`Grid`] snaps the chart bounds outwards to whole device pixels and maps window points into
//! that grid (`device = logical × scale − grid origin`). [`Grid::paint_bounds`] gives logical
//! bounds that GPUI's sprite placement (`floor(origin × scale)`, `ceil(size × scale)`) turns back
//! into exactly that pixel rectangle, so every texel lands on one device pixel (no resampling).
//!
//! # Colors and texture lifetime
//!
//! Colors are the GPUI colors the path renderer used (`Rgba::from(Hsla)`, rounded to 8-bit sRGB
//! like the Metal `BGRA8Unorm` target). Layers composite in premultiplied 8-bit sRGB, as Core
//! Graphics and GPUI's path compositing do; [`Raster::from_premultiplied`] converts to the
//! straight (non-premultiplied) BGRA that GPUI's polychrome sprite pipeline blends with
//! `SourceAlpha, OneMinusSourceAlpha`.
//!
//! GPUI's atlas frees a texture only once every tile in it is removed, and never reuses the space
//! of a removed tile while the texture lives. Images are therefore padded (transparent) to at
//! least the atlas page size ([`MIN_TEXTURE`]) so each raster owns its texture outright and
//! `Window::drop_image` releases it.

use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::sync::Arc;

use gpui::{Bounds, Path, PathBuilder, Pixels, RenderImage, Rgba, point, px, size};
use image::{Frame, RgbaImage};
use orbit_sunburst::{ArcParams, CollectionState, HATCH_SPACING, Layout, Metrics, Point as SPoint, Sector};

use super::coverage::{self, Layer, Polygons};
use crate::theme;

/// Maximum chord deviation (device pixels) of the base raster's arcs.
pub const RASTER_TOLERANCE: f64 = 0.05;
/// Maximum chord deviation (device pixels) of the hover and selection outlines.
pub const OUTLINE_TOLERANCE: f64 = 0.1;
pub const OUTLINE_WIDTH: f32 = 2.0;
/// Outline color: white at 95 % (Swift `.white.opacity(0.95)`).
pub const OUTLINE_ALPHA: f32 = 0.95;
pub const HATCH_WIDTH: f64 = 1.5;
pub const INCLUDED_BADGE: f64 = 16.0;
pub const PARTIAL_BADGE: f64 = 14.0;
const GLYPH_WIDTH: f64 = 1.8;
/// Placeholder ring (no data): radius `inner + 13`, 18 pt wide, white at 12 %.
const PLACEHOLDER_OFFSET: f64 = 13.0;
const PLACEHOLDER_WIDTH: f64 = 18.0;
pub const PLACEHOLDER_ALPHA: f32 = 0.12;
/// GPUI's default atlas page: smaller images share pages (see the module docs).
pub const MIN_TEXTURE: u32 = 1024;
/// Largest Metal texture side.
const MAX_TEXTURE: u32 = 16_384;
/// Row bands drawn in parallel; more bands than threads balance the busy middle rows.
const BANDS_PER_THREAD: usize = 3;
const MAX_THREADS: usize = 8;

/// Chart metrics for a canvas laid out at `bounds` (window coordinates).
pub fn metrics(bounds: Bounds<Pixels>) -> Metrics {
    Metrics::from_bounds(
        f64::from(bounds.origin.x),
        f64::from(bounds.origin.y),
        f64::from(bounds.size.width),
        f64::from(bounds.size.height),
    )
}

/// The device-pixel rectangle covering some logical bounds at a scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    /// Device-pixel origin in the window.
    pub x: i64,
    pub y: i64,
    /// Device pixels drawn (the image may be padded beyond, see [`Grid::image_size`]).
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

impl Grid {
    /// The smallest device-pixel rectangle containing `bounds`; `None` when empty or too large.
    pub fn new(bounds: Bounds<Pixels>, scale: f32) -> Option<Self> {
        let s = f64::from(scale);
        if !(s.is_finite() && s > 0.0) {
            return None;
        }
        let (x, y) = (f64::from(bounds.origin.x), f64::from(bounds.origin.y));
        let (w, h) = (f64::from(bounds.size.width), f64::from(bounds.size.height));
        let (x0, y0) = ((x * s).floor(), (y * s).floor());
        let (x1, y1) = (((x + w) * s).ceil(), ((y + h) * s).ceil());
        let (width, height) = (x1 - x0, y1 - y0);
        let fits = |side: f64| side >= 1.0 && side <= f64::from(MAX_TEXTURE);
        (fits(width) && fits(height)).then_some(Self {
            x: x0 as i64,
            y: y0 as i64,
            width: width as u32,
            height: height as u32,
            scale,
        })
    }

    /// Image size: the drawn area, padded to a whole atlas page.
    pub fn image_size(&self) -> (u32, u32) {
        (self.width.max(MIN_TEXTURE), self.height.max(MIN_TEXTURE))
    }

    /// A window point in raster pixels.
    #[inline]
    fn map(&self, p: SPoint) -> [f32; 2] {
        let s = f64::from(self.scale);
        [(p.x * s - self.x as f64) as f32, (p.y * s - self.y as f64) as f32]
    }

    /// Logical bounds of the whole image that GPUI places exactly on this grid (see the module
    /// docs): the origin nudged in and the size nudged down by 1/64 px so that its `floor` and
    /// `ceil` land on the intended whole pixels despite `f32` rounding.
    pub fn paint_bounds(&self) -> Bounds<Pixels> {
        const NUDGE: f32 = 1.0 / 64.0;
        let (width, height) = self.image_size();
        let s = self.scale;
        Bounds::new(
            point(px((self.x as f32 + NUDGE) / s), px((self.y as f32 + NUDGE) / s)),
            size(px((width as f32 - NUDGE) / s), px((height as f32 - NUDGE) / s)),
        )
    }
}

/// 8-bit sRGB of a GPUI color, as the Metal target stores it.
fn rgba8(color: impl Into<Rgba>) -> [u8; 4] {
    let c: Rgba = color.into();
    [c.r, c.g, c.b, c.a].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Builds polygons in raster pixels from window points. All shapes wind clockwise on screen
/// (increasing screen angles), so overlapping shapes of one layer union.
struct Shapes<'a> {
    grid: &'a Grid,
    /// Flattening tolerance in points.
    tolerance: f64,
    polygons: Polygons,
}

impl<'a> Shapes<'a> {
    fn new(grid: &'a Grid) -> Self {
        Self { grid, tolerance: RASTER_TOLERANCE / f64::from(grid.scale), polygons: Polygons::new() }
    }

    fn polygon(&mut self, points: &[SPoint]) {
        self.polygons.push(points.iter().map(|p| self.grid.map(*p)));
    }

    /// The flattened outline of `sector` (window points) into `out`.
    fn flatten_sector(&self, sector: &Sector, out: &mut Vec<SPoint>) {
        out.clear();
        sector.flatten(self.tolerance, out);
    }

    /// Appends the arc from `start` to `end` (screen angles) around `center` to `out`.
    fn arc(&self, center: SPoint, radius: f64, start: f64, end: f64, out: &mut Vec<SPoint>) {
        ArcParams { center, radius, start_angle: start, end_angle: end }.flatten(self.tolerance, out);
    }

    fn circle(&mut self, center: SPoint, radius: f64, scratch: &mut Vec<SPoint>) {
        scratch.clear();
        self.arc(center, radius, 0.0, TAU, scratch);
        scratch.pop(); // The end repeats the start.
        self.polygon(scratch);
    }

    /// A ring of `width` around a circle: the outer circle clockwise, the inner one back.
    fn ring(&mut self, center: SPoint, radius: f64, width: f64, scratch: &mut Vec<SPoint>) {
        let map = |p: &SPoint| self.grid.map(*p);
        scratch.clear();
        self.arc(center, radius + width / 2.0, 0.0, TAU, scratch);
        let outer: Vec<_> = scratch.iter().map(map).collect();
        scratch.clear();
        self.arc(center, radius - width / 2.0, TAU, 0.0, scratch);
        let inner: Vec<_> = scratch.iter().map(map).collect();
        // One polygon: the seam edges at angle 0 cancel out.
        self.polygons.push(outer.into_iter().chain(inner));
    }

    /// A stroked polyline of `width` with round caps and joins: one capsule per piece.
    fn round_line(&mut self, points: &[SPoint], width: f64, scratch: &mut Vec<SPoint>) {
        let radius = width / 2.0;
        for pair in points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let direction = (b.y - a.y).atan2(b.x - a.x);
            scratch.clear();
            self.arc(b, radius, direction - FRAC_PI_2, direction + FRAC_PI_2, scratch);
            self.arc(a, radius, direction + FRAC_PI_2, direction + FRAC_PI_2 + PI, scratch);
            self.polygon(scratch);
        }
    }

    fn take(&mut self) -> Polygons {
        std::mem::replace(&mut self.polygons, Polygons::new())
    }
}

/// Keeps the part of polygon `input` where `f(p) ≥ 0` (one Sutherland–Hodgman pass; `f` affine).
fn clip_half(input: &[SPoint], f: impl Fn(SPoint) -> f64, out: &mut Vec<SPoint>) {
    out.clear();
    let Some(&last) = input.last() else { return };
    let (mut a, mut fa) = (last, f(last));
    for &b in input {
        let fb = f(b);
        if (fa >= 0.0) != (fb >= 0.0) {
            let t = fa / (fa - fb);
            out.push(SPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
        }
        if fb >= 0.0 {
            out.push(b);
        }
        (a, fa) = (b, fb);
    }
}

/// Hatch stripes of `sector` (outline `polygon`): the diagonals `x − y = c` of the Swift renderer
/// (starting at `bounds.min.x − bounds.height`, every [`HATCH_SPACING`] points horizontally),
/// [`HATCH_WIDTH`] wide and clipped exactly to the sector (Swift `clip(to: path)`), as polygons.
fn hatch(sector: &Sector, polygon: &[SPoint], shapes: &mut Shapes, scratch: &mut [Vec<SPoint>; 2]) {
    let bounds = sector.bounds();
    let first = bounds.min.x - (bounds.max.y - bounds.min.y);
    let lines = ((bounds.max.x - first) / HATCH_SPACING).floor();
    if !(lines.is_finite() && lines >= 0.0) || polygon.len() < 3 {
        return;
    }
    // In `u = x − y`, a stripe of width w around `u = c` spans `c ± w·√2/2`.
    let half = HATCH_WIDTH * std::f64::consts::FRAC_1_SQRT_2;
    let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in polygon {
        low = low.min(p.x - p.y);
        high = high.max(p.x - p.y);
    }
    let base = first - bounds.min.y;
    let from = ((low - half - base) / HATCH_SPACING).ceil().max(0.0);
    let to = ((high + half - base) / HATCH_SPACING).floor().min(lines);
    let [inside, stripe] = scratch;
    let mut k = from;
    while k <= to {
        let c = base + k * HATCH_SPACING;
        clip_half(polygon, |p| p.x - p.y - (c - half), inside);
        clip_half(inside, |p| c + half - (p.x - p.y), stripe);
        shapes.polygon(stripe);
        k += 1.0;
    }
}

/// The base layer as polygon layers in raster pixels, in paint order.
fn scene<Id: Copy + Eq + std::hash::Hash>(
    layout: &Layout<Id>,
    states: &[CollectionState],
    metrics: &Metrics,
    grid: &Grid,
) -> Vec<Layer> {
    let mut layers = Vec::new();
    let mut shapes = Shapes::new(grid);
    let mut outline = Vec::new();
    if layout.is_empty() {
        shapes.ring(metrics.center, metrics.inner_radius + PLACEHOLDER_OFFSET, PLACEHOLDER_WIDTH, &mut outline);
        layers.push(Layer { color: rgba8(theme::white(PLACEHOLDER_ALPHA)), shapes: shapes.take() });
        return layers;
    }

    // Fills, one layer per paint color (≤ 33). Sectors never overlap except where tiny charts
    // give bands a 1 pt minimum width; those union.
    let mut colors: Vec<(orbit_sunburst::SegmentColor, Vec<usize>)> = Vec::new();
    for (index, segment) in layout.segments().iter().enumerate() {
        let color = segment.color();
        match colors.iter_mut().find(|(c, _)| *c == color) {
            Some((_, members)) => members.push(index),
            None => colors.push((color, vec![index])),
        }
    }
    for (color, members) in &colors {
        for &index in members {
            shapes.flatten_sector(&layout.sector(metrics, index), &mut outline);
            shapes.polygon(&outline);
        }
        layers.push(Layer { color: rgba8(theme::segment_color(*color)), shapes: shapes.take() });
    }
    if states.iter().all(|state| *state == CollectionState::None) {
        return layers;
    }

    // One ink layer: hatching of included sectors and the badge discs (16 pt check, 14 pt
    // minus) over it; then the white glyphs (round caps and joins).
    let mut scratch = [Vec::new(), Vec::new()];
    let mut glyphs = Shapes::new(grid);
    for (index, state) in states.iter().enumerate().take(layout.len()) {
        if *state == CollectionState::Included {
            let sector = layout.sector(metrics, index);
            shapes.flatten_sector(&sector, &mut outline);
            hatch(&sector, &outline, &mut shapes, &mut scratch);
        }
    }
    for (index, state) in states.iter().enumerate().take(layout.len()) {
        let diameter = match state {
            CollectionState::None => continue,
            CollectionState::Included => INCLUDED_BADGE,
            CollectionState::Partial => PARTIAL_BADGE,
        };
        let Some(center) = layout.sector(metrics, index).badge_center(diameter) else { continue };
        let radius = diameter / 2.0;
        shapes.circle(center, radius, &mut outline);
        let at = |dx: f64, dy: f64| SPoint::new(center.x + radius * dx, center.y + radius * dy);
        if *state == CollectionState::Included {
            glyphs.round_line(&[at(-0.45, 0.0), at(-0.1, 0.35), at(0.48, -0.35)], GLYPH_WIDTH, &mut outline);
        } else {
            glyphs.round_line(&[at(-0.42, 0.0), at(0.42, 0.0)], GLYPH_WIDTH, &mut outline);
        }
    }
    layers.push(Layer { color: rgba8(theme::color(theme::INK)), shapes: shapes.take() });
    layers.push(Layer { color: rgba8(theme::color(theme::TEXT)), shapes: glyphs.take() });
    layers.retain(|layer| !layer.shapes.is_empty());
    layers
}

/// Runs `job(rows, first row)` over row bands of `data` (rows of `width` RGBA pixels) on up to
/// [`MAX_THREADS`] scoped threads.
fn for_each_band(data: &mut [u8], width: usize, job: impl Fn(&mut [u8], usize) + Sync) {
    let row = width * 4;
    let rows = data.len() / row.max(1);
    if rows == 0 {
        return;
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, MAX_THREADS);
    let band_rows = rows.div_ceil(threads * BANDS_PER_THREAD).max(1);
    let queue = std::sync::Mutex::new(data.chunks_mut(band_rows * row).enumerate());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().map(|mut bands| bands.next()).unwrap_or(None);
                    let Some((index, rows)) = next else { break };
                    job(rows, index * band_rows);
                }
            });
        }
    });
}

/// Premultiplied RGBA pixels of [`Grid::image_size`].
pub struct Pixels8 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Draws the base layer of `layout` with collection `states` at `metrics` in parallel row bands.
/// Returns `None` when `cancelled` turns true (a newer job superseded this one).
pub fn render<Id: Copy + Eq + std::hash::Hash>(
    layout: &Layout<Id>,
    states: &[CollectionState],
    metrics: &Metrics,
    grid: &Grid,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Pixels8> {
    let layers = scene(layout, states, metrics, grid);
    if cancelled() {
        return None;
    }
    let (width, height) = grid.image_size();
    let mut data = vec![0u8; width as usize * height as usize * 4];
    // Only the drawn rows; the rest is padding.
    let drawn = grid.height as usize * width as usize * 4;
    for_each_band(&mut data[..drawn], width as usize, |rows, top| {
        coverage::draw(&layers, rows, width as usize, top, cancelled);
    });
    (!cancelled()).then_some(Pixels8 { width, height, data })
}

/// [`render`] as one band on the calling thread (tests compare the two for seams).
#[cfg(test)]
pub fn render_unbanded<Id: Copy + Eq + std::hash::Hash>(
    layout: &Layout<Id>,
    states: &[CollectionState],
    metrics: &Metrics,
    grid: &Grid,
) -> Pixels8 {
    let layers = scene(layout, states, metrics, grid);
    let (width, height) = grid.image_size();
    let mut data = vec![0u8; width as usize * height as usize * 4];
    coverage::draw(&layers, &mut data, width as usize, 0, &|| false);
    Pixels8 { width, height, data }
}

/// A finished base layer, ready for `Window::paint_image`.
pub struct Raster {
    pub grid: Grid,
    pub image: Arc<RenderImage>,
}

impl Raster {
    /// Converts premultiplied RGBA pixels of `grid` into GPUI's straight-alpha BGRA, in place.
    pub fn from_premultiplied(pixels: Pixels8, grid: Grid) -> Option<Self> {
        let Pixels8 { width, height, mut data } = pixels;
        let drawn = grid.height as usize * width as usize * 4;
        for_each_band(&mut data[..drawn], width as usize, |rows, _| {
            let (pixels, _) = rows.as_chunks_mut::<4>();
            for pixel in pixels {
                match pixel[3] {
                    0 => {}
                    255 => pixel.swap(0, 2),
                    alpha => {
                        let straight =
                            |c: u8| ((u32::from(c) * 255 + u32::from(alpha) / 2) / u32::from(alpha)).min(255) as u8;
                        *pixel = [straight(pixel[2]), straight(pixel[1]), straight(pixel[0]), alpha];
                    }
                }
            }
        });
        let buffer = RgbaImage::from_raw(width, height, data)?;
        Some(Self { grid, image: Arc::new(RenderImage::new(vec![Frame::new(buffer)])) })
    }

    /// Bytes held by the image (the GPU texture holds as many again once uploaded).
    pub fn bytes(&self) -> usize {
        let (width, height) = self.grid.image_size();
        width as usize * height as usize * 4
    }
}

/// The 2 pt outline of segment `index`, flattened to [`OUTLINE_TOLERANCE`] device pixels at
/// `scale`, or `None` for an unknown index.
pub fn outline<Id: Copy + Eq + std::hash::Hash>(
    layout: &Layout<Id>,
    metrics: &Metrics,
    index: usize,
    scale: f32,
) -> Option<Path<Pixels>> {
    let segment = layout.segments().get(index)?;
    let mut flat = Vec::new();
    segment.sector(metrics).flatten(OUTLINE_TOLERANCE / f64::from(scale.max(1.0)), &mut flat);
    let points: Vec<_> = flat.into_iter().map(|p| point(px(p.x as f32), px(p.y as f32))).collect();
    let mut builder = PathBuilder::stroke(px(OUTLINE_WIDTH));
    builder.add_polygon(&points, true);
    builder.build().ok()
}

/// Straight 8-bit sRGB of a GPUI color (tests compare raster pixels against it).
#[cfg(test)]
pub fn expected_rgba(color: gpui::Hsla) -> [u8; 4] {
    rgba8(color)
}
