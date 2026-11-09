//! Chart metrics and annular-sector geometry.
//!
//! Two angle conventions appear here:
//! - *Chart angles* (`theta`) run clockwise from 12 o'clock in `[0, 2π]`. Layout and hit-testing use them.
//! - *Screen angles* (`ArcParams`) follow the usual y-down canvas convention: a point is
//!   `center + r·(cos a, sin a)`, so increasing angles also turn clockwise on screen. Screen angle = theta − π/2.

use std::f64::consts::{FRAC_PI_2, TAU};

/// Space kept between the outermost ring and the shorter side of the chart bounds.
pub const OUTER_MARGIN: f64 = 14.0;
/// Center hole radius as a fraction of the outer radius.
pub const HOLE_FRACTION: f64 = 0.43;
/// Number of equal-width rings.
pub const RING_COUNT: usize = 4;
/// Radial gap trimmed from each side of a ring.
pub const RING_INSET: f64 = 1.4;
/// Angular gap per side, as a fraction of the segment sweep...
pub const GAP_SWEEP_FRACTION: f64 = 0.17;
/// ...capped at this arc length (points) measured at the ring's mid radius.
pub const GAP_ARC_LENGTH: f64 = 1.2;
/// Horizontal spacing of the hatch lines drawn over collected segments.
pub const HATCH_SPACING: f64 = 8.0;

const MAX_FLATTEN_STEPS: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: Point,
    pub max: Point,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineSegment {
    pub from: Point,
    pub to: Point,
}

/// Distance and chart angle of `point` around `center`. The single source of truth for both
/// [`Sector::contains`] and layout hit-testing, so the two agree bit for bit.
#[inline]
pub(crate) fn polar(center: Point, point: Point) -> (f64, f64) {
    let dx = point.x - center.x;
    let dy = point.y - center.y;
    let theta = dx.atan2(-dy);
    ((dx * dx + dy * dy).sqrt(), if theta < 0.0 { theta + TAU } else { theta })
}

#[inline]
fn point_at_theta(center: Point, radius: f64, theta: f64) -> Point {
    let (sin, cos) = theta.sin_cos();
    Point::new(center.x + radius * sin, center.y - radius * cos)
}

/// Chart placement derived from the view size. Port of Swift `SunburstMetrics`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub center: Point,
    /// Radius of the center hole (the "go up" target).
    pub inner_radius: f64,
    pub ring_width: f64,
}

impl Metrics {
    /// Metrics for a chart occupying `(0, 0, width, height)`.
    pub fn new(width: f64, height: f64) -> Self {
        Self::from_bounds(0.0, 0.0, width, height)
    }

    /// Metrics for a chart occupying the given bounds (e.g. window coordinates).
    pub fn from_bounds(x: f64, y: f64, width: f64, height: f64) -> Self {
        let radius = (width.min(height) / 2.0 - OUTER_MARGIN).max(1.0);
        let inner_radius = radius * HOLE_FRACTION;
        Self {
            center: Point::new(x + width / 2.0, y + height / 2.0),
            inner_radius,
            ring_width: (radius - inner_radius) / RING_COUNT as f64,
        }
    }

    pub fn outer_radius(&self) -> f64 {
        self.inner_radius + RING_COUNT as f64 * self.ring_width
    }

    /// Inner and outer radius of the painted band of ring `depth` (after the radial inset).
    /// For very small charts the band keeps a 1pt minimum width, so adjacent bands may overlap.
    #[inline]
    pub fn ring_radii(&self, depth: usize) -> (f64, f64) {
        let inner = self.inner_radius + depth as f64 * self.ring_width + RING_INSET;
        (inner, inner + (self.ring_width - 2.0 * RING_INSET).max(1.0))
    }

    /// The painted sector for a segment on ring `depth` spanning chart angles `start..end`,
    /// with the angular gap already trimmed.
    #[inline]
    pub fn sector(&self, depth: usize, start: f64, end: f64) -> Sector {
        let (inner, outer) = self.ring_radii(depth);
        let inset = ((end - start) * GAP_SWEEP_FRACTION).min(GAP_ARC_LENGTH / ((inner + outer) / 2.0));
        Sector { center: self.center, inner_radius: inner, outer_radius: outer, start: start + inset, end: end - inset }
    }

    /// Whether `point` lies strictly inside the center hole.
    pub fn in_center(&self, point: Point) -> bool {
        let dx = point.x - self.center.x;
        let dy = point.y - self.center.y;
        dx * dx + dy * dy < self.inner_radius * self.inner_radius
    }
}

/// A circular arc in screen angles (see the module docs). `end_angle > start_angle` turns clockwise
/// on screen; the inner arc of a sector runs the other way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArcParams {
    pub center: Point,
    pub radius: f64,
    pub start_angle: f64,
    pub end_angle: f64,
}

impl ArcParams {
    pub fn point_at(&self, angle: f64) -> Point {
        let (sin, cos) = angle.sin_cos();
        Point::new(self.center.x + self.radius * cos, self.center.y + self.radius * sin)
    }

    pub fn start_point(&self) -> Point {
        self.point_at(self.start_angle)
    }

    pub fn end_point(&self) -> Point {
        self.point_at(self.end_angle)
    }

    /// Appends points from the start to the end of the arc (both included) so that no chord
    /// deviates from the arc by more than `tolerance`.
    pub fn flatten(&self, tolerance: f64, out: &mut Vec<Point>) {
        let sweep = self.end_angle - self.start_angle;
        let steps = flatten_steps(self.radius, sweep.abs(), tolerance);
        out.reserve(steps + 1);
        for step in 0..=steps {
            let angle =
                if step == steps { self.end_angle } else { self.start_angle + sweep * step as f64 / steps as f64 };
            out.push(self.point_at(angle));
        }
    }
}

fn flatten_steps(radius: f64, sweep: f64, tolerance: f64) -> usize {
    if !(radius > 0.0 && sweep > 0.0 && sweep.is_finite()) {
        return 1;
    }
    let tolerance = if tolerance.is_finite() && tolerance > 0.0 { tolerance } else { 0.25 };
    // A chord spanning `step` radians deviates from its arc by r·(1 − cos(step/2)).
    let step = if tolerance >= radius { FRAC_PI_2 } else { (2.0 * (1.0 - tolerance / radius).acos()).min(FRAC_PI_2) };
    ((sweep / step).ceil() as usize).clamp(1, MAX_FLATTEN_STEPS)
}

/// A trimmed annular sector: the exact painted shape of one segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sector {
    pub center: Point,
    pub inner_radius: f64,
    pub outer_radius: f64,
    /// Trimmed chart angles.
    pub start: f64,
    pub end: f64,
}

impl Sector {
    pub fn mid_radius(&self) -> f64 {
        (self.inner_radius + self.outer_radius) / 2.0
    }

    /// Outer arc, clockwise from the start edge to the end edge.
    pub fn outer_arc(&self) -> ArcParams {
        ArcParams {
            center: self.center,
            radius: self.outer_radius,
            start_angle: self.start - FRAC_PI_2,
            end_angle: self.end - FRAC_PI_2,
        }
    }

    /// Inner arc, counter-clockwise from the end edge back to the start edge. Outline =
    /// outer arc, line to inner arc start, inner arc, close.
    pub fn inner_arc(&self) -> ArcParams {
        ArcParams {
            center: self.center,
            radius: self.inner_radius,
            start_angle: self.end - FRAC_PI_2,
            end_angle: self.start - FRAC_PI_2,
        }
    }

    /// Appends a closed polygon (first point not repeated) approximating the outline within `tolerance`.
    pub fn flatten(&self, tolerance: f64, out: &mut Vec<Point>) {
        self.outer_arc().flatten(tolerance, out);
        self.inner_arc().flatten(tolerance, out);
    }

    /// Point-in-sector, boundaries included.
    #[inline]
    pub fn contains(&self, point: Point) -> bool {
        let (radius, theta) = polar(self.center, point);
        radius >= self.inner_radius && radius <= self.outer_radius && theta >= self.start && theta <= self.end
    }

    /// Tight axis-aligned bounds of the sector.
    pub fn bounds(&self) -> Rect {
        let corners = [
            point_at_theta(self.center, self.inner_radius, self.start),
            point_at_theta(self.center, self.inner_radius, self.end),
            point_at_theta(self.center, self.outer_radius, self.start),
            point_at_theta(self.center, self.outer_radius, self.end),
        ];
        let mut min = corners[0];
        let mut max = corners[0];
        let mut include = |point: Point| {
            min = Point::new(min.x.min(point.x), min.y.min(point.y));
            max = Point::new(max.x.max(point.x), max.y.max(point.y));
        };
        corners.into_iter().for_each(&mut include);
        for quarter in 0..=4 {
            let theta = quarter as f64 * FRAC_PI_2;
            if theta >= self.start && theta <= self.end {
                include(point_at_theta(self.center, self.outer_radius, theta));
            }
        }
        Rect { min, max }
    }

    /// Diagonal hatching (down-right at 45°) clipped exactly to the sector, matching the Swift
    /// renderer: lines start at `bounds.min.x − bounds.height` and repeat every `spacing` points
    /// horizontally. Appends one segment per visible piece of each line.
    pub fn hatch(&self, spacing: f64, out: &mut Vec<LineSegment>) {
        if !(spacing > 0.0 && self.end > self.start && self.outer_radius > self.inner_radius) {
            return;
        }
        let bounds = self.bounds();
        let first = bounds.min.x - (bounds.max.y - bounds.min.y);
        let lines = ((bounds.max.x - first) / spacing).floor();
        if !(lines.is_finite() && lines >= 0.0) {
            return;
        }
        for line in 0..=(lines as usize).min(1 << 20) {
            self.clip_diagonal(first + line as f64 * spacing - bounds.min.y, out);
        }
    }

    /// Clips the line `x − y = c` to the sector.
    fn clip_diagonal(&self, c: f64, out: &mut Vec<LineSegment>) {
        const SQRT_HALF: f64 = std::f64::consts::FRAC_1_SQRT_2;
        // Unit direction u = (1, 1)/√2 and unit normal n = (1, −1)/√2. `offset` is the signed
        // distance from the line to the center along n; `foot` is the closest point to the center.
        let offset = (self.center.x - self.center.y - c) * SQRT_HALF;
        if offset.abs() >= self.outer_radius {
            return;
        }
        let foot = Point::new(self.center.x - offset * SQRT_HALF, self.center.y + offset * SQRT_HALF);
        let at = |t: f64| Point::new(foot.x + t * SQRT_HALF, foot.y + t * SQRT_HALF);

        let reach = (self.outer_radius * self.outer_radius - offset * offset).sqrt();
        let mut cuts = [0.0; 6];
        let mut count = 0;
        let mut cut = |t: f64| {
            cuts[count] = t;
            count += 1;
        };
        cut(-reach);
        cut(reach);
        if offset.abs() < self.inner_radius {
            let hole = (self.inner_radius * self.inner_radius - offset * offset).sqrt();
            cut(-hole);
            cut(hole);
        }
        for theta in [self.start, self.end] {
            // Edge ray from the center: center + s·v with v = (sin θ, −cos θ).
            let (sin, cos) = theta.sin_cos();
            let across = (sin + cos) * SQRT_HALF; // v·n
            if across.abs() > 1e-15 {
                let along_ray = -offset / across;
                let t = along_ray * (sin - cos) * SQRT_HALF; // s·(v·u)
                if along_ray > 0.0 && t > -reach && t < reach {
                    cut(t);
                }
            }
        }
        let cuts = &mut cuts[..count];
        cuts.sort_unstable_by(f64::total_cmp);

        let mut open: Option<f64> = None;
        for pair in cuts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let inside = b > a && self.contains(at((a + b) / 2.0));
            match (inside, open) {
                (true, None) => open = Some(a),
                (false, Some(from)) => {
                    out.push(LineSegment { from: at(from), to: at(a) });
                    open = None;
                }
                _ => {}
            }
        }
        if let Some(from) = open {
            out.push(LineSegment { from: at(from), to: at(cuts[cuts.len() - 1]) });
        }
    }

    /// Center for a collection badge of the given diameter, or `None` when it would not fit with
    /// 2pt of clearance on each side. Port of Swift `collectionMarkerCenter`.
    pub fn badge_center(&self, diameter: f64) -> Option<Point> {
        let width = self.outer_radius - self.inner_radius;
        let radius = self.mid_radius();
        let half_sweep = ((self.end - self.start) / 2.0).max(0.0);
        let available = 2.0 * radius * half_sweep.min(FRAC_PI_2).sin();
        (width >= diameter + 4.0 && available >= diameter + 4.0)
            .then(|| point_at_theta(self.center, radius, (self.start + self.end) / 2.0))
    }
}
