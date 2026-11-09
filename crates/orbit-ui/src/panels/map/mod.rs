//! The explorer center: the sunburst, the metrics row and the collection legend (Swift
//! `SunburstView`, `MapInteractionView`, `ContentView.explorer`). Observes [`Regions::MAP`].
//!
//! Drawing (one canvas, `DESIGN.md`: "the colored map and collection marks retain their drawing
//! layer; hover and selection update a separate outline layer"):
//! - base layer: fills, hatching, badges (or the empty placeholder ring) rasterized on a
//!   background thread at device resolution ([`layers::render`]), keyed by (layout revision,
//!   presentation revision, bounds, scale factor) and painted as one pixel-aligned image. Hover
//!   and selection never rebuild it. While a newer raster is being built, frames keep painting
//!   the previous one (mapped onto the new chart after a resize); no frame ever waits. A newer
//!   key supersedes (cancels) the pending job; only the latest raster is kept and the GPU
//!   texture of a replaced one is released (`Window::drop_image`).
//! - outline layer: the 2 pt white stroke of the active segment (pointer, hovered row, pinned
//!   group, else the selection) and of the selection, cached by the set of outlined segments.
//! - center: name, size, detail and Up slots ([`center`]), laid out against the hole radius of
//!   the current frame.
//!
//! Input: every pointer move costs one analytic hit test ([`orbit_sunburst::Layout::hit`]); the
//! app state hears `HoverSegment` only when the hovered segment changes and `ClearMapHover` when
//! it leaves. Mouse-down on a segment holds updates (`GestureBegin`) until mouse-up
//! (`GestureEnd`). A release on the pressed segment opens a folder, selects a file or pins a
//! group; a release in the center goes up (the root center is inert). Moving 5 points starts a
//! collection drag with the pressed segment as the source (`DragBegin`) and a GPUI typed drag
//! carrying [`CollectionDrag`]; drop targets send `DragDrop`, Escape cancels.

mod center;
mod coverage;
pub mod layers;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Context, Corners, CursorStyle, DispatchPhase, Entity, FocusHandle, Hitbox,
    HitboxBehavior, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Path, Pixels, Point, Render, RenderImage, SharedString, Styled, Subscription, Task,
    Window, canvas, div, fill, point, prelude::*, px, size,
};
use orbit_app_state::gesture::exceeds_drag_threshold;
use orbit_app_state::{AppState, DragId, DragSource, Event, GestureId, Regions};
use orbit_core::types::ScanState;
use orbit_core::view::{FolderTree, NodeIx};
use orbit_sunburst::{CollectionState, Hit, Metrics, Point as SPoint, Presentation, SegmentIndex};

pub use center::CenterText;
use layers::{Grid, Raster};

use crate::actions::OpenFolder;
use crate::components::{ButtonStyle, CollectionDrag, DragPreview, button, truncate_middle};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::theme;

pub const REGIONS: Regions = Regions::MAP;

/// Horizontal inset of the chart inside the map column.
pub const CHART_INSET: f32 = 16.;
/// Reserved height below the chart: metrics row (40 + 12) and legend (14 + 10), 14 above.
pub const BOTTOM_HEIGHT: f32 = 90.;
/// Center hover wash (Swift `.white.opacity(0.055)`).
const CENTER_HOVER_ALPHA: f32 = 0.055;
/// Gesture and drag ids of the map live above this (rows and the harness use small ids).
const ID_BASE: u64 = 1 << 40;

/// Rebuild counters and timings of the cached layers (tests, measurements).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapStats {
    /// Base rasters requested (background jobs spawned, including superseded ones).
    pub raster_jobs: u64,
    /// Base rasters built and adopted.
    pub raster_builds: u64,
    pub outline_builds: u64,
    /// Background build time of the last adopted raster (render + pixel conversion).
    pub last_raster: Duration,
    pub last_outline: Duration,
    /// UI-thread time of the last chart paint (base image + outlines).
    pub last_paint: Duration,
    /// Bytes of the cached raster image (CPU copy; the GPU texture holds as many again).
    pub raster_bytes: usize,
}

/// What a base raster depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RasterKey {
    layout_revision: u64,
    /// Presentation revision (layout or collection states).
    revision: u64,
    bounds: Bounds<Pixels>,
    scale: f32,
}

/// The latest finished base raster.
struct Base {
    key: RasterKey,
    /// Metrics the raster was drawn for (maps it onto a resized chart while a new one builds).
    metrics: Metrics,
    raster: Raster,
}

/// A base raster being built in the background.
struct PendingRaster {
    key: RasterKey,
    cancelled: Arc<AtomicBool>,
    _task: Task<()>,
}

impl Drop for PendingRaster {
    fn drop(&mut self) {
        // The job checks this between passes; dropping the task cancels it if not yet started.
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

struct Outlines {
    key: (u64, Bounds<Pixels>, u32, [Option<SegmentIndex>; 2]),
    paths: Vec<Path<Pixels>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PressTarget {
    Segment(SegmentIndex),
    Center,
}

#[derive(Clone, Copy, Debug)]
struct Press {
    position: Point<Pixels>,
    target: PressTarget,
    layout_revision: u64,
    /// Held for segment presses only (the center is a plain button).
    gesture: Option<GestureId>,
    /// Moved beyond the drag threshold: never a click.
    moved: bool,
}

pub struct MapPanel {
    host: Host,
    focus: FocusHandle,
    presentation: Option<Presentation<NodeIx>>,
    base: Option<Base>,
    pending: Option<PendingRaster>,
    /// Replaced raster images whose GPU textures are released at the next prepaint.
    retired: Vec<Arc<RenderImage>>,
    outlines: Option<Outlines>,
    last_paint: Cell<Duration>,
    /// Chart bounds and metrics of the last prepaint.
    chart: Option<(Bounds<Pixels>, Metrics)>,
    /// What the pointer is over (local: drives the cursor, center wash and outline).
    pointer: Hit,
    /// The hover the app state was told about (segment, layout revision).
    hover_sent: Option<(SegmentIndex, u64)>,
    /// The segment of the hovered list row.
    row_hovered: Option<SegmentIndex>,
    /// The segment showing the selection, for the selected path and layout revision.
    selected: Option<SegmentIndex>,
    selected_key: Option<(Arc<[u8]>, u64)>,
    /// A clicked group stays the center's subject until the selection or folder changes.
    pinned_group: Option<(SegmentIndex, u64)>,
    press: Option<Press>,
    /// The collection drag of the current press (armed at the threshold, then a GPUI drag).
    drag: Option<CollectionDrag>,
    /// The layout changed: re-hit-test at the pointer after the next layout.
    refresh_hover: bool,
    can_go_up: bool,
    center: CenterText,
    /// The middle-truncated center name for (name, slot width, lines).
    center_name: Option<((SharedString, Pixels, usize), SharedString)>,
    stats: MapStats,
    next_id: u64,
    _subscriptions: Vec<Subscription>,
}

impl MapPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self {
            host: host.clone(),
            focus: cx.focus_handle(),
            presentation: None,
            base: None,
            pending: None,
            retired: Vec::new(),
            outlines: None,
            last_paint: Cell::new(Duration::ZERO),
            chart: None,
            pointer: Hit::None,
            hover_sent: None,
            row_hovered: None,
            selected: None,
            selected_key: None,
            pinned_group: None,
            press: None,
            drag: None,
            refresh_hover: false,
            can_go_up: false,
            center: CenterText::default(),
            center_name: None,
            stats: MapStats::default(),
            next_id: ID_BASE,
            _subscriptions: host.observe(REGIONS, cx),
        }
    }

    /// Layer rebuild counters (tests, measurements).
    pub fn stats(&self) -> MapStats {
        MapStats { last_paint: self.last_paint.get(), ..self.stats }
    }

    /// A base raster is being built (tests).
    pub fn raster_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Chart bounds and metrics of the last frame.
    pub fn chart(&self) -> Option<(Bounds<Pixels>, Metrics)> {
        self.chart
    }

    /// The presentation painted by the last render.
    pub fn presentation(&self) -> Option<&Presentation<NodeIx>> {
        self.presentation.as_ref()
    }

    /// The center's current text.
    pub fn center_text(&self) -> &CenterText {
        &self.center
    }

    /// `name` shortened in the middle to fit the center's name slot at hole `radius` (native
    /// `.truncationMode(.middle)`: a long name keeps its start and its extension). Two-line slots
    /// get a budget of a little less than two lines so the wrapped text never needs a third.
    fn center_name(&mut self, name: &SharedString, radius: f32, window: &Window) -> SharedString {
        let (width, lines) = center::name_slot(radius);
        let key = (name.clone(), width, lines);
        if let Some((cached, truncated)) = &self.center_name
            && *cached == key
        {
            return truncated.clone();
        }
        let budget = if lines == 1 { width } else { width * (lines as f32 - 0.2) };
        let truncated = truncate_middle(name, budget, theme::TEXT_ROW, theme::WEIGHT_MEDIUM, window);
        self.center_name = Some((key, truncated.clone()));
        truncated
    }

    fn layout_revision(&self) -> Option<u64> {
        self.presentation.as_ref().map(|p| p.layout_revision)
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// The segment the center describes and the outline highlights (Swift `activeSegment`).
    fn active_segment(&self) -> Option<SegmentIndex> {
        let pinned = self.pinned_group.filter(|(_, revision)| Some(*revision) == self.layout_revision());
        match self.pointer {
            Hit::Segment(index) => Some(index),
            _ => self.row_hovered.or(pinned.map(|(index, _)| index)).or(self.selected),
        }
    }

    fn outlined(&self) -> [Option<SegmentIndex>; 2] {
        let active = self.active_segment();
        [active, self.selected.filter(|selected| Some(*selected) != active)]
    }

    fn cursor(&self) -> Option<CursorStyle> {
        match self.pointer {
            Hit::Segment(_) => Some(CursorStyle::PointingHand),
            Hit::Center if self.can_go_up => Some(CursorStyle::PointingHand),
            _ => None,
        }
    }

    fn hit(&self, position: Point<Pixels>) -> Hit {
        match (&self.chart, &self.presentation) {
            (Some((_, metrics)), Some(presentation)) => {
                presentation.layout.hit(metrics, SPoint::new(f64::from(position.x), f64::from(position.y)))
            }
            _ => Hit::None,
        }
    }

    // -----------------------------------------------------------------------------------------
    // Layers.

    /// Brings the cached layers up to date for `bounds` (canvas prepaint).
    fn prepare(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let metrics = layers::metrics(bounds);
        self.chart = Some((bounds, metrics));
        // Textures of replaced rasters: the frame that showed them is already submitted (Metal
        // retains what in-flight command buffers use), and this frame paints the new one.
        for image in self.retired.drain(..) {
            window.drop_image(image).ok();
        }
        let Some(presentation) = self.presentation.clone() else {
            self.pending = None;
            if let Some(base) = self.base.take() {
                window.drop_image(base.raster.image).ok();
            }
            self.outlines = None;
            return;
        };
        let scale = window.scale_factor();
        let key =
            RasterKey { layout_revision: presentation.layout_revision, revision: presentation.revision, bounds, scale };
        let current = self.base.as_ref().is_some_and(|base| base.key == key);
        let building = self.pending.as_ref().is_some_and(|pending| pending.key == key);
        if !current && !building {
            self.pending = None; // Supersedes (cancels) an older job.
            self.request_raster(key, metrics, cx);
        }
        let segments = self.outlined();
        let key = (presentation.layout_revision, bounds, scale.to_bits(), segments);
        if self.outlines.as_ref().is_none_or(|outlines| outlines.key != key) {
            let start = Instant::now();
            let paths = segments
                .iter()
                .flatten()
                .filter_map(|&index| layers::outline(&presentation.layout, &metrics, index, scale))
                .collect();
            self.outlines = Some(Outlines { key, paths });
            self.stats.outline_builds += 1;
            self.stats.last_outline = start.elapsed();
        }
        if std::mem::take(&mut self.refresh_hover) && self.press.is_none() {
            // A new layout under a still pointer: hover what is there now (Swift
            // `scheduleHoverRefresh`). Events are sent after this frame, never while drawing.
            let position = window.mouse_position();
            if bounds.contains(&position) {
                cx.defer_in(window, move |map, _, cx| map.pointer_moved(position, cx));
            }
        }
    }

    /// Builds the base raster for `key` on the background executor; the result is adopted only if
    /// no newer key superseded it meanwhile.
    fn request_raster(&mut self, key: RasterKey, metrics: Metrics, cx: &mut Context<Self>) {
        let Some(presentation) = &self.presentation else { return };
        let Some(grid) = Grid::new(key.bounds, key.scale) else { return };
        self.stats.raster_jobs += 1;
        let cancelled = Arc::new(AtomicBool::new(false));
        let job = cx.background_spawn({
            let (layout, states) = (Arc::clone(&presentation.layout), Arc::clone(&presentation.states));
            let cancelled = Arc::clone(&cancelled);
            async move {
                let start = Instant::now();
                let is_cancelled = || cancelled.load(Ordering::Relaxed);
                let pixels = layers::render(&layout, &states, &metrics, &grid, &is_cancelled)?;
                let raster = Raster::from_premultiplied(pixels, grid)?;
                Some((raster, start.elapsed()))
            }
        });
        let task = cx.spawn(async move |map, cx| {
            let Some((raster, elapsed)) = job.await else { return };
            map.update(cx, |map, cx| map.adopt_raster(key, metrics, raster, elapsed, cx)).ok();
        });
        self.pending = Some(PendingRaster { key, cancelled, _task: task });
    }

    fn adopt_raster(
        &mut self,
        key: RasterKey,
        metrics: Metrics,
        raster: Raster,
        elapsed: Duration,
        cx: &mut Context<Self>,
    ) {
        if self.pending.as_ref().is_none_or(|pending| pending.key != key) {
            return;
        }
        self.pending = None;
        self.stats.raster_builds += 1;
        self.stats.last_raster = elapsed;
        self.stats.raster_bytes = raster.bytes();
        if let Some(old) = self.base.replace(Base { key, metrics, raster }) {
            self.retired.push(old.raster.image);
        }
        cx.notify();
    }

    fn paint(&self, window: &mut Window) {
        let start = Instant::now();
        if let (Some(base), Some((bounds, metrics))) = (&self.base, &self.chart) {
            let mut image_bounds = base.raster.grid.paint_bounds();
            if base.key.bounds != *bounds || base.key.scale != window.scale_factor() {
                // A newer raster is on its way: map this one onto the current chart meanwhile.
                let factor = (metrics.outer_radius() / base.metrics.outer_radius()) as f32;
                let (from, to) = (base.metrics.center, metrics.center);
                image_bounds = Bounds::new(
                    point(
                        px(to.x as f32 + (f32::from(image_bounds.origin.x) - from.x as f32) * factor),
                        px(to.y as f32 + (f32::from(image_bounds.origin.y) - from.y as f32) * factor),
                    ),
                    size(image_bounds.size.width * factor, image_bounds.size.height * factor),
                );
            }
            window.paint_image(image_bounds, Corners::default(), Arc::clone(&base.raster.image), 0, false).ok();
        }
        if let Some((_, metrics)) = &self.chart
            && self.pointer == Hit::Center
            && self.can_go_up
        {
            let radius = metrics.inner_radius as f32;
            let origin = point(px(metrics.center.x as f32 - radius), px(metrics.center.y as f32 - radius));
            let bounds = Bounds::new(origin, size(px(2. * radius), px(2. * radius)));
            window.paint_quad(fill(bounds, theme::white(CENTER_HOVER_ALPHA)).corner_radii(px(radius)));
        }
        if let Some(outlines) = &self.outlines {
            for path in &outlines.paths {
                window.paint_path(path.clone(), theme::white(layers::OUTLINE_ALPHA));
            }
        }
        self.last_paint.set(start.elapsed());
    }

    // -----------------------------------------------------------------------------------------
    // Pointer.

    /// The pointer moved to `position` with no button held (the mouse-move handler's body; the
    /// frame harness calls it directly).
    pub fn pointer_moved(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let inside = self.chart.is_some_and(|(bounds, _)| bounds.contains(&position));
        let hit = if inside { self.hit(position) } else { Hit::None };
        self.set_pointer(hit, cx);
    }

    /// Updates the local pointer zone; tells the state only when the hovered segment changed.
    fn set_pointer(&mut self, hit: Hit, cx: &mut Context<Self>) {
        if self.drag.is_some() || hit == self.pointer {
            return;
        }
        self.pointer = hit;
        let segment = match hit {
            Hit::Segment(index) => Some(index),
            Hit::Center | Hit::None => None,
        };
        let target = segment.zip(self.layout_revision());
        if target != self.hover_sent {
            self.hover_sent = target;
            let event = match target {
                Some((segment, layout_revision)) => Event::HoverSegment { segment, layout_revision },
                None => Event::ClearMapHover,
            };
            self.host.send(event, cx);
        }
        cx.notify();
    }

    fn mouse_moved(&mut self, position: Point<Pixels>, inside: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(press) = self.press.as_mut() {
            if press.moved {
                return;
            }
            if exceeds_drag_threshold(xy(press.position), xy(position)) {
                press.moved = true;
                if let PressTarget::Segment(index) = press.target {
                    let revision = press.layout_revision;
                    self.begin_drag(index, revision, window, cx);
                }
                return;
            }
        }
        let hit = if inside { self.hit(position) } else { Hit::None };
        self.set_pointer(hit, cx);
    }

    fn mouse_down(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        // A press whose release never arrived still owes its gesture end.
        if let Some(gesture) = self.press.take().and_then(|press| press.gesture) {
            self.host.send(Event::GestureEnd(gesture), cx);
        }
        let hit = self.hit(position);
        if let Some(layout_revision) = self.layout_revision() {
            let target = match hit {
                Hit::Segment(index) => Some(PressTarget::Segment(index)),
                Hit::Center => Some(PressTarget::Center),
                Hit::None => None,
            };
            if let Some(target) = target {
                let gesture = matches!(target, PressTarget::Segment(_)).then(|| GestureId(self.next_id()));
                if let Some(gesture) = gesture {
                    self.host.send(Event::GestureBegin(gesture), cx);
                }
                self.press = Some(Press { position, target, layout_revision, gesture, moved: false });
            }
        }
        self.set_pointer(hit, cx);
    }

    fn mouse_up(&mut self, position: Point<Pixels>, inside: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(press) = self.press.take() else { return };
        if let Some(drag) = self.drag.take() {
            // A drop target handles this same mouse-up after us: end the drag once it has.
            cx.defer_in(window, move |map, _, cx| map.end_drag(drag.drag, press.gesture, cx));
            return;
        }
        if !press.moved && inside && Some(press.layout_revision) == self.layout_revision() {
            match (press.target, self.hit(position)) {
                (PressTarget::Segment(pressed), Hit::Segment(released)) if pressed == released => {
                    self.activate(pressed, cx)
                }
                (PressTarget::Center, Hit::Center) if self.can_go_up => self.host.send(Event::GoUp, cx),
                _ => {}
            }
        }
        if let Some(gesture) = press.gesture {
            self.host.send(Event::GestureEnd(gesture), cx);
        }
    }

    /// Single click on a segment: folders open, files select, groups pin as the center subject.
    fn activate(&mut self, index: SegmentIndex, cx: &mut Context<Self>) {
        let Some(presentation) = &self.presentation else { return };
        let Some(segment) = presentation.layout.segments().get(index) else { return };
        let Some(ix) = segment.node else {
            self.pinned_group = Some((index, presentation.layout_revision));
            cx.notify();
            return;
        };
        self.pinned_group = None;
        let Some(folder) = self.host.state(cx).folder() else { return };
        let path: Arc<[u8]> = folder.path(ix).into();
        let event =
            if folder.meta(ix).flags.is_dir { Event::Open { path } } else { Event::Select { path: Some(path) } };
        self.host.send(event, cx);
    }

    fn begin_drag(&mut self, index: SegmentIndex, layout_revision: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(presentation) = self.presentation.clone() else { return };
        if presentation.layout_revision != layout_revision {
            return;
        }
        let state = self.host.state(cx);
        let Some(subject) = Subject::of(state, &presentation, Some(index)) else { return };
        let label = subject.name.clone();
        let icon = match subject.kind {
            SubjectKind::Group => Icon::Layers,
            SubjectKind::Node { is_dir: true } => Icon::FolderFill,
            SubjectKind::Node { is_dir: false } | SubjectKind::Folder => Icon::FileFill,
        };
        let detail: SharedString = theme::format_bytes(subject.size).into();
        let drag = DragId(self.next_id());
        let source = DragSource::Segment { segment: index, layout_revision };
        self.host.send(Event::DragBegin { drag, source, label: label.to_string() }, cx);
        // The state refuses drags it cannot collect (it says why in the status line).
        if self.host.state(cx).drag_id() == Some(drag) {
            self.drag = Some(CollectionDrag { drag, label, detail, icon });
            window.focus(&self.focus);
            cx.notify();
        }
    }

    fn end_drag(&mut self, drag: DragId, gesture: Option<GestureId>, cx: &mut Context<Self>) {
        // No-op when a drop target already consumed the drag.
        self.host.send(Event::DragEnd { drag }, cx);
        if let Some(gesture) = gesture {
            self.host.send(Event::GestureEnd(gesture), cx);
        }
        self.set_pointer(Hit::None, cx);
        cx.notify();
    }

    /// Escape during a drag.
    fn cancel_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        cx.stop_active_drag(window);
        let gesture = self.press.take().and_then(|press| press.gesture);
        self.end_drag(drag.drag, gesture, cx);
    }

    // -----------------------------------------------------------------------------------------
    // Render.

    /// Picks up a new presentation, row hover and selection.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let (presentation, row_hovered) =
            self.host.update(cx, |host, _| (host.presentation(), host.row_hover_segment()));
        let revision = presentation.as_ref().map(|p| p.layout_revision);
        if revision != self.layout_revision() {
            // Build the group-member lookup in the background so the first hover of a large
            // folder doesn't pay for it on the UI thread (≈5 ms for a million grouped files).
            if let Some(layout) = presentation.as_ref().map(|p| Arc::clone(&p.layout))
                && layout.has_large_groups()
            {
                cx.background_spawn(async move { layout.warm_member_index() }).detach();
            }
            // Segment indices of the old layout mean nothing now; the state re-targets its hover.
            self.pointer = Hit::None;
            self.hover_sent = None;
            self.pinned_group = None;
            self.refresh_hover = true;
        }
        self.presentation = presentation;
        self.row_hovered = row_hovered;
        let state = self.host.state(cx);
        self.can_go_up = state.can_go_up();
        let selected = state.selected().map(|item| Arc::clone(&item.path)).zip(revision);
        if selected != self.selected_key {
            if selected.as_ref().map(|(path, _)| path) != self.selected_key.as_ref().map(|(path, _)| path) {
                self.pinned_group = None;
            }
            self.selected = match (&selected, &self.presentation, state.folder()) {
                (Some((path, _)), Some(presentation), Some(folder)) => folder
                    .find(path)
                    .filter(|&ix| folder.is_descendant(ix))
                    .and_then(|ix| presentation.layout.index_of_child(ix, folder.tree().node(ix).parent)),
                _ => None,
            };
            self.selected_key = selected;
        }
        self.center = match &self.presentation {
            Some(presentation) => center_text(state, presentation, self.active_segment()),
            None => CenterText::default(),
        };
    }

    fn chart_element(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let canvas = canvas(
            {
                let entity = entity.clone();
                move |bounds, window, cx| prepaint_chart(&entity, bounds, window, cx)
            },
            move |_, prepaint, window, cx| paint_chart(&entity, prepaint, window, cx),
        )
        .size_full();
        div()
            .id("map-chart")
            .flex_1()
            .min_h_0()
            .mx(px(CHART_INSET))
            .child(canvas)
            // GPUI tracks a press for typed drags only on elements with click or drag listeners.
            .on_click(|_, _, _| {})
            .when_some(self.drag.clone(), |el, payload| {
                el.on_drag(payload, |payload, offset, _, cx| cx.new(|_| DragPreview::new(payload, offset)))
            })
    }
}

struct ChartPrepaint {
    hitbox: Hitbox,
    center: Option<AnyElement>,
}

fn prepaint_chart(
    entity: &Entity<MapPanel>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> ChartPrepaint {
    let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
    let (text, metrics) = entity.update(cx, |map, cx| {
        map.prepare(bounds, window, cx);
        let metrics = map.chart.map(|(_, metrics)| metrics);
        let text = map.presentation.is_some().then(|| map.center.clone()).map(|mut text| {
            if let Some(metrics) = metrics {
                text.name = map.center_name(&text.name, metrics.inner_radius as f32, window);
            }
            text
        });
        (text, metrics)
    });
    let center = text.zip(metrics).map(|(text, metrics)| {
        let radius = metrics.inner_radius as f32;
        let value_size = center::value_font_size(&text.value, radius, window);
        let mut element = center::element(&text, radius, value_size);
        let available = size(AvailableSpace::Definite(center::stack_width(radius)), AvailableSpace::MinContent);
        let laid_out = element.layout_as_root(available, window, cx);
        let origin = point(
            px(metrics.center.x as f32) - laid_out.width / 2.,
            px(metrics.center.y as f32) - laid_out.height / 2.,
        );
        element.prepaint_at(origin, window, cx);
        element
    });
    ChartPrepaint { hitbox, center }
}

fn paint_chart(entity: &Entity<MapPanel>, prepaint: ChartPrepaint, window: &mut Window, cx: &mut App) {
    let ChartPrepaint { hitbox, center } = prepaint;
    let map = entity.read(cx);
    map.paint(window);
    if let Some(cursor) = map.cursor() {
        window.set_cursor_style(cursor, &hitbox);
    }
    if let Some(mut center) = center {
        center.paint(window, cx);
    }
    // Window-level listeners: moves and releases are seen outside the chart too (hover exit,
    // drag threshold, releases after leaving the map).
    window.on_mouse_event({
        let (entity, hitbox) = (entity.clone(), hitbox.clone());
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Capture {
                let inside = hitbox.is_hovered(window);
                entity.update(cx, |map, cx| map.mouse_moved(event.position, inside, window, cx));
            }
        }
    });
    window.on_mouse_event({
        let (entity, hitbox) = (entity.clone(), hitbox.clone());
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && hitbox.is_hovered(window) {
                entity.update(cx, |map, cx| map.mouse_down(event.position, cx));
            }
        }
    });
    window.on_mouse_event({
        let entity = entity.clone();
        move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                let inside = hitbox.is_hovered(window);
                entity.update(cx, |map, cx| map.mouse_up(event.position, inside, window, cx));
            }
        }
    });
}

fn xy(point: Point<Pixels>) -> (f64, f64) {
    (f64::from(point.x), f64::from(point.y))
}

// ---------------------------------------------------------------------------------------------
// Center text (Swift `SunburstText`).

enum SubjectKind {
    Folder,
    Node { is_dir: bool },
    Group,
}

/// What the center (or a drag) talks about: a segment or the folder itself.
struct Subject {
    name: SharedString,
    size: u64,
    kind: SubjectKind,
    scan: ScanState,
    collection: CollectionState,
}

impl Subject {
    fn of(state: &AppState, presentation: &Presentation<NodeIx>, segment: Option<SegmentIndex>) -> Option<Self> {
        let folder = state.folder()?;
        let layout = &presentation.layout;
        let Some((index, found)) = segment.and_then(|index| layout.segments().get(index).map(|s| (index, s))) else {
            let meta = folder.folder_meta();
            let path = folder.folder_path();
            let name = folder.name(FolderTree::FOLDER);
            let name = match state.place_name(path) {
                Some(place) => place.to_owned(),
                None => theme::display(if name.is_empty() { path } else { name }),
            };
            return Some(Self {
                name: name.into(),
                size: meta.size,
                kind: SubjectKind::Folder,
                scan: meta.state,
                collection: state.collection_state(path),
            });
        };
        let collection = presentation.state(index);
        Some(match found.node {
            Some(ix) => {
                let meta = folder.meta(ix);
                Self {
                    name: theme::display(folder.name(ix)).into(),
                    size: found.size,
                    kind: SubjectKind::Node { is_dir: meta.flags.is_dir },
                    scan: meta.state,
                    collection,
                }
            }
            None => {
                let group = layout.group(index)?;
                let count = group.members.len();
                Self {
                    name: format!(
                        "{} grouped {}",
                        theme::format_count(count as u64),
                        plural(count as u64, "item", "items")
                    )
                    .into(),
                    size: found.size,
                    kind: SubjectKind::Group,
                    scan: if group.complete { ScanState::Complete } else { ScanState::Scanning },
                    collection,
                }
            }
        })
    }
}

fn plural(count: u64, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

fn center_text(state: &AppState, presentation: &Presentation<NodeIx>, active: Option<SegmentIndex>) -> CenterText {
    let can_go_up = state.can_go_up();
    let Some(subject) = Subject::of(state, presentation, active) else {
        return CenterText { can_go_up, ..CenterText::default() };
    };
    let Some(folder) = state.folder() else { return CenterText::default() };
    let root = folder.folder_meta();
    let updating = state.updating_removal_sizes()
        && match (&subject.kind, active.and_then(|index| presentation.layout.segments().get(index))) {
            (SubjectKind::Node { is_dir }, Some(segment)) => {
                segment.node.is_some_and(|ix| state.updating_removal_size(&folder.path(ix), *is_dir))
            }
            // The folder itself, or a group whose members it sums.
            _ => state.updating_removal_size(folder.folder_path(), true),
        };
    let value = if updating {
        "Updating…".to_owned()
    } else if subject.scan == ScanState::Pending {
        "Waiting…".to_owned()
    } else {
        theme::format_bytes(subject.size)
    };
    let detail = if updating {
        "Updating sizes".to_owned()
    } else if subject.scan == ScanState::Pending {
        "Waiting to map".to_owned()
    } else if subject.scan == ScanState::Scanning {
        "Mapped so far".to_owned()
    } else if subject.collection == CollectionState::Included {
        "In Collection".to_owned()
    } else if subject.collection == CollectionState::Partial {
        "Partly collected".to_owned()
    } else {
        match subject.kind {
            SubjectKind::Folder if root.size == 0 => "No measurable file data".to_owned(),
            SubjectKind::Folder => {
                format!("{} {}", theme::format_count(root.file_count), plural(root.file_count, "file", "files"))
            }
            SubjectKind::Group => "Drag these items to Collection".to_owned(),
            SubjectKind::Node { .. } => {
                let percent = theme::format_percent(subject.size as f64 / root.size.max(1) as f64);
                let total = if root.state == ScanState::Complete { "of this folder" } else { "of mapped bytes" };
                format!("{percent} {total}")
            }
        }
    };
    CenterText { name: subject.name, value: value.into(), detail: detail.into(), can_go_up }
}

impl Render for MapPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        self.sync(cx);
        let root = div().size_full().flex().flex_col().track_focus(&self.focus).on_key_down(cx.listener(
            |map, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && map.drag.is_some() {
                    map.cancel_drag(window, cx);
                    cx.stop_propagation();
                }
            },
        ));
        let state = self.host.state(cx);
        let Some(folder) = state.folder() else {
            if !(state.is_scanning() || state.is_opening()) {
                return root.child(welcome());
            }
            // Progress notifies this region only until the first folder is shown.
            let opening = Opening {
                title: state.title(),
                count: theme::format_count(state.progress_count()),
                bytes: if state.updating_removal_sizes() {
                    "Updating…".to_owned()
                } else {
                    theme::format_bytes(state.progress_bytes())
                },
                path: truncate_middle(
                    &theme::display(state.scan_path()),
                    px(OPENING_PATH_WIDTH),
                    theme::TEXT_SMALL,
                    gpui::FontWeight::NORMAL,
                    window,
                ),
            };
            return root.child(scanning(opening));
        };
        let meta = *folder.folder_meta();
        let updating = state.updating_removal_size(folder.folder_path(), true);
        let complete = meta.state == ScanState::Complete;
        let value = |bytes: u64| {
            if updating {
                "Updating…".to_owned()
            } else if meta.state == ScanState::Pending {
                "Waiting".to_owned()
            } else {
                theme::format_bytes(bytes)
            }
        };
        let (size, allocated) = (value(meta.size), value(meta.allocated));
        root.child(self.chart_element(cx)).child(
            div()
                .h(px(BOTTOM_HEIGHT))
                .flex_none()
                .pt(px(14.))
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .h(px(40.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(24.))
                        .child(metric(if complete { "File size" } else { "File size so far" }, size, true))
                        .child(div().w(px(1.)).h(px(30.)).bg(theme::white(0.14)))
                        .child(metric(if complete { "Allocated" } else { "Allocated so far" }, allocated, false)),
                )
                .child(
                    div()
                        .mt(px(12.))
                        .h(px(14.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(18.))
                        .text_size(theme::TEXT_CAPTION)
                        .text_color(theme::color(theme::SLATE))
                        .child(legend(Icon::CircleCheckFill, "In collection"))
                        .child(legend(Icon::CircleMinusFill, "Partly collected")),
                ),
        )
    }
}

fn metric(title: &'static str, value: String, trailing: bool) -> impl IntoElement {
    let align = |el: gpui::Div| if trailing { el.text_right() } else { el };
    div()
        .w(theme::MAP_METRIC_WIDTH)
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(align(
            div()
                .h(px(14.))
                .text_size(theme::TEXT_SMALL)
                .text_color(theme::color(theme::SLATE))
                .truncate()
                .child(title),
        ))
        .child(theme::monospaced_digits(align(
            div().h(px(22.)).text_size(theme::TEXT_METRIC).font_weight(theme::WEIGHT_SEMIBOLD).truncate().child(value),
        )))
}

fn legend(symbol: Icon, label: &'static str) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .child(icon(symbol).size(px(11.)).text_color(theme::color(theme::SLATE)))
        .child(label)
}

/// No folder yet: the single scan instruction (Swift `welcomeView`).
fn welcome() -> gpui::Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(22.))
        .child(icon(Icon::ChartPie).size(px(74.)).text_color(theme::color(theme::CORAL)))
        .child(
            div()
                .text_size(theme::TEXT_TRAY)
                .text_color(theme::color(theme::SLATE))
                .child("Choose a disk or folder to scan."),
        )
        .child(div().pt(px(6.)).child(button(
            "map-choose-folder",
            "Choose Folder",
            None,
            ButtonStyle::Prominent,
            true,
            |_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx),
        )))
}

/// Widest path line of the opening view (Swift `frame(maxWidth: 520)`).
const OPENING_PATH_WIDTH: f32 = 520.;

/// What the opening view shows before the first folder is available.
struct Opening {
    title: &'static str,
    count: String,
    bytes: String,
    /// The folder being read, middle-truncated.
    path: SharedString,
}

/// Opening or mapping before the first folder is available (Swift `scanningView`): the title,
/// items found and their file size so far, and the folder being read.
fn scanning(opening: Opening) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(20.))
        .px(px(24.))
        .child(icon(Icon::Loader).size(px(28.)).text_color(theme::color(theme::SLATE)))
        .child(div().text_size(px(26.)).font_weight(theme::WEIGHT_MEDIUM).child(opening.title))
        .child(
            div()
                .id("opening-metrics")
                .debug_selector(|| "opening-metrics".into())
                .flex()
                .flex_row()
                .gap(px(24.))
                .child(metric("Items found", opening.count, false))
                .child(metric("File size", opening.bytes, false)),
        )
        .child(
            div()
                .max_w(px(OPENING_PATH_WIDTH))
                .text_size(theme::TEXT_SMALL)
                .text_color(theme::color(theme::SLATE))
                .whitespace_nowrap()
                .overflow_hidden()
                .child(opening.path),
        )
}
