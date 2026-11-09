//! Frame instrumentation (UI thread only).
//!
//! - Every panel calls [`rendered`] at the top of its `render` (render counts per region; tests
//!   use them to prove a region-only notify re-renders only that region).
//! - The root wraps each panel in [`timed`], which measures the panel's prepaint (for a cached
//!   view this includes its `render` and layout) and paint per frame, and records its bounds.
//! - The root calls [`begin_frame`] in its render and paints [`end_marker`] last, giving the
//!   frame interval (vsync to vsync, what the user sees) and the frame work (render to last paint).
//!
//! With `ORBIT_FRAME_STATS=1` a summary is printed every `ORBIT_FRAME_STATS_EVERY` (default 120)
//! frames. Intervals longer than [`IDLE_GAP`] are idle gaps (nothing changed), not slow frames.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels,
    Styled, Window, canvas,
};
use orbit_app_state::Regions;

use crate::regions::{self, ALL};

/// Intervals above this are idle gaps (the window had nothing to draw).
pub const IDLE_GAP: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default)]
struct RegionTimes {
    prepaint: Duration,
    paint: Duration,
    max_frame: Duration,
    frames: u32,
}

#[derive(Debug, Default)]
struct Stats {
    log: bool,
    report_every: usize,
    renders: [u64; 8],
    times: [RegionTimes; 8],
    current: [Duration; 8],
    bounds: [Option<Bounds<Pixels>>; 8],
    frame_start: Option<Instant>,
    last_begin: Option<Instant>,
    intervals: Vec<Duration>,
    work: Vec<Duration>,
    idle_gaps: usize,
    frames: usize,
    renders_at_report: [u64; 8],
}

thread_local! {
    static STATS: RefCell<Stats> = RefCell::new(Stats {
        log: std::env::var_os("ORBIT_FRAME_STATS").is_some_and(|v| v != "0"),
        report_every: std::env::var("ORBIT_FRAME_STATS_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(120),
        ..Stats::default()
    });
}

fn with<R>(f: impl FnOnce(&mut Stats) -> R) -> R {
    STATS.with(|stats| f(&mut stats.borrow_mut()))
}

/// Logging enabled (`ORBIT_FRAME_STATS=1`)?
pub fn logging() -> bool {
    with(|s| s.log)
}

/// Counts one render of `region`'s view. Call first thing in `render`.
pub fn rendered(region: Regions) {
    with(|s| s.renders[regions::index(region)] += 1);
}

/// Renders of `region`'s view so far.
pub fn render_count(region: Regions) -> u64 {
    with(|s| s.renders[regions::index(region)])
}

/// Bounds of `region`'s view in the last frame it was laid out.
pub fn bounds(region: Regions) -> Option<Bounds<Pixels>> {
    with(|s| s.bounds[regions::index(region)])
}

/// Call at the start of the root view's render.
pub fn begin_frame() {
    let now = Instant::now();
    with(|s| {
        if let Some(last) = s.last_begin.replace(now) {
            let interval = now - last;
            if interval > IDLE_GAP {
                s.idle_gaps += 1;
            } else {
                s.intervals.push(interval);
            }
        }
        s.frame_start = Some(now);
        s.current = Default::default();
    });
}

fn end_frame() {
    let now = Instant::now();
    let report = with(|s| {
        if let Some(start) = s.frame_start.take() {
            s.work.push(now - start);
        }
        for (times, current) in s.times.iter_mut().zip(s.current) {
            times.max_frame = times.max_frame.max(current);
        }
        s.frames += 1;
        s.log && s.report_every > 0 && s.frames.is_multiple_of(s.report_every)
    });
    if report {
        eprintln!("{}", take_report());
    }
}

/// An element painted last by the root: closes the frame's work measurement.
pub fn end_marker() -> impl IntoElement {
    canvas(|_, _, _| {}, |_, _, _, _| end_frame()).size(gpui::px(0.))
}

/// Percentiles of frame interval and frame work (milliseconds).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Percentiles {
    pub count: usize,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl Percentiles {
    pub fn of(samples: &[Duration]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut ms: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e3).collect();
        ms.sort_by(f64::total_cmp);
        let at = |p: f64| ms[((ms.len() - 1) as f64 * p).round() as usize];
        Self { count: ms.len(), p50: at(0.5), p95: at(0.95), p99: at(0.99), max: at(1.0) }
    }
}

impl std::fmt::Display for Percentiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "n={} p50={:.2} p95={:.2} p99={:.2} max={:.2} ms", self.count, self.p50, self.p95, self.p99, self.max)
    }
}

/// Summary since the last [`reset`] or report.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub interval: Percentiles,
    pub work: Percentiles,
    pub idle_gaps: usize,
    /// Per region: (name, renders, mean prepaint+paint ms per drawn frame, max ms in one frame).
    pub regions: Vec<(&'static str, u64, f64, f64)>,
    pub interval_samples: Vec<Duration>,
    pub work_samples: Vec<Duration>,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "frame interval: {}", self.interval)?;
        writeln!(f, "frame work:     {} (root render .. last paint)", self.work)?;
        write!(f, "idle gaps: {}; regions (renders, mean ms, max ms):", self.idle_gaps)?;
        for (name, renders, mean, max) in &self.regions {
            write!(f, " {name}={renders}/{mean:.3}/{max:.3}")?;
        }
        Ok(())
    }
}

/// Clears interval/work samples and region timings (render counts are kept).
pub fn reset() {
    with(|s| {
        s.intervals.clear();
        s.work.clear();
        s.idle_gaps = 0;
        s.times = Default::default();
        s.renders_at_report = s.renders;
        s.last_begin = None;
    });
}

/// The summary since the last reset; resets afterwards.
pub fn take_report() -> Summary {
    let summary = with(|s| Summary {
        interval: Percentiles::of(&s.intervals),
        work: Percentiles::of(&s.work),
        idle_gaps: s.idle_gaps,
        regions: ALL
            .iter()
            .enumerate()
            .map(|(i, (_, name))| {
                let t = s.times[i];
                let mean =
                    if t.frames == 0 { 0.0 } else { (t.prepaint + t.paint).as_secs_f64() * 1e3 / f64::from(t.frames) };
                (*name, s.renders[i] - s.renders_at_report[i], mean, t.max_frame.as_secs_f64() * 1e3)
            })
            .collect(),
        interval_samples: s.intervals.clone(),
        work_samples: s.work.clone(),
    });
    reset();
    with(|s| s.last_begin = Some(Instant::now()));
    summary
}

/// Wraps a panel element: measures its prepaint and paint and records its bounds.
pub fn timed(region: Regions, child: impl IntoElement) -> Timed {
    Timed { index: regions::index(region), child: child.into_any_element() }
}

pub struct Timed {
    index: usize,
    child: AnyElement,
}

impl IntoElement for Timed {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Timed {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let start = Instant::now();
        self.child.prepaint(window, cx);
        let elapsed = start.elapsed();
        let i = self.index;
        with(|s| {
            s.times[i].prepaint += elapsed;
            s.times[i].frames += 1;
            s.current[i] += elapsed;
            s.bounds[i] = Some(bounds);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let start = Instant::now();
        self.child.paint(window, cx);
        let elapsed = start.elapsed();
        let i = self.index;
        with(|s| {
            s.times[i].paint += elapsed;
            s.current[i] += elapsed;
        });
    }
}
