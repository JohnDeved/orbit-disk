//! Small shared building blocks (buttons, dividers, labels). Panels may add their own; anything
//! two panels need belongs here.

use gpui::{
    App, ClickEvent, CursorStyle, Div, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use unicode_segmentation::UnicodeSegmentation;

use crate::icons::{Icon, icon};
use crate::theme;

/// Visual weight of a [`button`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Bordered neutral button (native `.bordered`).
    Bordered,
    /// Bordered button with the accent tint (native `.bordered` under `.tint(accent)`): coral
    /// label and icon on a faint warm fill — the explorer's actions (Up, inspector, tray).
    Tinted,
    /// Filled coral button (native `.borderedProminent` with the accent tint).
    Prominent,
    /// Text-only.
    Plain,
}

/// The faint warm fill of [`ButtonStyle::Tinted`] buttons.
const TINT_FILL: u32 = 0xffd2c4;

/// A labeled button with an optional leading icon. Disabled buttons keep their size, use the
/// default cursor and ignore clicks. Mouse-down does not propagate (title bar drags, rows).
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    leading: Option<Icon>,
    style: ButtonStyle,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (bg, hover_bg, fg) = match style {
        ButtonStyle::Bordered => (theme::white(0.08), theme::white(0.14), theme::color(theme::TEXT)),
        ButtonStyle::Tinted => {
            (theme::with_alpha(TINT_FILL, 0.10), theme::with_alpha(TINT_FILL, 0.16), theme::color(theme::CORAL))
        }
        ButtonStyle::Prominent => {
            (theme::color(theme::CORAL), theme::with_alpha(theme::CORAL, 0.85), theme::color(theme::CANVAS))
        }
        ButtonStyle::Plain => (theme::white(0.0), theme::white(0.07), theme::color(theme::TEXT)),
    };
    // Disabled: native dims the label; a prominent button keeps a muted coral fill.
    let (bg, fg) = match (enabled, style) {
        (true, _) => (bg, fg),
        (false, ButtonStyle::Prominent) => (theme::with_alpha(theme::CORAL, 0.42), theme::color(theme::SLATE)),
        (false, ButtonStyle::Tinted) => (theme::with_alpha(TINT_FILL, 0.05), theme::with_alpha(theme::CORAL, 0.38)),
        (false, _) => (bg, theme::with_alpha(theme::SLATE, 0.6)),
    };
    let mut element = div()
        .id(id.into())
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h(px(28.))
        .px(px(10.))
        .rounded(px(6.))
        // The prominent action is a native capsule.
        .when(style == ButtonStyle::Prominent, |el| el.rounded_full())
        .bg(bg)
        .text_color(fg)
        .text_size(theme::TEXT_ROW)
        .when_some(leading, |el, leading| el.child(icon(leading).text_color(fg)))
        .child(label.into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    if enabled {
        element = element.cursor(CursorStyle::PointingHand).hover(move |s| s.bg(hover_bg)).on_click(on_click);
    }
    element
}

/// A 1-point hairline (horizontal when `vertical == false`).
pub fn hairline(vertical: bool) -> Div {
    let line = div().flex_none().bg(theme::white(theme::DIVIDER_ALPHA));
    if vertical { line.w(px(1.)).h_full() } else { line.h(px(1.)).w_full() }
}

/// A small uppercase-free section label (sidebar "Devices", "Places").
pub fn section_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(theme::TEXT_SMALL)
        .font_weight(theme::WEIGHT_SEMIBOLD)
        .text_color(theme::color(theme::SLATE))
        .child(text.into())
}

/// Marks a placeholder region so it is obvious on screen which agent owns it.
pub fn placeholder_tag(owner: &'static str) -> Div {
    div()
        .text_size(theme::TEXT_CAPTION)
        .text_color(theme::with_alpha(theme::SLATE, 0.5))
        .child(SharedString::new_static(owner))
}

use gpui::prelude::FluentBuilder as _;

// --- map panel ---

/// Payload of an in-app collection drag (GPUI typed drag). The app state already holds the real
/// source (`Event::DragBegin`); a drop target only needs the id: `on_drop::<CollectionDrag>` →
/// `Event::DragDrop { drag }`, `on_drag_move::<CollectionDrag>` → `Event::DragOverTray`.
#[derive(Clone, Debug)]
pub struct CollectionDrag {
    pub drag: orbit_app_state::DragId,
    pub label: SharedString,
    /// Secondary line (the dragged size).
    pub detail: SharedString,
    pub icon: Icon,
    /// The grabbed element the drag carries ([`crate::ghost`]); without one, a plain card
    /// beside the pointer.
    pub ghost: Option<crate::ghost::Ghost>,
}

/// The drag preview (Swift `dragPreview`): the grabbed element itself, attached to the pointer
/// where it was grabbed ([`crate::ghost`]). It lifts off over [`theme::DRAG_LIFT`] and, while
/// the tray would accept it, eases into the "Release to collect" look ([`theme::DROP_TARGET_FADE`]).
/// GPUI draws the active drag view every frame at `pointer − offset`, so it reads the pointer
/// and the drop state itself ([`crate::overlay::OverlayLayer::drop_target_active`]). A drag
/// without a ghost shows a 240 × 56 card 12 points right of the pointer.
pub struct DragPreview {
    payload: CollectionDrag,
    offset: gpui::Point<gpui::Pixels>,
    lifted: std::time::Instant,
    over: Option<crate::motion::Transition>,
}

impl DragPreview {
    pub const SIZE: (f32, f32) = (240., 56.);
    /// The plain card's top-left relative to the pointer.
    pub const POINTER_GAP: (f32, f32) = (12., -28.);

    pub fn new(payload: &CollectionDrag, offset: gpui::Point<gpui::Pixels>) -> Self {
        Self { payload: payload.clone(), offset, lifted: std::time::Instant::now(), over: None }
    }

    /// The plain card's bounds for a pointer at `pointer`.
    pub fn card_bounds(pointer: gpui::Point<gpui::Pixels>) -> gpui::Bounds<gpui::Pixels> {
        let (width, height) = Self::SIZE;
        let (dx, dy) = Self::POINTER_GAP;
        gpui::Bounds::new(gpui::point(pointer.x + px(dx), pointer.y + px(dy)), gpui::size(px(width), px(height)))
    }

    /// Where `payload`'s card is with the pointer at `pointer` (where a drop flight starts).
    pub fn payload_bounds(payload: &CollectionDrag, pointer: gpui::Point<gpui::Pixels>) -> gpui::Bounds<gpui::Pixels> {
        payload.ghost.as_ref().map_or_else(|| Self::card_bounds(pointer), |ghost| ghost.card_bounds(pointer))
    }
}

impl gpui::Render for DragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        use crate::motion::{Transition, ease_out, progress};
        let now = std::time::Instant::now();
        let animate = !theme::reduce_motion(cx);
        let target = if crate::overlay::OverlayLayer::drop_target_active_in(cx) { 1.0 } else { 0.0 };
        let over = self.over.get_or_insert_with(|| Transition::settled(target, theme::DROP_TARGET_FADE, now));
        over.retarget(target, animate, now);
        let lift = if animate { progress(self.lifted, theme::DRAG_LIFT, now) } else { 1.0 };
        if animate && (lift < 1.0 || over.running(now)) {
            window.request_animation_frame();
        }
        let over = over.value(now);
        // GPUI draws this view at `pointer − offset`: a layer moved back by that much has the
        // window's origin, so the ghost is placed in window coordinates.
        let pointer = window.mouse_position();
        let view_origin = pointer - self.offset;
        let layer = div()
            .absolute()
            .left(-view_origin.x)
            .top(-view_origin.y)
            .w(window.viewport_size().width)
            .h(window.viewport_size().height)
            .font_family(theme::FONT_FAMILY);
        let layer = match &self.payload.ghost {
            Some(ghost) => layer.child(ghost.element(pointer, crate::ghost::Pose { lift, over })),
            None => {
                let card = Self::card_bounds(pointer);
                layer.child(
                    div().absolute().left(card.origin.x).top(card.origin.y).opacity(ease_out(lift)).child(drag_card(
                        &self.payload,
                        over,
                        ease_out(lift),
                    )),
                )
            }
        };
        div().relative().child(layer)
    }
}

/// The drag card itself: icon, name and size. `over` (0..=1) blends in the drop-target look,
/// `lift` (0..=1) the shadow of a card held above the window.
pub fn drag_card(payload: &CollectionDrag, over: f32, lift: f32) -> Div {
    let (width, height) = DragPreview::SIZE;
    let border = crate::motion::lerp_color(theme::color(0x858585).into(), theme::color(theme::CORAL).into(), over);
    div()
        .relative()
        .w(px(width))
        .h(px(height))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(11.))
        .pl(px(13.))
        .pr(px(14.))
        .rounded(px(8.))
        .bg(theme::with_alpha(0x242424, 0.98))
        .border_1()
        .border_color(border)
        .shadow(vec![gpui::BoxShadow {
            color: gpui::hsla(0., 0., 0., 0.45 * lift),
            offset: gpui::point(px(0.), px(4. + 6. * lift)),
            blur_radius: px(10. + 12. * lift),
            spread_radius: px(0.),
        }])
        .child(icon(payload.icon).size(px(20.)).text_color(theme::color(theme::TEXT)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(theme::TEXT_ROW)
                        .font_weight(theme::WEIGHT_MEDIUM)
                        .text_color(theme::color(theme::TEXT))
                        .truncate()
                        .child(payload.label.clone()),
                )
                .child(
                    div()
                        .text_size(theme::TEXT_SMALL)
                        .text_color(theme::color(0xc7c7c7))
                        .truncate()
                        .child(payload.detail.clone()),
                ),
        )
        .when(over > 0.0, |el| el.child(crate::ghost::plus_badge(over)))
}

// --- chrome panels ---
// Shared by the toolbar, sidebar, footer and tray. The tray accepts the map panel's
// [`CollectionDrag`].

/// A compact bordered button (native `.controlSize(.small)`): 20 points tall and `width` wide, so
/// changing its label never moves its neighbours.
pub fn small_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    width: gpui::Pixels,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    let fg = if enabled { theme::color(theme::TEXT) } else { theme::with_alpha(theme::SLATE, 0.6) };
    let element = div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(width)
        .h(px(20.))
        .rounded(px(5.))
        .bg(theme::white(0.08))
        .text_color(fg)
        .text_size(theme::TEXT_SMALL)
        .child(label.into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    if enabled {
        element.cursor(CursorStyle::PointingHand).hover(|s| s.bg(theme::white(0.14))).on_click(on_click)
    } else {
        element
    }
}

/// A help tooltip (`.help(…)` in the Swift app).
pub struct Tooltip(SharedString);

impl gpui::Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(320.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(5.))
            .bg(theme::color(theme::PANEL))
            .border_1()
            .border_color(theme::white(0.14))
            .text_size(theme::TEXT_SMALL)
            .text_color(theme::color(theme::TEXT))
            .font_family(theme::FONT_FAMILY)
            .child(self.0.clone())
    }
}

/// Builds a [`Tooltip`] for `.tooltip(…)`.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    let text = text.into();
    move |_, cx| gpui::AppContext::new(cx, |_| Tooltip(text.clone())).into()
}

/// The small activity indicator (native `ProgressView`). Under Reduce Motion it is the static
/// `fallback` symbol instead of a spinning one.
pub fn activity_indicator(
    id: impl Into<ElementId>,
    size: gpui::Pixels,
    reduce_motion: bool,
    fallback: Icon,
) -> gpui::AnyElement {
    use gpui::{Animation, AnimationExt as _, Transformation, percentage};
    if reduce_motion {
        return icon(fallback).size(size).text_color(theme::color(theme::SLATE)).into_any_element();
    }
    icon(Icon::Loader)
        .size(size)
        .text_color(theme::color(theme::SLATE))
        .with_animation(id, Animation::new(theme::SPINNER_PERIOD).repeat(), |svg, delta| {
            svg.with_transformation(Transformation::rotate(percentage(delta)))
        })
        .into_any_element()
}

/// `text` shortened in the middle with "…" to fit `max_width` at `font_size`/`weight` in the
/// system font (native `.truncationMode(.middle)`: paths keep both their start and their name).
/// Binary search over the number of kept grapheme clusters (never splitting one), half from
/// the start and half from the end, measured with the window's text system. Callers cache the
/// result per text and width.
pub fn truncate_middle(
    text: &str,
    max_width: gpui::Pixels,
    font_size: gpui::Pixels,
    weight: gpui::FontWeight,
    window: &Window,
) -> SharedString {
    let width = |candidate: &str| text_width(candidate, font_size, weight, window);
    if text.is_empty() || width(text) <= max_width {
        return SharedString::from(text.to_owned());
    }
    let clusters: Vec<&str> = text.graphemes(true).collect();
    let candidate = |keep: usize| {
        let head = keep.div_ceil(2);
        let tail = keep - head;
        let mut out: String = clusters[..head].concat();
        out.push('…');
        out.push_str(&clusters[clusters.len() - tail..].concat());
        out
    };
    // The largest number of kept clusters that still fits.
    let (mut low, mut high) = (0, clusters.len() - 1);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if width(&candidate(mid)) <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    candidate(low).into()
}

/// Width of `text` on one line in the system font at `font_size`/`weight`.
pub fn text_width(text: &str, font_size: gpui::Pixels, weight: gpui::FontWeight, window: &Window) -> gpui::Pixels {
    let mut font = window.text_style().font();
    font.family = theme::FONT_FAMILY.into();
    font.weight = weight;
    let run = gpui::TextRun {
        len: text.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().shape_line(SharedString::from(text.to_owned()), font_size, &[run], None).width
}

#[cfg(test)]
mod tests {
    use gpui::{Empty, TestAppContext, px};
    use unicode_segmentation::UnicodeSegmentation;

    use super::{text_width, truncate_middle};
    use crate::theme;

    /// Middle truncation fits the width, keeps a prefix and a suffix of the name around one
    /// ellipsis, never splits a grapheme cluster, and leaves names that fit alone.
    #[gpui::test]
    fn middle_truncation_fits_the_width_and_keeps_both_ends_whole(cx: &mut TestAppContext) {
        let family = "👨\u{200d}👩\u{200d}👧";
        let name = format!("Quarterly report {family} final FINAL draft e\u{301}te\u{301} (3).pdf");
        let (_, cx) = cx.add_window_view(|_, _| Empty);
        cx.update(|window, _| {
            let (size, weight) = (theme::TEXT_ROW, theme::WEIGHT_MEDIUM);
            let full = text_width(&name, size, weight, window);
            assert_eq!(truncate_middle(&name, full, size, weight, window).as_ref(), name);
            for max in [px(30.), px(60.), full * 0.4, full * 0.75, full - px(1.)] {
                let out = truncate_middle(&name, max, size, weight, window);
                assert!(text_width(&out, size, weight, window) <= max, "{out:?} wider than {max:?}");
                let (head, tail) = out.split_once('…').expect("one ellipsis");
                assert!(name.starts_with(head) && name.ends_with(tail), "{out:?}");
                let clusters: Vec<&str> = name.graphemes(true).collect();
                let whole = |part: &str, from_start: bool| {
                    let mut taken = String::new();
                    let mut iter: Box<dyn Iterator<Item = &&str>> =
                        if from_start { Box::new(clusters.iter()) } else { Box::new(clusters.iter().rev()) };
                    while taken.len() < part.len() {
                        let cluster = iter.next().expect("within the name");
                        taken = if from_start { taken + cluster } else { format!("{cluster}{taken}") };
                    }
                    taken == part
                };
                assert!(whole(head, true) && whole(tail, false), "{out:?} splits a cluster");
            }
        });
    }
}
