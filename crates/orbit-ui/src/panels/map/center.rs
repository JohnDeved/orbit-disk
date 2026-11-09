//! The center circle: name, size, detail and Up slots (Swift `SunburstView.centerLabel`).
//!
//! The slots keep their heights whatever the text, so hovering does not move the labels. Below a
//! 60 pt hole radius the text is compact and Up is a bare chevron; below 80 pt the name gets one
//! line instead of two. The Up slot is reserved (transparent) at the root.

use gpui::{
    AnyElement, IntoElement, ParentElement, Pixels, SharedString, Styled, TextRun, Window, div, prelude::*, px,
};

use crate::icons::{Icon, icon};
use crate::theme;

/// Below this hole radius: compact text, chevron-only Up, one detail line.
pub const COMPACT_RADIUS: f32 = 60.0;
/// Below this hole radius the name gets one line instead of two.
pub const ONE_LINE_RADIUS: f32 = 80.0;
const MAX_VALUE_SIZE: f32 = 38.0;

/// What the center shows: the active segment (hover, row hover, pinned group, selection) or the
/// folder itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CenterText {
    pub name: SharedString,
    pub value: SharedString,
    pub detail: SharedString,
    pub can_go_up: bool,
}

/// Width and line count of the name slot for a hole of `radius` points.
pub fn name_slot(radius: f32) -> (Pixels, usize) {
    (px((radius * 1.2).max(1.)), if radius < ONE_LINE_RADIUS { 1 } else { 2 })
}

/// Width of the label stack for a hole of `radius` points.
pub fn stack_width(radius: f32) -> Pixels {
    px((radius * 1.65).max(1.))
}

/// The value's font size: 0.37 × the hole radius (at most 38 points), shrunk down to 60% so a
/// long value ("661.69 GB") fits the stack on one line (native `.minimumScaleFactor(0.6)`).
pub fn value_font_size(value: &str, radius: f32, window: &Window) -> f32 {
    let size = (radius * 0.37).clamp(1.0, MAX_VALUE_SIZE);
    let mut font = window.text_style().font();
    font.family = theme::FONT_ROUNDED.into();
    font.weight = theme::WEIGHT_LIGHT;
    font.features = theme::tabular_digits();
    let run = TextRun {
        len: value.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let width =
        f32::from(window.text_system().shape_line(SharedString::from(value.to_owned()), px(size), &[run], None).width);
    let available = f32::from(stack_width(radius));
    if width <= available || width <= 0. { size } else { size * (available / width).max(0.6) }
}

/// The label stack for a hole of `radius` points. The name arrives middle-truncated for the
/// slot ([`name_slot`]); the clamp only guards against unexpected wrapping. The value is set at
/// `value_size` points ([`value_font_size`]) in a line box of the unscaled size, so a shrunk
/// value never moves the other slots.
pub fn element(text: &CenterText, radius: f32, value_size: f32) -> AnyElement {
    let compact = radius < COMPACT_RADIUS;
    let (name_width, title_lines) = name_slot(radius);
    let detail_lines = if compact { 1 } else { 2 };
    let line_size = (radius * 0.37).clamp(1.0, MAX_VALUE_SIZE);
    let spacing = if compact {
        3.0
    } else if radius < ONE_LINE_RADIUS {
        4.0
    } else {
        7.0
    };
    let clamp =
        |el: gpui::Div, lines: usize| if lines == 1 { el.truncate() } else { el.line_clamp(lines).text_ellipsis() };
    let up = div()
        .h(px(14.))
        .when(radius >= ONE_LINE_RADIUS, |el| el.mt(px(7.)))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .text_size(theme::TEXT_SMALL)
        .font_weight(theme::WEIGHT_MEDIUM)
        .text_color(theme::color(theme::TEXT))
        .when(!text.can_go_up, |el| el.opacity(0.))
        .child(icon(Icon::ChevronUp).size(px(11.)).text_color(theme::color(theme::TEXT)))
        .when(!compact, |el| el.child("Up one folder"));
    div()
        .w(stack_width(radius))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(spacing))
        .child(
            clamp(
                div()
                    .w(name_width)
                    .h(px(16. * title_lines as f32))
                    .text_size(theme::TEXT_ROW)
                    .line_height(px(16.))
                    .font_weight(theme::WEIGHT_MEDIUM)
                    .text_color(theme::color(theme::CENTER_NAME))
                    .text_center(),
                title_lines,
            )
            .child(text.name.clone()),
        )
        .child(theme::monospaced_digits(
            div()
                .max_w_full()
                .h(px((line_size * 1.25).ceil()))
                .text_size(px(value_size))
                .line_height(px((line_size * 1.25).ceil()))
                .font_family(theme::FONT_ROUNDED)
                .font_weight(theme::WEIGHT_LIGHT)
                .text_color(theme::color(theme::TEXT))
                .truncate()
                .child(text.value.clone()),
        ))
        .child(
            clamp(
                div()
                    .w_full()
                    .h(px(14. * detail_lines as f32))
                    .text_size(theme::TEXT_SMALL)
                    .line_height(px(14.))
                    .text_color(theme::color(theme::CENTER_DETAIL))
                    .text_center(),
                detail_lines,
            )
            .child(text.detail.clone()),
        )
        .child(up)
        .into_any_element()
}
