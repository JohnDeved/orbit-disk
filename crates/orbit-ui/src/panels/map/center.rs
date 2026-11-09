//! The center circle: name, size, detail and Up slots (Swift `SunburstView.centerLabel`).
//!
//! The slots keep their heights whatever the text, so hovering does not move the labels. Below a
//! 60 pt hole radius the text is compact and Up is a bare chevron; below 80 pt the name gets one
//! line instead of two. The Up slot is reserved (transparent) at the root.

use gpui::{AnyElement, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};

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

/// The label stack for a hole of `radius` points.
pub fn element(text: &CenterText, radius: f32) -> AnyElement {
    let compact = radius < COMPACT_RADIUS;
    let title_lines = if radius < ONE_LINE_RADIUS { 1 } else { 2 };
    let detail_lines = if compact { 1 } else { 2 };
    let value_size = (radius * 0.37).clamp(1.0, MAX_VALUE_SIZE);
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
        .w(px((radius * 1.65).max(1.)))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(spacing))
        .child(
            clamp(
                div()
                    .w(px((radius * 1.2).max(1.)))
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
                .h(px((value_size * 1.25).ceil()))
                .text_size(px(value_size))
                .line_height(px((value_size * 1.25).ceil()))
                .font_weight(FontWeight::LIGHT)
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
