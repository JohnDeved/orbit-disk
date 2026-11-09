//! The explorer center: the sunburst, the metrics row and the collection legend (Swift
//! `SunburstView`, `MapInteractionView`, `ContentView.explorer`). Observes [`Regions::MAP`].
//!
//! Drawing (one canvas, `DESIGN.md`: "the colored map and collection marks retain their drawing
//! layer; hover and selection update a separate outline layer"):
//! - base layer: [`layers::Fills`] keyed by (layout revision, bounds) and [`layers::Marks`] keyed
//!   additionally by the presentation revision. Neither is rebuilt by hover or selection; a
//!   collection change rebuilds only the marks. Painting clones a few dozen cached paths.
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
pub mod layers;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Context, CursorStyle, DispatchPhase, Entity, FocusHandle, Hitbox,
    HitboxBehavior, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Path, Pixels, Point, Render, SharedString, Styled, Subscription, Window, canvas, div,
    fill, point, prelude::*, px, size,
};
use orbit_app_state::gesture::exceeds_drag_threshold;
use orbit_app_state::{AppState, DragId, DragSource, Event, GestureId, Regions};
use orbit_core::types::ScanState;
use orbit_core::view::{FolderTree, NodeIx};
use orbit_sunburst::{CollectionState, Hit, Metrics, Point as SPoint, Presentation, SegmentIndex};

pub use center::CenterText;
use layers::{Fills, Marks};

use crate::actions::OpenFolder;
use crate::components::{ButtonStyle, CollectionDrag, DragPreview, button};
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
    pub geometry_builds: u64,
    pub marks_builds: u64,
    pub outline_builds: u64,
    pub last_geometry: Duration,
    pub last_marks: Duration,
    pub last_outline: Duration,
}

/// The base layer of the current layout at the current bounds.
struct Base {
    layout_revision: u64,
    bounds: Bounds<Pixels>,
    fills: Fills,
    /// Presentation revision the marks were built for.
    marks_revision: u64,
    marks: Marks,
}

struct Outlines {
    key: (u64, Bounds<Pixels>, [Option<SegmentIndex>; 2]),
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
    outlines: Option<Outlines>,
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
            outlines: None,
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
            stats: MapStats::default(),
            next_id: ID_BASE,
            _subscriptions: host.observe(REGIONS, cx),
        }
    }

    /// Layer rebuild counters (tests, measurements).
    pub fn stats(&self) -> MapStats {
        self.stats
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
        let Some(presentation) = &self.presentation else {
            self.base = None;
            self.outlines = None;
            return;
        };
        let fresh = self
            .base
            .as_ref()
            .is_some_and(|base| base.layout_revision == presentation.layout_revision && base.bounds == bounds);
        if !fresh {
            let start = Instant::now();
            let fills = Fills::build(&presentation.layout, &metrics);
            self.stats.geometry_builds += 1;
            self.stats.last_geometry = start.elapsed();
            self.base = Some(Base {
                layout_revision: presentation.layout_revision,
                bounds,
                fills,
                marks_revision: 0,
                marks: Marks::default(),
            });
        }
        if let Some(base) = self.base.as_mut()
            && base.marks_revision != presentation.revision
        {
            let start = Instant::now();
            base.marks = Marks::build(&presentation.layout, &presentation.states, &metrics);
            base.marks_revision = presentation.revision;
            self.stats.marks_builds += 1;
            self.stats.last_marks = start.elapsed();
        }
        let segments = self.outlined();
        let key = (presentation.layout_revision, bounds, segments);
        if self.outlines.as_ref().is_none_or(|outlines| outlines.key != key) {
            let start = Instant::now();
            let paths = segments
                .iter()
                .flatten()
                .filter_map(|&index| layers::outline(&presentation.layout, &metrics, index))
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

    fn paint(&self, window: &mut Window) {
        if let Some(base) = &self.base {
            for (path, color) in &base.fills.paths {
                window.paint_path(path.clone(), *color);
            }
            let ink = theme::color(theme::INK);
            for path in base.marks.hatch.iter().chain(&base.marks.badges) {
                window.paint_path(path.clone(), ink);
            }
            for path in &base.marks.glyphs {
                window.paint_path(path.clone(), theme::color(theme::TEXT));
            }
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
        (map.presentation.is_some().then(|| map.center.clone()), map.chart.map(|(_, metrics)| metrics))
    });
    let center = text.zip(metrics).map(|(text, metrics)| {
        let radius = metrics.inner_radius as f32;
        let mut element = center::element(&text, radius);
        let available = size(AvailableSpace::Definite(px(radius * 1.65)), AvailableSpace::MinContent);
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
            return Some(Self {
                name: theme::display(if name.is_empty() { path } else { name }).into(),
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
    let updating = state.updating_removal_sizes();
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
                let percent = subject.size as f64 / root.size.max(1) as f64 * 100.0;
                let total = if root.state == ScanState::Complete { "of this folder" } else { "of mapped bytes" };
                format!("{percent:.1}% {total}")
            }
        }
    };
    CenterText { name: subject.name, value: value.into(), detail: detail.into(), can_go_up }
}

impl Render for MapPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            let waiting = state.is_scanning() || state.is_opening();
            let title = state.title();
            let path: SharedString = theme::display(state.scan_path()).into();
            return root.child(if waiting { scanning(title, path) } else { welcome() });
        };
        let meta = *folder.folder_meta();
        let updating = state.updating_removal_sizes();
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
            div().h(px(22.)).text_size(theme::TEXT_METRIC).font_weight(theme::WEIGHT_MEDIUM).truncate().child(value),
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

/// Opening or mapping before the first folder is available (Swift `scanningView`; counts are
/// in the footer, which observes progress).
fn scanning(title: &'static str, path: SharedString) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(20.))
        .px(px(24.))
        .child(icon(Icon::Loader).size(px(28.)).text_color(theme::color(theme::SLATE)))
        .child(div().text_size(px(26.)).font_weight(theme::WEIGHT_MEDIUM).child(title))
        .child(
            div()
                .max_w(px(520.))
                .text_size(theme::TEXT_SMALL)
                .text_color(theme::color(theme::SLATE))
                .truncate()
                .child(path),
        )
}
