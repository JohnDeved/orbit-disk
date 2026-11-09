//! Exact-area anti-aliased polygon rasterization (signed-area accumulation, as in font-rs and
//! stb_truetype v2): every pixel gets the exact fraction of its area inside the shape, so long
//! gentle arcs ramp smoothly through 256 levels like Core Graphics instead of stepping.
//! (Supersampling rasterizers, `tiny-skia`'s 4 × 4 included, quantize a near-horizontal edge to
//! a few coverage levels: the stair-steps this replaces.)
//!
//! Shapes are closed polygons in raster pixels, wound consistently; overlapping shapes of one
//! layer union (the accumulated winding is clamped to full coverage). Layers composite in order
//! (premultiplied source-over, 8-bit sRGB like Core Graphics and GPUI's own compositing).

/// Closed polygons in raster pixels.
#[derive(Default)]
pub struct Polygons {
    points: Vec<[f32; 2]>,
    /// End index (exclusive) of each polygon in `points`.
    ends: Vec<u32>,
    /// Rows touched (pixels): `min_y..max_y`.
    min_y: f32,
    max_y: f32,
}

impl Polygons {
    pub fn new() -> Self {
        Self { points: Vec::new(), ends: Vec::new(), min_y: f32::INFINITY, max_y: f32::NEG_INFINITY }
    }

    /// Adds one closed polygon (the last point connects back to the first).
    pub fn push(&mut self, points: impl IntoIterator<Item = [f32; 2]>) {
        let start = self.points.len();
        for p in points {
            self.min_y = self.min_y.min(p[1]);
            self.max_y = self.max_y.max(p[1]);
            self.points.push(p);
        }
        if self.points.len() - start < 3 {
            self.points.truncate(start);
        } else {
            self.ends.push(self.points.len() as u32);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Every edge `(from, to)`.
    fn edges(&self) -> impl Iterator<Item = ([f32; 2], [f32; 2])> + '_ {
        let mut start = 0;
        self.ends.iter().flat_map(move |&end| {
            let polygon = &self.points[start..end as usize];
            start = end as usize;
            let closing = std::iter::once((polygon[polygon.len() - 1], polygon[0]));
            polygon.windows(2).map(|pair| (pair[0], pair[1])).chain(closing)
        })
    }
}

/// One color's shapes.
pub struct Layer {
    /// Straight 8-bit sRGB.
    pub color: [u8; 4],
    pub shapes: Polygons,
}

/// Accumulation buffer for one band of rows (reused across layers and bands).
pub struct Coverage {
    width: usize,
    /// Row stride: two spare cells for writes just right of the last pixel.
    stride: usize,
    rows: usize,
    cells: Vec<f32>,
    /// Touched cells per row: `(first, last)` inclusive; `first > last` when untouched.
    spans: Vec<(usize, usize)>,
}

impl Coverage {
    pub fn new(width: usize, rows: usize) -> Self {
        let stride = width + 2;
        Self { width, stride, rows, cells: vec![0.0; stride * rows], spans: vec![(usize::MAX, 0); rows] }
    }

    #[inline]
    fn add(&mut self, row: usize, x: usize, value: f32) {
        let x = x.min(self.stride - 1);
        self.cells[row * self.stride + x] += value;
        let span = &mut self.spans[row];
        span.0 = span.0.min(x);
        span.1 = span.1.max(x);
    }

    /// Accumulates the signed area right of the edge `p0 → p1` (band-local pixels).
    fn line(&mut self, p0: [f32; 2], p1: [f32; 2]) {
        if p0[1] == p1[1] {
            return;
        }
        let (direction, p0, p1) = if p0[1] < p1[1] { (1.0, p0, p1) } else { (-1.0, p1, p0) };
        let rows = self.rows as f32;
        if p1[1] <= 0.0 || p0[1] >= rows {
            return;
        }
        // Left of the image counts as at its left edge, right of it as at its right edge (the
        // area inside is unchanged).
        let limit = self.width as f32;
        let dxdy = (p1[0] - p0[0]) / (p1[1] - p0[1]);
        let first = p0[1].max(0.0);
        let mut x = p0[0] + (first - p0[1]) * dxdy;
        let mut row = first as usize;
        let end = p1[1].min(rows);
        while (row as f32) < end {
            let top = (row as f32).max(p0[1]);
            let bottom = ((row + 1) as f32).min(p1[1]);
            let dy = bottom - top;
            let next = x + dxdy * dy;
            let d = dy * direction;
            let (x0, x1) = if x < next { (x, next) } else { (next, x) };
            let (x0, x1) = (x0.clamp(0.0, limit), x1.clamp(0.0, limit));
            let x0_floor = x0.floor();
            let x0_index = x0_floor as usize;
            let x1_ceil = x1.ceil();
            let x1_index = x1_ceil as usize;
            if x1_index <= x0_index + 1 {
                // Within one pixel column: split by the mean x.
                let mean = 0.5 * (x0 + x1) - x0_floor;
                self.add(row, x0_index, d - d * mean);
                self.add(row, x0_index + 1, d * mean);
            } else {
                let s = (x1 - x0).recip();
                let x0_fraction = x0 - x0_floor;
                let a0 = 0.5 * s * (1.0 - x0_fraction) * (1.0 - x0_fraction);
                let x1_fraction = x1 - x1_ceil + 1.0;
                let am = 0.5 * s * x1_fraction * x1_fraction;
                self.add(row, x0_index, d * a0);
                if x1_index == x0_index + 2 {
                    self.add(row, x0_index + 1, d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - x0_fraction);
                    self.add(row, x0_index + 1, d * (a1 - a0));
                    for column in x0_index + 2..x1_index - 1 {
                        self.add(row, column, d * s);
                    }
                    let a2 = a1 + (x1_index - x0_index - 3) as f32 * s;
                    self.add(row, x1_index - 1, d * (1.0 - a2 - am));
                }
                self.add(row, x1_index, d * am);
            }
            x = next;
            row += 1;
        }
    }

    /// Composites `color` (straight sRGB) at the accumulated coverage into `out` (premultiplied
    /// RGBA rows of this band, `width` pixels each), then clears the accumulator.
    fn composite(&mut self, color: [u8; 4], out: &mut [u8]) {
        let alpha = u32::from(color[3]);
        let premultiplied = [0, 1, 2].map(|k| div255(u32::from(color[k]) * alpha));
        for row in 0..self.rows {
            let (first, last) = std::mem::replace(&mut self.spans[row], (usize::MAX, 0));
            if first > last {
                continue;
            }
            let cells = &mut self.cells[row * self.stride..(row + 1) * self.stride];
            let pixels = &mut out[row * self.width * 4..(row + 1) * self.width * 4];
            let mut sum = 0.0f32;
            for (x, cell) in cells.iter_mut().enumerate().take(last + 1).skip(first) {
                sum += std::mem::take(cell);
                if x >= self.width {
                    continue;
                }
                let coverage = (sum.abs().min(1.0) * 255.0 + 0.5) as u32;
                if coverage == 0 {
                    continue;
                }
                let pixel = &mut pixels[x * 4..x * 4 + 4];
                if coverage == 255 && alpha == 255 {
                    pixel.copy_from_slice(&[
                        premultiplied[0] as u8,
                        premultiplied[1] as u8,
                        premultiplied[2] as u8,
                        255,
                    ]);
                    continue;
                }
                let source_alpha = div255(alpha * coverage);
                let keep = 255 - source_alpha;
                for k in 0..3 {
                    pixel[k] =
                        (div255(premultiplied[k] * coverage) + div255(u32::from(pixel[k]) * keep)).min(255) as u8;
                }
                pixel[3] = (source_alpha + div255(u32::from(pixel[3]) * keep)).min(255) as u8;
            }
            // A closed shape returns the running sum to zero by its last edge cell.
        }
    }
}

/// `x / 255`, rounded, for `x ≤ 255 · 255`.
#[inline]
fn div255(x: u32) -> u32 {
    let x = x + 128;
    (x + (x >> 8)) >> 8
}

/// Draws `layers` into `out`: premultiplied RGBA rows `top..` of an image `width` pixels wide.
/// `cancelled` is polled between layers.
pub fn draw(layers: &[Layer], out: &mut [u8], width: usize, top: usize, cancelled: &dyn Fn() -> bool) {
    let rows = out.len() / (width * 4).max(1);
    let (band_top, band_bottom) = (top as f32, (top + rows) as f32);
    let mut coverage = Coverage::new(width, rows);
    for layer in layers {
        if layer.shapes.max_y <= band_top || layer.shapes.min_y >= band_bottom {
            continue;
        }
        if cancelled() {
            return;
        }
        let mut touched = false;
        for (from, to) in layer.shapes.edges() {
            if from[1].max(to[1]) <= band_top || from[1].min(to[1]) >= band_bottom {
                continue;
            }
            coverage.line([from[0], from[1] - band_top], [to[0], to[1] - band_top]);
            touched = true;
        }
        if touched {
            coverage.composite(layer.color, out);
        }
    }
}
