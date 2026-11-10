//! Every state of the mapping footer, rendered by the app's own footer code
//! ([`FooterView`] → [`footer_content`]) at its real width and height, for design review.
//!
//! `cargo run -p orbit-ui --example footer_gallery`
//!
//! Each row is the 51-point footer frame on the explorer canvas, captioned with the state it
//! shows. Values are fixed (no backend), so the window is deterministic for screenshots.

use gpui::{App, Application, Bounds, Context, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px, size};
use orbit_app_state::{FooterAction, FooterState, Phase};
use orbit_ui::Assets;
use orbit_ui::panels::footer::{FooterView, Shown, footer_content, footer_frame};
use orbit_ui::theme;

/// The footer's width in a 1280-point window (explorer column minus nothing: it spans the
/// explorer below the map and inspector).
const WIDTH: f32 = 1070.;

struct Case {
    caption: &'static str,
    footer: FooterState,
    age: Option<&'static str>,
    status: Option<&'static str>,
    skipped: u64,
}

fn base(root: &str) -> FooterState {
    FooterState {
        root_name: root.into(),
        discovered_files: 0,
        cached_files: None,
        discovery_fraction: None,
        discovery_detail: "From this scan",
        is_paused: false,
        discovery_complete: false,
        rescan: false,
        phase: Phase::Finding,
        action: FooterAction::Stop,
    }
}

fn cases() -> Vec<Case> {
    let mac = || base("Macintosh HD");
    vec![
        Case {
            caption: "First map · no estimate yet (no track, no invented percentage)",
            footer: FooterState { discovered_files: 48_213, cached_files: Some(31_020), ..base("Home") },
            age: None,
            status: None,
            skipped: 0,
        },
        Case {
            caption: "First map · estimate from this scan",
            footer: FooterState {
                discovered_files: 1_204_559,
                cached_files: Some(902_118),
                discovery_fraction: Some(0.37),
                ..base("Home")
            },
            age: None,
            status: None,
            skipped: 0,
        },
        Case {
            caption: "Rescan of the whole Mac · the reported screenshot's numbers (more files than the previous map)",
            footer: FooterState {
                discovered_files: 6_702_410,
                cached_files: Some(4_633_420),
                discovery_fraction: Some(1.02),
                discovery_detail: "More files than expected",
                rescan: true,
                ..mac()
            },
            age: Some("11 hr, 26 min"),
            status: None,
            skipped: 780,
        },
        Case {
            caption: "Every file found · indexing the rest",
            footer: FooterState {
                discovered_files: 6_702_410,
                cached_files: Some(6_120_334),
                discovery_fraction: Some(1.0),
                discovery_complete: true,
                phase: Phase::Saving,
                action: FooterAction::None,
                ..mac()
            },
            age: None,
            status: None,
            skipped: 780,
        },
        Case {
            caption: "Background update of changed folders",
            footer: FooterState {
                discovered_files: 4_946_253,
                discovery_complete: true,
                phase: Phase::Updating,
                action: FooterAction::Pause,
                ..base("Home")
            },
            age: Some("38 min"),
            status: None,
            skipped: 437,
        },
        Case {
            caption: "Mapping stopped by the user",
            footer: FooterState {
                discovered_files: 2_741_998,
                cached_files: Some(2_532_700),
                discovery_fraction: Some(0.41),
                discovery_detail: "From previous map",
                is_paused: true,
                phase: Phase::Paused,
                action: FooterAction::Resume,
                ..mac()
            },
            age: None,
            status: None,
            skipped: 0,
        },
        Case {
            caption: "Automatic updates paused after a failed update (no alert; reason inline, Refresh beside it)",
            footer: FooterState {
                discovered_files: 4_946_253,
                discovery_complete: true,
                phase: Phase::Paused,
                action: FooterAction::Refresh,
                ..base("Home")
            },
            age: Some("38 min"),
            status: Some(
                "Map kept. Update failed: /Users/johann/Library/CloudStorage/O+Connect-DeviceSpace/.Trash: Resource deadlock avoided (os error 11)",
            ),
            skipped: 437,
        },
        Case {
            caption: "Mapping interrupted by an error",
            footer: FooterState {
                discovered_files: 912_004,
                cached_files: Some(880_115),
                discovery_fraction: Some(0.62),
                is_paused: true,
                phase: Phase::Interrupted,
                action: FooterAction::Resume,
                ..base("Projects")
            },
            age: None,
            status: None,
            skipped: 0,
        },
        Case {
            caption: "Up to date · at rest",
            footer: FooterState {
                discovered_files: 6_702_410,
                cached_files: Some(6_702_410),
                discovery_complete: true,
                phase: Phase::Saved,
                action: FooterAction::None,
                ..mac()
            },
            age: Some("5 min"),
            status: None,
            skipped: 780,
        },
        Case {
            caption: "Up to date · with a transient message",
            footer: FooterState {
                discovered_files: 6_702_410,
                cached_files: Some(6_702_410),
                discovery_complete: true,
                phase: Phase::Saved,
                action: FooterAction::None,
                ..mac()
            },
            age: Some("less than a minute"),
            status: Some("Moved 3 items to the Trash."),
            skipped: 0,
        },
    ]
}

struct Gallery;

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let rows = cases().into_iter().map(|case| {
            let view = FooterView::new(&case.footer, &case.footer.root_name, case.age, case.status, case.skipped);
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .px(px(28.))
                        .text_size(theme::TEXT_CAPTION)
                        .text_color(theme::color(theme::NEUTRAL))
                        .child(case.caption),
                )
                .child(
                    div()
                        .w(px(WIDTH))
                        .h(theme::FOOTER_HEIGHT)
                        .bg(theme::color(theme::CANVAS))
                        .border_t_1()
                        .border_b_1()
                        .border_color(theme::white(theme::DIVIDER_ALPHA))
                        .child(footer_frame(Some(footer_content(&view, Shown::settled(&view), false, None)))),
                )
        });
        div()
            .size_full()
            .bg(theme::color(theme::SIDEBAR))
            .font_family(theme::FONT_FAMILY)
            .py(px(20.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .children(rows)
    }
}

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(WIDTH), px(930.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), focus: true, ..Default::default() },
            |_, cx| cx.new(|_| Gallery),
        )
        .expect("gallery window");
        cx.activate(true);
    });
}
