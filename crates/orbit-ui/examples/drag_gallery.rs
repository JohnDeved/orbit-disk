//! The map tooltip, grabbed rows and segments lifting off the UI, and frames of the drop flights,
//! rendered by the app's own overlay code at real size on the explorer canvas, for design review.
//!
//! `cargo run -p orbit-ui --example drag_gallery`

use std::sync::Arc;

use gpui::{
    App, Application, Bounds, Context, Pixels, Point, Render, Window, WindowBounds, WindowOptions, div, point,
    prelude::*, px, size,
};
use orbit_sunburst::CollectionState;
use orbit_ui::Assets;
use orbit_ui::components::CollectionDrag;
use orbit_ui::ghost::{Ghost, GhostLook, Pose};
use orbit_ui::icons::{Icon, icon};
use orbit_ui::overlay::{FlightKind, TipContent, flight_frame, square, tip_element};
use orbit_ui::theme;

const WIDTH: f32 = 1100.;

fn content(
    ordinal: u32,
    depth: u8,
    name: &str,
    mark: CollectionState,
    detail: &str,
    size: &str,
    share: &str,
) -> TipContent {
    TipContent {
        swatch: theme::chart(ordinal, depth),
        name: name.to_owned().into(),
        mark,
        detail: detail.to_owned().into(),
        size: size.to_owned().into(),
        share: share.to_owned().into(),
    }
}

fn tips() -> Vec<(&'static str, TipContent)> {
    vec![
        (
            "A folder in the first ring",
            content(0, 0, "tears of the kingdom save", CollectionState::None, "479 files", "31.2 MB", "14.5%"),
        ),
        (
            "A file in a deeper ring",
            content(
                3,
                2,
                "slot_02.sav",
                CollectionState::None,
                "SAV · in tears of the kingdom save › saves",
                "2.4 MB",
                "1.1%",
            ),
        ),
        ("Already collected", content(5, 0, "PopSetup.exe", CollectionState::Included, "EXE", "28.3 MB", "13.1%")),
    ]
}

/// A row ghost grabbed at `grab` from a 316 × 40 row at `origin`.
fn row_ghost(origin: Point<Pixels>, grab: Point<Pixels>) -> Ghost {
    Ghost {
        grab,
        look: GhostLook::Row { bounds: Bounds::new(origin, size(px(316.), px(40.))), folder: true },
        label: content(0, 0, "tears of the kingdom save", CollectionState::None, "479 files", "31.2 MB", "14.5%"),
    }
}

/// A segment ghost: part of a ring around `center`, grabbed at its middle.
fn segment_ghost(center: Point<Pixels>) -> Ghost {
    let (inner, outer) = (120f32, 150f32);
    let (start, end) = (-1.9f32, -0.5f32);
    let at = |r: f32, a: f32| point(center.x + px(r * a.cos()), center.y + px(r * a.sin()));
    let steps = 40;
    let angle = |i: usize| start + (end - start) * i as f32 / steps as f32;
    let mut points: Vec<Point<Pixels>> = (0..=steps).map(|i| at(outer, angle(i))).collect();
    points.extend((0..=steps).rev().map(|i| at(inner, angle(i))));
    let (mut min, mut max) = (points[0], points[0]);
    for p in &points {
        (min.x, min.y, max.x, max.y) = (min.x.min(p.x), min.y.min(p.y), max.x.max(p.x), max.y.max(p.y));
    }
    Ghost {
        grab: at((inner + outer) / 2., (start + end) / 2.),
        look: GhostLook::Segment {
            points: Arc::from(points),
            color: theme::chart(1, 0),
            bounds: Bounds::new(min, size(max.x - min.x, max.y - min.y)),
        },
        label: content(1, 0, "Applications", CollectionState::None, "23,988 files", "126.41 GB", "19.1%"),
    }
}

fn card(ghost: Ghost) -> CollectionDrag {
    CollectionDrag {
        drag: orbit_app_state::DragId(1),
        label: "tears of the kingdom save".into(),
        detail: "31.2 MB".into(),
        icon: Icon::FolderFill,
        ghost: Some(ghost),
    }
}

fn caption(text: &'static str) -> gpui::Div {
    div().text_size(theme::TEXT_CAPTION).text_color(theme::color(theme::NEUTRAL)).child(text)
}

/// A strip of `height` whose children are absolutely placed inside it.
fn strip(height: f32, children: Vec<gpui::AnyElement>) -> gpui::Div {
    div().relative().h(px(height)).w_full().children(children)
}

struct Gallery;

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let tips =
            tips().into_iter().map(|(label, content)| {
                div().flex().flex_col().gap(px(6.)).child(caption(label)).child(
                    div().relative().h(px(52.)).w(px(300.)).child(tip_element(&content, point(px(0.), px(0.)), 1.0)),
                )
            });
        // Rows grabbed 60 points into the row, lifting as they are carried; the last over the tray.
        let rows: Vec<_> = [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (1.0, 1.0)]
            .into_iter()
            .enumerate()
            .map(|(i, (lift, over))| {
                let origin = point(px(10. + 340. * (i % 3) as f32), px(10. + 60. * (i / 3) as f32));
                let grab = origin + point(px(60.), px(20.));
                row_ghost(origin, grab).element(grab + point(px(0.), px(4. * lift)), Pose { lift, over })
            })
            .collect();
        // Segments lifting off (scale 1 → carry size) as the pointer moves away.
        let segments: Vec<_> = [(0.0, 0.0), (0.5, 30.0), (1.0, 70.0)]
            .into_iter()
            .enumerate()
            .map(|(i, (lift, carry))| {
                let ghost = segment_ghost(point(px(180. + 340. * i as f32), px(530.)));
                let pointer = ghost.grab + point(px(carry), px(carry * 0.6));
                ghost.element(pointer, Pose { lift, over: 0.0 })
            })
            .collect();
        // Drop: a lifted row released over the tray flies into the glyph.
        let glyph = point(px(WIDTH - 140.), px(60.));
        let ghost = row_ghost(point(px(20.), px(20.)), point(px(80.), px(40.)));
        let release = point(px(160.), px(40.));
        let from = ghost.card_bounds(release);
        let collect: Vec<_> = [0.0, 0.3, 0.55, 0.75, 0.9]
            .map(|t| {
                flight_frame(&card(ghost.clone()), from, square(glyph, 30.), release, FlightKind::Collect, t)
                    .into_any_element()
            })
            .into();
        // A missed drop: the segment slides back into its place.
        let back = segment_ghost(point(px(220.), px(960.)));
        let away = back.grab + point(px(520.), px(-20.));
        let none = Bounds::default();
        let returns: Vec<_> = [0.0, 0.12, 0.25, 0.5, 1.0]
            .map(|t| flight_frame(&card(back.clone()), none, none, away, FlightKind::Return, t).into_any_element())
            .into();
        div()
            .relative()
            .size_full()
            .bg(theme::color(theme::CANVAS))
            .font_family(theme::FONT_FAMILY)
            .text_color(theme::color(theme::TEXT))
            .p(px(28.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(div().flex().flex_row().flex_wrap().gap(px(24.)).children(tips))
            .child(caption("A grabbed row lifting: 0, ½, 1; below, over the tray"))
            .child(strip(130., rows))
            .child(caption("A grabbed segment lifting off, shrinking to carry size, its label appearing"))
            .child(div().h(px(250.)))
            .child(caption("Drop: the lifted row is tossed into the tray glyph (t = 0, .3, .55, .75, .9)"))
            .child(
                div()
                    .relative()
                    .h(px(120.))
                    .w_full()
                    .rounded(theme::RADIUS_CARD)
                    .bg(theme::color(theme::SIDEBAR))
                    .child(
                        div()
                            .absolute()
                            .left(glyph.x - px(17.))
                            .top(glyph.y - px(17.))
                            .child(icon(Icon::TrayDown).size(px(34.)).text_color(theme::color(theme::CORAL))),
                    )
                    .children(collect),
            )
            .child(caption("Missed drop: the segment springs back and sets down in its place (t = 0, .12, .25, .5, 1)"))
            .child(div().h(px(250.)))
            // Ghosts are placed in window coordinates (as in the drag view): a layer at the origin.
            .child(div().absolute().top_0().left_0().size_full().children(segments).children(returns))
    }
}

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(WIDTH), px(1080.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), focus: true, ..Default::default() },
            |_, cx| cx.new(|_| Gallery),
        )
        .expect("gallery window");
        cx.activate(true);
    });
}
