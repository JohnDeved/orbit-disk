//! PLACEHOLDER — map panel (owner: map panel agent).
//!
//! The explorer center: the sunburst plus the metrics row and collection legend below it.
//! Observes [`Regions::MAP`]. What is here is the minimum the frame harness needs: fills painted
//! from the cached [`Presentation`], a white hover outline, hover → `HoverSegment` /
//! `ClearMapHover`, click → `Open` / `Select` / `GoUp` (center). Still to do (see DESIGN.md):
//! collection hatching and badges, center labels and the Up slot, pointer cursors over sectors,
//! gesture hold (`GestureBegin/End`), drag to the tray, reduce-motion fill transitions, the
//! selection outline, compact center text for small windows.
//!
//! Geometry (flattened sector paths) is rebuilt only when the layout revision or the bounds
//! change; hover only repaints the outline.

use gpui::{
    App, Bounds, Context, Hsla, IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement, Path,
    PathBuilder, Pixels, Render, Styled, Subscription, Window, canvas, div, point, prelude::*, px,
};
use orbit_app_state::{Event, Regions};
use orbit_core::view::NodeIx;
use orbit_sunburst::{Hit, Metrics, Point as SPoint, Presentation, SegmentIndex};

use crate::components::placeholder_tag;
use crate::frame_stats;
use crate::host::Host;
use crate::theme;

pub const REGIONS: Regions = Regions::MAP;

struct Geometry {
    layout_revision: u64,
    bounds: Bounds<Pixels>,
    metrics: Metrics,
    fills: Vec<(Path<Pixels>, Hsla)>,
    outlines: Vec<Vec<gpui::Point<Pixels>>>,
}

pub struct MapPanel {
    host: Host,
    presentation: Option<Presentation<NodeIx>>,
    geometry: Option<Geometry>,
    /// The segment under the pointer (local; the state learns it through `HoverSegment`).
    hovered: Option<SegmentIndex>,
    /// The segment to outline for a hovered list row.
    row_hovered: Option<SegmentIndex>,
    _subscriptions: Vec<Subscription>,
}

impl MapPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self {
            host: host.clone(),
            presentation: None,
            geometry: None,
            hovered: None,
            row_hovered: None,
            _subscriptions: host.observe(REGIONS, cx),
        }
    }

    fn prepare(&mut self, bounds: Bounds<Pixels>) {
        let Some(presentation) = &self.presentation else {
            self.geometry = None;
            return;
        };
        if self
            .geometry
            .as_ref()
            .is_some_and(|g| g.bounds == bounds && g.layout_revision == presentation.layout_revision)
        {
            return;
        }
        let metrics = Metrics::from_bounds(
            f64::from(bounds.origin.x),
            f64::from(bounds.origin.y),
            f64::from(bounds.size.width),
            f64::from(bounds.size.height),
        );
        let layout = &presentation.layout;
        let mut fills = Vec::with_capacity(layout.len());
        let mut outlines = Vec::with_capacity(layout.len());
        let mut flat = Vec::new();
        for segment in layout.segments() {
            flat.clear();
            segment.sector(&metrics).flatten(0.25, &mut flat);
            let points: Vec<_> = flat.iter().map(|p| point(px(p.x as f32), px(p.y as f32))).collect();
            let mut builder = PathBuilder::fill();
            builder.add_polygon(&points, true);
            if let Ok(path) = builder.build() {
                fills.push((path, theme::segment_color(segment.color())));
            }
            outlines.push(points);
        }
        self.geometry =
            Some(Geometry { layout_revision: presentation.layout_revision, bounds, metrics, fills, outlines });
    }

    fn paint(&self, window: &mut Window) {
        let Some(geometry) = &self.geometry else { return };
        for (path, color) in &geometry.fills {
            window.paint_path(path.clone(), *color);
        }
        if let Some(index) = self.hovered.or(self.row_hovered)
            && let Some(outline) = geometry.outlines.get(index)
        {
            let mut builder = PathBuilder::stroke(px(2.));
            builder.add_polygon(outline, true);
            if let Ok(path) = builder.build() {
                window.paint_path(path, gpui::white());
            }
        }
    }

    fn hit(&self, position: gpui::Point<Pixels>) -> Hit {
        match (&self.geometry, &self.presentation) {
            (Some(geometry), Some(presentation)) => {
                presentation.layout.hit(&geometry.metrics, SPoint::new(f64::from(position.x), f64::from(position.y)))
            }
            _ => Hit::None,
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.pointer_moved(event.position, cx);
    }

    /// The pointer moved over the map (mouse-move handler; the frame harness calls it directly).
    pub fn pointer_moved(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let hovered = match self.hit(position) {
            Hit::Segment(index) => Some(index),
            Hit::Center | Hit::None => None,
        };
        if hovered == self.hovered {
            return;
        }
        self.hovered = hovered;
        let event = match (hovered, &self.presentation) {
            (Some(segment), Some(p)) => Event::HoverSegment { segment, layout_revision: p.layout_revision },
            _ => Event::ClearMapHover,
        };
        self.host.send(event, cx);
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let hit = self.hit(event.position);
        let Some(presentation) = self.presentation.clone() else { return };
        let event = match hit {
            Hit::Center => Event::GoUp,
            Hit::Segment(index) => {
                let Some(node) = presentation.layout.segments()[index].node else { return };
                let state = self.host.state(cx);
                let Some(folder) = state.folder() else { return };
                let item = folder.item(node);
                if item.is_dir() { Event::Open { path: item.path } } else { Event::Select { path: Some(item.path) } }
            }
            Hit::None => return,
        };
        self.host.send(event, cx);
    }

    fn clear_hover(&mut self, cx: &mut Context<Self>) {
        if self.hovered.take().is_some() {
            self.host.send(Event::ClearMapHover, cx);
            cx.notify();
        }
    }
}

impl Render for MapPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let (presentation, row_hovered) =
            self.host.update(cx, |host, _| (host.presentation(), host.row_hover_segment()));
        if presentation.as_ref().map(|p| p.layout_revision) != self.presentation.as_ref().map(|p| p.layout_revision) {
            self.hovered = None;
        }
        self.presentation = presentation;
        self.row_hovered = row_hovered;
        let state = self.host.state(cx);
        let folder = state.folder().map(|f| (*f.folder_meta(), f.rows().len()));
        let entity = cx.entity();
        let chart = canvas(
            {
                let entity = entity.clone();
                move |bounds, _, cx: &mut App| entity.update(cx, |map, _| map.prepare(bounds))
            },
            move |_, (), window, cx| entity.read(cx).paint(window),
        )
        .size_full();
        let (size, allocated) = folder.map_or((String::new(), String::new()), |(meta, _)| {
            (theme::format_bytes(meta.size), theme::format_bytes(meta.allocated))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("map-chart")
                    .flex_1()
                    .min_h_0()
                    .mx(px(16.))
                    .child(chart)
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_hover(cx.listener(|map, hovered: &bool, _, cx| {
                        if !*hovered {
                            map.clear_hover(cx);
                        }
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_center()
                    .gap(px(24.))
                    .pb(px(12.))
                    .child(metric("File size", size))
                    .child(metric("Allocated", allocated)),
            )
            .child(div().flex().justify_center().pb(px(10.)).child(placeholder_tag("map panel placeholder")))
    }
}

fn metric(title: &'static str, value: String) -> impl IntoElement {
    div()
        .w(theme::MAP_METRIC_WIDTH)
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.))
        .child(div().text_size(theme::TEXT_SMALL).text_color(theme::color(theme::SLATE)).child(title))
        .child(theme::monospaced_digits(
            div().text_size(theme::TEXT_METRIC).font_weight(theme::WEIGHT_MEDIUM).h(px(22.)).child(value),
        ))
}
