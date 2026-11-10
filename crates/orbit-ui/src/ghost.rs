//! What a collection drag carries on screen: the grabbed element itself.
//!
//! A drag does not float a separate card next to the pointer. The pressed element comes off the
//! UI and stays attached at the point where it was grabbed:
//! - **a Contents row** lifts out of the list at its own size: it rises (a growing shadow and a
//!   slight grow around the grab point) and takes on a raised fill, while the row it left dims;
//! - **a map segment** lifts out of the chart as its own arc, in its color. Large arcs shrink
//!   towards the grab point so they stay easy to carry, and a row-style label with its name and
//!   size fades in above it, centered over the pointer. The segment it left darkens.
//!
//! While the tray would accept the drop, the lifted row (or the segment's outline and label)
//! takes on the coral of the target with a plus badge. A drop sends it into the tray's glyph; a
//! release anywhere else springs it back into its place ([`crate::overlay`]).

use std::sync::Arc;

use gpui::{
    Bounds, Div, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, Point, Styled, canvas, div, point, prelude::*,
    px, size,
};
use orbit_sunburst::CollectionState;

use crate::icons::{Icon, icon};
use crate::motion::{ease_out, lerp, lerp_color};
use crate::overlay::TipContent;
use crate::theme;

/// Size of a lifted segment's label, and its gap above the pointer.
const LABEL_SIZE: (f32, f32) = (260., 46.);
const LABEL_GAP: f32 = 12.;
/// A lifted segment shrinks until its larger side is at most this long.
const SEGMENT_CARRY: f32 = 150.;
/// How far a lifted row grows on each side.
const ROW_GROW: f32 = 3.;

/// The grabbed element and the pointer position that grabbed it (window coordinates).
#[derive(Clone, Debug)]
pub struct Ghost {
    pub grab: Point<Pixels>,
    pub look: GhostLook,
    /// The row text (a row's own, or the segment's as in its tooltip).
    pub label: TipContent,
}

#[derive(Clone, Debug)]
pub enum GhostLook {
    /// The pressed row's painted bounds; `folder` rows end in the open-folder arrow.
    Row { bounds: Bounds<Pixels>, folder: bool },
    /// The pressed segment's outline (a closed polygon) and color.
    Segment { points: Arc<[Point<Pixels>]>, color: Hsla, bounds: Bounds<Pixels> },
}

/// How far along a lifted element is: `lift` (0 at the press, 1 lifted) and `over` (0..=1, the
/// drop-target look).
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub lift: f32,
    pub over: f32,
}

impl Ghost {
    /// How much a segment is scaled about the grab point once lifted.
    fn carry_scale(&self) -> f32 {
        match &self.look {
            GhostLook::Row { .. } => 1.0,
            GhostLook::Segment { bounds, .. } => {
                let extent = f32::from(bounds.size.width.max(bounds.size.height));
                if extent > SEGMENT_CARRY { SEGMENT_CARRY / extent } else { 1.0 }
            }
        }
    }

    /// The card the drop flight starts from with the pointer at `pointer`: the lifted row, or
    /// the segment's label.
    pub fn card_bounds(&self, pointer: Point<Pixels>) -> Bounds<Pixels> {
        match &self.look {
            GhostLook::Row { bounds, .. } => row_bounds(*bounds, pointer - self.grab, 1.0),
            GhostLook::Segment { .. } => self.label_bounds(pointer, 1.0),
        }
    }

    /// A lifted segment's label: centered above the pointer, and above the carried arc (paths
    /// paint above other elements, so the two must not overlap).
    fn label_bounds(&self, pointer: Point<Pixels>, lift: f32) -> Bounds<Pixels> {
        let (width, height) = LABEL_SIZE;
        let mut bottom = pointer.y - px(LABEL_GAP);
        if let GhostLook::Segment { bounds, .. } = &self.look {
            let scale = lerp(1.0, self.carry_scale(), ease_out(lift));
            let above = pointer.y + (bounds.top() - self.grab.y) * scale - px(10.);
            bottom = bottom.min(above);
        }
        Bounds::new(point(pointer.x - px(width / 2.), bottom - px(height)), size(px(width), px(height)))
    }

    /// The lifted element with the pointer at `pointer`, in window coordinates (place it in an
    /// absolutely positioned layer whose origin is the window's).
    pub fn element(&self, pointer: Point<Pixels>, pose: Pose) -> gpui::AnyElement {
        let delta = pointer - self.grab;
        let lift = pose.lift.clamp(0.0, 1.0);
        match &self.look {
            GhostLook::Row { bounds, folder } => {
                let bounds = row_bounds(*bounds, delta, lift);
                list_row(&self.label, *folder, pose)
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .into_any_element()
            }
            GhostLook::Segment { points, color, .. } => {
                let scale = lerp(1.0, self.carry_scale(), ease_out(lift));
                let grab = self.grab;
                let shape: Vec<Point<Pixels>> =
                    points.iter().map(|&p| pointer + point((p.x - grab.x) * scale, (p.y - grab.y) * scale)).collect();
                let (fill, over) = (*color, pose.over);
                // The white edge comes with the lift, so a segment set back down is the map's own.
                let edge = gpui::white().opacity(0.9 * ease_out(lift));
                let outline = lerp_color(edge, theme::color(theme::CORAL).into(), over);
                let label = self.label_bounds(pointer, lift);
                // The label rises out of the arc as it comes off the map.
                let label_lift = ease_out(((lift - 0.25) / 0.75).clamp(0.0, 1.0));
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, _| {
                                let mut body = PathBuilder::fill();
                                body.add_polygon(&shape, true);
                                if let Ok(path) = body.build() {
                                    window.paint_path(path, fill);
                                }
                                let mut edge = PathBuilder::stroke(px(1.5));
                                edge.add_polygon(&shape, true);
                                if let Ok(path) = edge.build() {
                                    window.paint_path(path, outline);
                                }
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .child(
                        lifted_row(&self.label, Pose { lift: 1.0, over })
                            .absolute()
                            .left(label.origin.x)
                            .top(label.origin.y + px(8. * (1. - label_lift)))
                            .w(label.size.width)
                            .h(label.size.height)
                            .opacity(label_lift),
                    )
                    .into_any_element()
            }
        }
    }
}

/// A row's bounds moved by `delta` and grown by `lift` around its center.
fn row_bounds(bounds: Bounds<Pixels>, delta: Point<Pixels>, lift: f32) -> Bounds<Pixels> {
    let grow = px(ROW_GROW * lift);
    Bounds::new(
        point(bounds.origin.x + delta.x - grow, bounds.origin.y + delta.y - grow),
        size(bounds.size.width + grow * 2., bounds.size.height + grow * 2.),
    )
}

/// A row card raised off the UI: the selected row's warm fill gives way to a raised neutral one
/// as it lifts, under a growing shadow; over the tray its border turns coral and a plus badge
/// appears.
pub fn lifted_row(content: &TipContent, pose: Pose) -> Div {
    let lift = ease_out(pose.lift.clamp(0.0, 1.0));
    let over = pose.over.clamp(0.0, 1.0);
    let fill = lerp_color(theme::color(theme::ROW_SELECTED).into(), theme::with_alpha(0x2a2d35, 0.98).into(), lift);
    let resting = lerp_color(theme::color(theme::CORAL).into(), theme::white(0.16).into(), lift);
    let border = lerp_color(resting, theme::color(theme::CORAL).into(), over);
    row_card(content, px(0.), px(0.))
        .rounded(px(8.))
        .bg(fill)
        .border_1()
        .border_color(border)
        .shadow(vec![gpui::BoxShadow {
            color: gpui::hsla(0., 0., 0., 0.5 * lift),
            offset: point(px(0.), px(2. + 10. * lift)),
            blur_radius: px(4. + 20. * lift),
            spread_radius: px(0.),
        }])
        .when(over > 0.0, |el| el.child(plus_badge(over)))
}

/// A Contents row raised off the list, laid out exactly like the list's row (padding, swatch,
/// columns and the trailing folder arrow or file mark), so that at `lift` 0 it is the pressed,
/// selected row itself: warm fill, thick coral outline. As it lifts the fill turns to a raised
/// neutral, the outline thins under a growing shadow; over the tray it turns coral with a plus
/// badge.
pub fn list_row(content: &TipContent, folder: bool, pose: Pose) -> Div {
    let lift = ease_out(pose.lift.clamp(0.0, 1.0));
    let over = pose.over.clamp(0.0, 1.0);
    let slate = theme::color(theme::SLATE);
    let fill = lerp_color(theme::color(theme::ROW_SELECTED).into(), theme::with_alpha(0x2a2d35, 0.98).into(), lift);
    let resting = lerp_color(theme::color(theme::CORAL).into(), theme::white(0.16).into(), lift);
    let border = lerp_color(resting, theme::color(theme::CORAL).into(), over);
    let mark = match content.mark {
        CollectionState::None => None,
        CollectionState::Included => Some((Icon::CircleCheckFill, "In Collection")),
        CollectionState::Partial => Some((Icon::CircleMinusFill, "Partly collected")),
    };
    let trailing = if folder {
        div()
            .flex_none()
            .w(px(theme::ROW_ARROW.0))
            .h(px(theme::ROW_ARROW.1))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .bg(theme::white(0.07))
            .child(icon(Icon::ChevronRight).size(px(17.)).text_color(theme::color(theme::TEXT)))
    } else {
        div()
            .flex_none()
            .w(px(theme::ROW_ARROW.0))
            .h(px(theme::ROW_ARROW.1))
            .flex()
            .items_center()
            .justify_center()
            .child(icon(Icon::File).size(px(12.)).text_color(slate))
    };
    div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(11.))
        .px(px(5.))
        .rounded(px(7.))
        .bg(fill)
        .text_color(theme::color(theme::TEXT))
        .shadow(vec![gpui::BoxShadow {
            color: gpui::hsla(0., 0., 0., 0.5 * lift),
            offset: point(px(0.), px(2. + 10. * lift)),
            blur_radius: px(4. + 20. * lift),
            spread_radius: px(0.),
        }])
        .child(div().flex_none().w(px(7.)).h(px(30.)).rounded(px(3.)).bg(content.swatch))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .h(px(15.))
                        .text_size(theme::TEXT_BODY)
                        .font_weight(theme::WEIGHT_SEMIBOLD)
                        .truncate()
                        .child(content.name.clone()),
                )
                .child(
                    div()
                        .h(px(13.))
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .text_size(theme::TEXT_CAPTION)
                        .when_some(mark, |el, (mark, label)| {
                            el.child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(3.))
                                    .font_weight(theme::WEIGHT_MEDIUM)
                                    .child(icon(mark).size(px(10.)))
                                    .child(label),
                            )
                        })
                        .when(!content.detail.is_empty(), |el| {
                            el.child(div().min_w_0().truncate().text_color(slate).child(content.detail.clone()))
                        }),
                ),
        )
        .child(
            div()
                .flex_none()
                .w(px(76.))
                .flex()
                .flex_col()
                .items_end()
                .gap(px(3.))
                .child(theme::monospaced_digits(
                    div()
                        .h(px(15.))
                        .text_size(theme::TEXT_BODY)
                        .font_weight(theme::WEIGHT_SEMIBOLD)
                        .whitespace_nowrap()
                        .child(content.size.clone()),
                ))
                .child(theme::monospaced_digits(
                    div()
                        .h(px(13.))
                        .text_size(theme::TEXT_CAPTION)
                        .text_color(slate)
                        .whitespace_nowrap()
                        .child(content.share.clone()),
                )),
        )
        .child(trailing)
        // The outline is an overlay, like the list's: its width never moves the contents.
        .child(div().absolute().inset_0().rounded(px(7.)).border(px(lerp(2.5, 1., lift))).border_color(border))
        .when(over > 0.0, |el| el.child(plus_badge(over)))
}

/// The plus badge on a card's top-right corner: this drop adds to the collection.
pub fn plus_badge(opacity: f32) -> Div {
    div()
        .absolute()
        .top(px(-7.))
        .right(px(-7.))
        .size(px(18.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::color(theme::CORAL))
        .opacity(opacity)
        .child(icon(Icon::Plus).size(px(11.)).text_color(theme::color(theme::INK)))
}

/// A Contents row as a free-standing card (`width` × `height`): branch swatch, name, the
/// secondary line with its collection mark, size and share. The map tooltip, a lifted row and a
/// lifted segment's label all use it; callers restyle the fill, border and shadow.
pub fn row_card(content: &TipContent, width: Pixels, height: Pixels) -> gpui::Div {
    let slate = theme::color(theme::SLATE);
    let mark = match content.mark {
        CollectionState::None => None,
        CollectionState::Included => Some((Icon::CircleCheckFill, "In Collection")),
        CollectionState::Partial => Some((Icon::CircleMinusFill, "Partly collected")),
    };
    div()
        .relative()
        .w(width)
        .h(height)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(11.))
        .pl(px(10.))
        .pr(px(12.))
        .rounded(px(9.))
        .bg(theme::with_alpha(theme::ROW_HOVER, 0.97))
        .border_1()
        .border_color(theme::white(0.12))
        .shadow(vec![gpui::BoxShadow {
            color: gpui::hsla(0., 0., 0., 0.4),
            offset: point(px(0.), px(6.)),
            blur_radius: px(18.),
            spread_radius: px(0.),
        }])
        .child(div().flex_none().w(px(7.)).h(px(30.)).rounded(px(3.)).bg(content.swatch))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .h(px(15.))
                        .text_size(theme::TEXT_BODY)
                        .font_weight(theme::WEIGHT_SEMIBOLD)
                        .text_color(theme::color(theme::TEXT))
                        .truncate()
                        .child(content.name.clone()),
                )
                .child(
                    div()
                        .h(px(13.))
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .text_size(theme::TEXT_CAPTION)
                        .text_color(theme::color(theme::TEXT))
                        .when_some(mark, |el, (mark, label)| {
                            el.child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(3.))
                                    .font_weight(theme::WEIGHT_MEDIUM)
                                    .child(icon(mark).size(px(10.)))
                                    .child(label),
                            )
                        })
                        .when(!content.detail.is_empty(), |el| {
                            el.child(div().min_w_0().truncate().text_color(slate).child(content.detail.clone()))
                        }),
                ),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(3.))
                .child(theme::monospaced_digits(
                    div()
                        .h(px(15.))
                        .text_size(theme::TEXT_BODY)
                        .font_weight(theme::WEIGHT_SEMIBOLD)
                        .text_color(theme::color(theme::TEXT))
                        .whitespace_nowrap()
                        .child(content.size.clone()),
                ))
                .child(theme::monospaced_digits(
                    div()
                        .h(px(13.))
                        .text_size(theme::TEXT_CAPTION)
                        .text_color(slate)
                        .whitespace_nowrap()
                        .child(content.share.clone()),
                )),
        )
}
