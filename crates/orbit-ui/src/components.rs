//! Small shared building blocks (buttons, dividers, labels). Panels may add their own; anything
//! two panels need belongs here.

use gpui::{
    App, ClickEvent, CursorStyle, Div, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::icons::{Icon, icon};
use crate::theme;

/// Visual weight of a [`button`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Bordered neutral button (native `.bordered`).
    Bordered,
    /// Filled coral button (native `.borderedProminent` with the accent tint).
    Prominent,
    /// Text-only.
    Plain,
}

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
        ButtonStyle::Prominent => {
            (theme::color(theme::CORAL), theme::with_alpha(theme::CORAL, 0.85), theme::color(theme::CANVAS))
        }
        ButtonStyle::Plain => (theme::white(0.0), theme::white(0.07), theme::color(theme::TEXT)),
    };
    let fg = if enabled { fg } else { theme::with_alpha(theme::SLATE, 0.6) };
    let bg = if enabled || style != ButtonStyle::Prominent { bg } else { theme::white(0.10) };
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
}

/// The drag preview (Swift `dragPreview`): a 240 × 56 card 12 points right of the pointer,
/// vertically centered on it. `offset` is the pointer position inside the dragged element when
/// the drag started (GPUI draws the view at `pointer − offset`).
pub struct DragPreview {
    payload: CollectionDrag,
    offset: gpui::Point<gpui::Pixels>,
}

impl DragPreview {
    pub const SIZE: (f32, f32) = (240., 56.);

    pub fn new(payload: &CollectionDrag, offset: gpui::Point<gpui::Pixels>) -> Self {
        Self { payload: payload.clone(), offset }
    }
}

impl gpui::Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let (width, height) = Self::SIZE;
        div()
            .pl(self.offset.x + px(12.))
            .pt((self.offset.y - px(height / 2.)).max(px(0.)))
            .font_family(theme::FONT_FAMILY)
            .child(
                div()
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
                    .border_color(theme::color(0x858585))
                    .child(icon(self.payload.icon).size(px(20.)).text_color(theme::color(theme::TEXT)))
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
                                    .child(self.payload.label.clone()),
                            )
                            .child(
                                div()
                                    .text_size(theme::TEXT_SMALL)
                                    .text_color(theme::color(0xc7c7c7))
                                    .truncate()
                                    .child(self.payload.detail.clone()),
                            ),
                    ),
            )
    }
}
