//! Frame harness: drives synthetic input, one input per display frame, then prints
//! frame-interval and frame-work percentiles per step and overall, and quits. Used to gate UI performance
//! (target: p99 frame interval < 8.3 ms on a 120 Hz display).
//!
//! Script (one step per line, `#` comments):
//! ```text
//! hover 300      # pointer sweeps the map rings for 300 frames
//! scroll 240     # wheel scrolling over the Contents list (down, then up)
//! navigate 10    # open the largest folder, then go up (10 cycles, 30 frames each)
//! drag 5         # press on a list row, drag to the tray, release (40 frames each)
//! idle 60        # nothing happens
//! ```
//! Input enters where the panels' own handlers would put it: map hover through
//! `MapPanel::pointer_moved` (the mouse-move handler's body), list scrolling through
//! `ContentsPanel::scroll_by` (offset + notify, as GPUI's wheel handler does), navigation and
//! drags as the app-state events the panels emit. GPUI 0.2.2's `Window::dispatch_event` returns
//! a crate-private type and cannot be called from outside gpui, so OS-level hit testing is not
//! part of the measurement.

use std::time::{Duration, Instant};

use gpui::{App, Bounds, Pixels, Point, Window, point, px};
use orbit_app_state::{DragId, DragSource, Event, GestureId, Regions};

use crate::frame_stats::{self, Percentiles};
use crate::host::Host;
use crate::panels::Panels;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Hover(u32),
    Scroll(u32),
    Navigate(u32),
    Drag(u32),
    Idle(u32),
}

impl Step {
    pub fn frames(self) -> u32 {
        match self {
            Self::Hover(n) | Self::Scroll(n) | Self::Idle(n) => n,
            Self::Navigate(n) => n * 30,
            Self::Drag(n) => n * 40,
        }
    }

    pub fn name(self) -> String {
        match self {
            Self::Hover(n) => format!("hover {n}"),
            Self::Scroll(n) => format!("scroll {n}"),
            Self::Navigate(n) => format!("navigate {n}"),
            Self::Drag(n) => format!("drag {n}"),
            Self::Idle(n) => format!("idle {n}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessScript {
    pub steps: Vec<Step>,
    /// Frames drawn (after the map is ready) before measuring.
    pub warmup: u32,
}

impl HarnessScript {
    pub fn builtin() -> Self {
        Self {
            steps: vec![Step::Hover(360), Step::Scroll(240), Step::Navigate(8), Step::Drag(4), Step::Idle(30)],
            warmup: 30,
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut steps = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut words = line.split_whitespace();
            let (Some(kind), Some(count)) = (words.next(), words.next()) else {
                return Err(format!("line {}: expected `<step> <count>`", number + 1));
            };
            let count: u32 = count.parse().map_err(|_| format!("line {}: bad count `{count}`", number + 1))?;
            steps.push(match kind {
                "hover" => Step::Hover(count),
                "scroll" => Step::Scroll(count),
                "navigate" => Step::Navigate(count),
                "drag" => Step::Drag(count),
                "idle" => Step::Idle(count),
                other => return Err(format!("line {}: unknown step `{other}`", number + 1)),
            });
        }
        if steps.is_empty() {
            return Err("the script has no steps".into());
        }
        Ok(Self { steps, warmup: 30 })
    }
}

enum Phase {
    Waiting { since: Instant },
    Warmup(u32),
    Running { step: usize, frame: u32 },
    Done,
}

pub struct FrameHarness {
    host: Host,
    panels: Panels,
    script: HarnessScript,
    phase: Phase,
    results: Vec<(String, frame_stats::Summary)>,
    all_intervals: Vec<Duration>,
    all_work: Vec<Duration>,
    started: Option<Instant>,
}

/// Give up when the map is not ready after this long.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

impl FrameHarness {
    pub fn new(host: Host, panels: Panels, script: HarnessScript) -> Self {
        Self {
            host,
            panels,
            script,
            phase: Phase::Waiting { since: Instant::now() },
            results: Vec::new(),
            all_intervals: Vec::new(),
            all_work: Vec::new(),
            started: None,
        }
    }

    /// Runs before each frame is drawn.
    pub fn tick(&mut self, _: &mut Window, cx: &mut App) {
        match self.phase {
            Phase::Waiting { since } => {
                let state = self.host.state(cx);
                let ready = state.folder().is_some()
                    && state.snapshot().is_some()
                    && !state.is_scanning()
                    && !state.is_refreshing()
                    && state.row_count() > 0;
                if ready {
                    self.phase = Phase::Warmup(self.script.warmup);
                } else if since.elapsed() > READY_TIMEOUT {
                    eprintln!("frame-harness: the map was not ready after {READY_TIMEOUT:?}");
                    self.phase = Phase::Done;
                    cx.quit();
                }
            }
            Phase::Warmup(0) => {
                frame_stats::reset();
                self.started = Some(Instant::now());
                self.phase = Phase::Running { step: 0, frame: 0 };
            }
            Phase::Warmup(n) => self.phase = Phase::Warmup(n - 1),
            Phase::Running { step, frame } => {
                let current = self.script.steps[step];
                self.drive(current, frame, cx);
                if frame + 1 >= current.frames() {
                    let summary = frame_stats::take_report();
                    self.all_intervals.extend(&summary.interval_samples);
                    self.all_work.extend(&summary.work_samples);
                    self.results.push((current.name(), summary));
                    if step + 1 < self.script.steps.len() {
                        self.phase = Phase::Running { step: step + 1, frame: 0 };
                    } else {
                        self.phase = Phase::Done;
                        self.finish(cx);
                    }
                } else {
                    self.phase = Phase::Running { step, frame: frame + 1 };
                }
            }
            Phase::Done => {}
        }
    }

    /// Applies the input of `step` at `frame` (also used by the headless harness test).
    pub fn drive(&mut self, step: Step, frame: u32, cx: &mut App) {
        match step {
            Step::Hover(_) => {
                if let Some(target) = map_point(frame) {
                    self.panels.map.update(cx, |map, cx| map.pointer_moved(target, cx));
                }
            }
            Step::Scroll(_) => {
                let dy = if (frame / 60).is_multiple_of(2) { -24. } else { 24. };
                self.panels.contents.update(cx, |list, cx| list.scroll_by(px(dy), cx));
            }
            Step::Navigate(_) => match frame % 30 {
                0 => {
                    let state = self.host.state(cx);
                    let first_dir = (0..state.row_count().min(64))
                        .filter_map(|i| state.row(i))
                        .find(|row| row.meta().flags.is_dir)
                        .map(|row| row.item().path);
                    if let Some(path) = first_dir {
                        self.host.send(Event::Open { path }, cx);
                    }
                }
                15 => self.host.send(Event::GoUp, cx),
                _ => {}
            },
            Step::Drag(_) => {
                let drag = DragId(1_000 + u64::from(frame / 40));
                let gesture = GestureId(drag.0);
                match frame % 40 {
                    0 => {
                        let state = self.host.state(cx);
                        let Some(row) = state.row(1).or_else(|| state.row(0)) else { return };
                        let (path, label) = (row.item().path, row.item().display_name());
                        self.host.send(Event::HoverRow { path: path.clone() }, cx);
                        self.host.send(Event::GestureBegin(gesture), cx);
                        self.host.send(Event::DragBegin { drag, source: DragSource::Items(vec![path]), label }, cx);
                    }
                    20 => self.host.send(Event::DragOverTray { drag, over: true }, cx),
                    31 => {
                        // Release outside the tray: the collection stays unchanged between runs.
                        self.host.send(Event::DragOverTray { drag, over: false }, cx);
                        self.host.send(Event::DragEnd { drag }, cx);
                        self.host.send(Event::GestureEnd(gesture), cx);
                        self.host.send(Event::ClearRowHover, cx);
                    }
                    _ => {}
                }
            }
            Step::Idle(_) => {}
        }
    }

    fn finish(&mut self, cx: &mut App) {
        let elapsed = self.started.map(|s| s.elapsed()).unwrap_or_default();
        println!("frame-harness: {} steps in {:.1}s", self.results.len(), elapsed.as_secs_f64());
        for (name, summary) in &self.results {
            println!("== {name}");
            println!("{summary}");
        }
        let interval = Percentiles::of(&self.all_intervals);
        let work = Percentiles::of(&self.all_work);
        println!("== overall");
        println!("frame interval: {interval}");
        println!("frame work:     {work}");
        let verdict = if interval.p99 < 8.3 { "PASS" } else { "FAIL" };
        println!("target p99 frame interval < 8.3 ms: {verdict} (p99 {:.2} ms)", interval.p99);
        cx.quit();
    }
}

/// A pointer sweeping clockwise over the map, moving out one ring every 15 frames.
fn map_point(frame: u32) -> Option<Point<Pixels>> {
    let map: Bounds<Pixels> = frame_stats::bounds(Regions::MAP)?;
    // The chart canvas leaves the metrics and legend rows (≈ 90 pt) below it.
    let (x, y) = (f64::from(map.origin.x) + 16., f64::from(map.origin.y));
    let (w, h) = (f64::from(map.size.width) - 32., f64::from(map.size.height) - 90.);
    let metrics = orbit_sunburst::Metrics::from_bounds(x, y, w, h);
    let angle = f64::from(frame) * 0.09;
    let ring = f64::from(frame / 15 % 4);
    let radius = metrics.inner_radius + metrics.ring_width * (0.5 + ring);
    Some(point(
        px((metrics.center.x + radius * angle.sin()) as f32),
        px((metrics.center.y - radius * angle.cos()) as f32),
    ))
}
