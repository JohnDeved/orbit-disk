//! The overlay layer: transient visuals above the panels that follow the pointer or travel
//! between panels, drawn by one view mounted over the whole window (below the sheets).
//!
//! - **Map tooltip.** Hovering a map segment shows a card centered above the pointer in the look
//!   of a Contents row: branch swatch, name, the row's secondary line (plus where the item is,
//!   for rings below the shown folder), size and share. The map center keeps describing the
//!   folder (or the selection), so the two never repeat each other. The tooltip appears with a
//!   short fade ([`theme::TIP_FADE`]), changes in place between segments, goes below the pointer
//!   near the top edge and stays inside the window. Moving the pointer re-renders only this
//!   layer.
//! - **Flights.** A card dropped on the tray flies into the tray's glyph, shrinking and taking
//!   on the coral of the target ([`theme::DROP_FLIGHT`]); a drag released anywhere else goes
//!   back into the row or segment it was pulled from ([`theme::DRAG_RETURN`]). Reduce Motion
//!   skips both.
//!
//! Panels reach the layer through [`OverlayLayer::global`] (it is not a panel and holds no app
//! state of its own); without a main window (tests of single panels) every call is a no-op.

use std::time::Instant;

use gpui::{
    App, Bounds, Context, Entity, Global, Hsla, IntoElement, ParentElement, Pixels, Point, Render, SharedString,
    Styled, WeakEntity, Window, div, point, px, size,
};
use orbit_sunburst::CollectionState;

use crate::components::{CollectionDrag, DragPreview, drag_card};
use crate::ghost::{GhostLook, Pose, lifted_row, list_row, row_card};
use crate::host::Host;
use crate::motion::{Spring, ease_in_out, ease_out, lerp, lerp_bounds, lerp_color, progress};
use crate::theme;

/// The tooltip card's fixed size: moving between segments never resizes it.
pub const TIP_SIZE: (f32, f32) = (300., 52.);
/// Gap between the pointer and the tooltip's bottom edge (centered above it), and between the
/// pointer and its top edge when there is no room above (below it, clearing the arrow cursor).
const TIP_GAP_ABOVE: f32 = 12.;
const TIP_GAP_BELOW: f32 = 22.;
/// Closest the tooltip comes to the window edge.
const TIP_MARGIN: f32 = 8.;

/// What the map tooltip shows, formatted like a Contents row.
#[derive(Clone, Debug, PartialEq)]
pub struct TipContent {
    pub swatch: Hsla,
    pub name: SharedString,
    pub mark: CollectionState,
    /// The row's secondary line ("479 files", "ZIP"), with the location for deeper rings.
    pub detail: SharedString,
    pub size: SharedString,
    /// Share of the shown folder ("15.6%"), "so far", or empty.
    pub share: SharedString,
}

struct Tip {
    content: TipContent,
    pointer: Point<Pixels>,
    /// When the fade-in started; `None` when it appeared without one.
    fade: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlightKind {
    /// Into the tray's glyph.
    Collect,
    /// Back to where the drag started.
    Return,
}

/// What runs when a flight lands.
type Landing = Box<dyn FnOnce(&mut App)>;

struct Flight {
    card: CollectionDrag,
    from: Bounds<Pixels>,
    to: Bounds<Pixels>,
    /// The pointer at the release.
    release: Point<Pixels>,
    start: Instant,
    duration: std::time::Duration,
    kind: FlightKind,
    /// Runs on the frame the flight lands: the source shows its element again.
    on_land: Option<Landing>,
    /// Landed: drawn once more in its final pose (the frame the source reappears), then gone.
    landed: bool,
}

impl Flight {
    fn new(
        card: CollectionDrag,
        from: Bounds<Pixels>,
        to: Bounds<Pixels>,
        release: Point<Pixels>,
        kind: FlightKind,
        duration: std::time::Duration,
        on_land: Option<Landing>,
    ) -> Self {
        Self { card, from, to, release, start: Instant::now(), duration, kind, on_land, landed: false }
    }
}

/// How long a returning drag released at `release` takes to settle: as long as its spring
/// needs for the distance (the plain card uses [`theme::DRAG_RETURN`]).
fn return_duration(card: &CollectionDrag, release: Point<Pixels>) -> std::time::Duration {
    match &card.ghost {
        Some(ghost) => {
            let delta = release - ghost.grab;
            let distance = f32::from(delta.x).hypot(f32::from(delta.y));
            std::time::Duration::from_secs_f32(Spring::RETURN.settle_time(distance))
        }
        None => theme::DRAG_RETURN,
    }
}

struct OverlayHandle(WeakEntity<OverlayLayer>);

impl Global for OverlayHandle {}

pub struct OverlayLayer {
    host: Host,
    tip: Option<Tip>,
    /// When the tooltip was last hidden (a quick return skips the fade).
    hidden_at: Option<Instant>,
    flights: Vec<Flight>,
}

impl OverlayLayer {
    pub fn new(host: &Host) -> Self {
        Self { host: host.clone(), tip: None, hidden_at: None, flights: Vec::new() }
    }

    /// Makes `layer` the window's overlay for [`Self::global`].
    pub fn install(layer: &Entity<Self>, cx: &mut App) {
        cx.set_global(OverlayHandle(layer.downgrade()));
    }

    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<OverlayHandle>().and_then(|handle| handle.0.upgrade())
    }

    /// The current drag would be collected if released now ("Release to collect").
    pub fn drop_target_active(&self, cx: &App) -> bool {
        self.host.state(cx).drop_target_active()
    }

    /// [`Self::drop_target_active`] of the installed layer (`false` without one).
    pub fn drop_target_active_in(cx: &App) -> bool {
        Self::global(cx).is_some_and(|layer| layer.read(cx).drop_target_active(cx))
    }

    // Tooltip.

    /// Shows `content` next to `pointer`; an already visible tooltip changes in place.
    pub fn show_tip(&mut self, content: TipContent, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let now = Instant::now();
        match &mut self.tip {
            Some(tip) => {
                if tip.content == content && tip.pointer == pointer {
                    return;
                }
                tip.content = content;
                tip.pointer = pointer;
            }
            None => {
                let quick = self.hidden_at.is_some_and(|at| now.saturating_duration_since(at) < theme::TIP_REAPPEAR);
                let fade = (!quick && !theme::reduce_motion(cx)).then_some(now);
                self.tip = Some(Tip { content, pointer, fade });
            }
        }
        cx.notify();
    }

    /// Follows the pointer (no-op while hidden).
    pub fn move_tip(&mut self, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(tip) = &mut self.tip
            && tip.pointer != pointer
        {
            tip.pointer = pointer;
            cx.notify();
        }
    }

    pub fn hide_tip(&mut self, cx: &mut Context<Self>) {
        if self.tip.take().is_some() {
            self.hidden_at = Some(Instant::now());
            cx.notify();
        }
    }

    pub fn tip(&self) -> Option<&TipContent> {
        self.tip.as_ref().map(|tip| &tip.content)
    }

    // Flights.

    /// A card released at `pointer` flies into `target` (the tray glyph's bounds).
    pub fn collect(
        &mut self,
        card: CollectionDrag,
        pointer: Point<Pixels>,
        target: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let from = DragPreview::payload_bounds(&card, pointer);
        self.launch(Flight::new(card, from, target, pointer, FlightKind::Collect, theme::DROP_FLIGHT, None), cx);
    }

    /// A drag released at `pointer` without a drop springs back to where it was grabbed and
    /// settles into its place (a plain card returns to `origin`, the pointer where the drag
    /// started). `on_land` runs on the frame it lands, so its source can show the element again
    /// exactly then (at once without motion).
    pub fn give_back(
        &mut self,
        card: CollectionDrag,
        pointer: Point<Pixels>,
        origin: Point<Pixels>,
        on_land: impl FnOnce(&mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        let (from, to) = (DragPreview::card_bounds(pointer), DragPreview::card_bounds(origin));
        let duration = return_duration(&card, pointer);
        let landing: Landing = Box::new(on_land);
        self.launch(Flight::new(card, from, to, pointer, FlightKind::Return, duration, Some(landing)), cx);
    }

    fn launch(&mut self, mut flight: Flight, cx: &mut Context<Self>) {
        if theme::reduce_motion(cx) {
            if let Some(on_land) = flight.on_land.take() {
                cx.defer(on_land);
            }
            return;
        }
        self.flights.push(flight);
        cx.notify();
    }

    /// Flights still on screen (tests).
    pub fn flights(&self) -> Vec<FlightKind> {
        self.flights.iter().map(|flight| flight.kind).collect()
    }
}

impl Render for OverlayLayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        // A landed flight is drawn once more, in the frame its source shows the element again.
        self.flights.retain(|flight| !flight.landed);
        for flight in &mut self.flights {
            if progress(flight.start, flight.duration, now) >= 1.0 {
                flight.landed = true;
                if let Some(on_land) = flight.on_land.take() {
                    cx.defer(on_land);
                }
            }
        }
        let mut animating = !self.flights.is_empty();
        let tip = self.tip.as_ref().map(|tip| {
            let opacity = tip.fade.map_or(1.0, |start| ease_out(progress(start, theme::TIP_FADE, now)));
            animating |= opacity < 1.0;
            tip_element(&tip.content, tip_origin(tip.pointer, window.viewport_size()), opacity)
        });
        let flights: Vec<_> = self.flights.iter().map(|flight| flight_element(flight, now)).collect();
        if animating {
            window.request_animation_frame();
        }
        // No listeners: the layer never takes the pointer from the panels below.
        div().absolute().top_0().left_0().size_full().font_family(theme::FONT_FAMILY).children(tip).children(flights)
    }
}

/// The tooltip's top-left for `pointer`: centered above it, below it when there is no room
/// above, and kept inside the window.
pub fn tip_origin(pointer: Point<Pixels>, viewport: gpui::Size<Pixels>) -> Point<Pixels> {
    let (width, height) = TIP_SIZE;
    let (pointer_x, pointer_y) = (f32::from(pointer.x), f32::from(pointer.y));
    let max_x = f32::from(viewport.width) - TIP_MARGIN;
    let x = (pointer_x - width / 2.).min(max_x - width).max(TIP_MARGIN);
    let mut y = pointer_y - TIP_GAP_ABOVE - height;
    if y < TIP_MARGIN {
        y = pointer_y + TIP_GAP_BELOW;
    }
    point(px(x), px(y))
}

/// The tooltip card: a Contents row lifted off the list.
pub fn tip_element(content: &TipContent, origin: Point<Pixels>, opacity: f32) -> gpui::Div {
    let (width, height) = TIP_SIZE;
    row_card(content, px(width), px(height)).absolute().left(origin.x).top(origin.y).opacity(opacity)
}

/// One frame of a flight.
fn flight_element(flight: &Flight, now: Instant) -> gpui::Div {
    let t = progress(flight.start, flight.duration, now);
    flight_frame(&flight.card, flight.from, flight.to, flight.release, flight.kind, t)
}

/// The frame at `t` (0..=1) of `card` released at `release`, flying from `from` to `to` (also
/// drawn by the gallery). A grabbed element ([`crate::ghost`]) returns by sliding back to where
/// it was grabbed and settling into place; `from` and `to` only move a plain card.
pub fn flight_frame(
    card: &CollectionDrag,
    from: Bounds<Pixels>,
    to: Bounds<Pixels>,
    release: Point<Pixels>,
    kind: FlightKind,
    t: f32,
) -> gpui::Div {
    match kind {
        FlightKind::Collect => {
            // The card is tossed into the glyph: it shrinks faster than it travels, along a
            // slight arc; its text goes first, then it takes on the target's coral and fades as
            // it lands. It keeps the coral border it had over the tray.
            let travel = ease_in_out(t);
            let shrink = ease_out(t);
            let center = |b: Bounds<Pixels>| (f32::from(b.center().x), f32::from(b.center().y));
            let ((fx, fy), (tx, ty)) = (center(from), center(to));
            let arc = (std::f32::consts::PI * travel).sin() * 36.;
            let (cx, cy) = (lerp(fx, tx, travel), lerp(fy, ty, travel) - arc);
            let width = lerp(f32::from(from.size.width), f32::from(to.size.width), shrink);
            let height = lerp(f32::from(from.size.height), f32::from(to.size.height), shrink);
            let text = 1.0 - ease_out((t / 0.3).min(1.0));
            let tint = ease_in_out(((t - 0.15) / 0.55).clamp(0.0, 1.0));
            let opacity = 1.0 - ease_out(((t - 0.75) / 0.25).clamp(0.0, 1.0));
            let card_bg = theme::with_alpha(0x242424, 0.98).into();
            let coral = theme::with_alpha(theme::CORAL, 0.92).into();
            let frame = div()
                .absolute()
                .left(px(cx - width / 2.))
                .top(px(cy - height / 2.))
                .w(px(width))
                .h(px(height))
                .overflow_hidden()
                .rounded(px(lerp(8., 9., shrink)))
                .bg(lerp_color(card_bg, coral, tint))
                .border_1()
                .border_color(theme::color(theme::CORAL))
                .opacity(opacity)
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::hsla(0., 0., 0., 0.4 * (1. - travel)),
                    offset: point(px(0.), px(8.)),
                    blur_radius: px(20.),
                    spread_radius: px(0.),
                }])
                .child(
                    // The card's contents at full size, clipped by the shrinking frame.
                    div().absolute().top(px(-1.)).left(px(-1.)).opacity(text).child(match &card.ghost {
                        // Exactly what was carried: the lifted row, or the segment's label.
                        Some(ghost) => match ghost.look {
                            GhostLook::Row { folder, .. } => {
                                list_row(&ghost.label, folder, Pose { lift: 1.0, over: 1.0 })
                                    .w(from.size.width)
                                    .h(from.size.height)
                                    .shadow(vec![])
                            }
                            GhostLook::Segment { .. } => lifted_row(&ghost.label, Pose { lift: 1.0, over: 1.0 })
                                .w(from.size.width)
                                .h(from.size.height)
                                .shadow(vec![]),
                        },
                        None => drag_card(card, 0.0, 0.0).border_color(gpui::transparent_black()).shadow(vec![]),
                    }),
                );
            // A lifted segment's arc fades where it was released while its label flies.
            let arc = card.ghost.as_ref().filter(|g| matches!(g.look, GhostLook::Segment { .. })).map(|ghost| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .opacity(1.0 - ease_out((t / 0.3).min(1.0)))
                    .child(ghost.element(release, Pose { lift: 1.0, over: 1.0 }))
            });
            div().absolute().top_0().left_0().size_full().children(arc).child(frame)
        }
        FlightKind::Return => match &card.ghost {
            // The grabbed element springs back to where it was picked up (a slight overshoot,
            // then still) and sets down as it arrives, ending exactly as its source looks.
            Some(ghost) => {
                let elapsed = t * return_duration(card, release).as_secs_f32();
                let left = Spring::RETURN.remaining(elapsed);
                let left = if t >= 1.0 { 0.0 } else { left };
                let pointer = ghost.grab + (release - ghost.grab) * left;
                let lift = ease_out((left / 0.35).clamp(0.0, 1.0));
                div().absolute().top_0().left_0().size_full().child(ghost.element(pointer, Pose { lift, over: 0.0 }))
            }
            None => {
                let travel = ease_out(t);
                let bounds = lerp_bounds(from, to, travel);
                let opacity = 1.0 - ease_out(((t - 0.35) / 0.65).clamp(0.0, 1.0));
                div().absolute().left(bounds.origin.x).top(bounds.origin.y).opacity(opacity).child(drag_card(
                    card,
                    0.0,
                    1.0 - travel,
                ))
            }
        },
    }
}

/// Bounds of a `side`-point square centered on `center` (a flight's target).
pub fn square(center: Point<Pixels>, side: f32) -> Bounds<Pixels> {
    Bounds::new(point(center.x - px(side / 2.), center.y - px(side / 2.)), size(px(side), px(side)))
}

#[cfg(test)]
mod tests {
    use gpui::{point, px, size};

    use super::{TIP_SIZE, tip_origin};

    #[test]
    fn the_tooltip_is_centered_above_the_pointer_and_stays_inside() {
        let viewport = size(px(1280.), px(810.));
        let above = tip_origin(point(px(600.), px(300.)), viewport);
        assert_eq!(above, point(px(600. - TIP_SIZE.0 / 2.), px(300. - 12. - TIP_SIZE.1)), "centered above the pointer");
        let right = tip_origin(point(px(1270.), px(100.)), viewport);
        assert_eq!(right.x, px(1280. - 8. - TIP_SIZE.0), "kept inside at the right edge");
        assert_eq!(tip_origin(point(px(5.), px(100.)), viewport).x, px(8.), "and at the left edge");
        let top = tip_origin(point(px(600.), px(30.)), viewport);
        assert_eq!(top.y, px(30. + 22.), "below the pointer at the top edge");
    }
}
