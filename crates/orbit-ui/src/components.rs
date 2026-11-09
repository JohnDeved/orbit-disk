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
