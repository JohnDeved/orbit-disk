//! Welcome and first-scan screens (Swift `welcomeView` / `scanningView`), layered over the explorer
//! area (breadcrumbs, map, contents) by the main window.
//!
//! - No map and nothing mapping: the single scan instruction and "Choose Folder".
//! - No map yet while mapping or opening: the full-screen progress (title, items found, the path
//!   being read). It gives way to the explorer as soon as the first preview is installed.
//! - With a map the layer is transparent and only acts as the explorer's Finder drop target
//!   (folders → `AddPlaces`, like the sidebar); its hitbox does not block the map or the list.
//!
//! Observes [`Regions::HEADER`] (location, scanning and opening state) and, while visible,
//! [`Regions::FOOTER`] (the item count ticks with progress). With a map neither re-renders it, so
//! progress ticks and typing stay out of this view. Renders count as `header` in frame stats.

use gpui::{
    Context, DragMoveEvent, ExternalPaths, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, px,
};
use orbit_app_state::{AppState, Regions};

use crate::actions::OpenFolder;
use crate::components::{ButtonStyle, activity_indicator, button, truncate_middle};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::panels::sidebar::{FolderDropHover, add_dropped_folders, track_folder_drag};
use crate::theme;

pub struct WelcomePanel {
    host: Host,
    /// The screen was visible at the last render.
    shown: bool,
    _subscriptions: Vec<Subscription>,
}

/// The layer covers the explorer (no folder is installed yet).
fn visible(state: &AppState) -> bool {
    state.folder().is_none()
}

impl WelcomePanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        let mut watch = |region| {
            cx.observe(host.signal(region), |this: &mut Self, _, cx| {
                if this.shown || visible(this.host.state(cx)) {
                    cx.notify();
                }
            })
        };
        let subscriptions = vec![watch(Regions::HEADER), watch(Regions::FOOTER)];
        Self { host: host.clone(), shown: false, _subscriptions: subscriptions }
    }
}

impl Render for WelcomePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(Regions::HEADER);
        let reduce_motion = theme::reduce_motion(cx);
        let state = self.host.state(cx);
        self.shown = visible(state);
        let layer = div()
            .id("explorer-drop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .on_drag_move(|event: &DragMoveEvent<ExternalPaths>, _, cx| {
                track_folder_drag(event, |hover, inside| hover.content = inside, cx)
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                FolderDropHover::update(|hover| *hover = FolderDropHover::default(), cx);
                add_dropped_folders(&this.host, paths.paths(), cx);
            }));
        if !self.shown {
            return layer;
        }
        let busy = state.is_scanning() || state.is_opening();
        let content = if busy {
            let path = truncate_middle(
                &theme::display(state.scan_path()),
                px(520.),
                theme::TEXT_SMALL,
                gpui::FontWeight::NORMAL,
                window,
            );
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(20.))
                .child(activity_indicator("scanning-activity", px(28.), reduce_motion, Icon::Loader))
                .child(div().text_size(px(26.)).font_weight(theme::WEIGHT_MEDIUM).child(state.title()))
                .child(metric("Items found", theme::format_count(state.progress_count())))
                .child(
                    div()
                        .max_w(px(520.))
                        .h(px(14.))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_size(theme::TEXT_SMALL)
                        .text_color(theme::color(theme::SLATE))
                        .child(path),
                )
        } else {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(22.))
                .child(icon(Icon::ChartPie).size(px(74.)).text_color(theme::color(theme::CORAL)))
                .child(
                    div()
                        .text_size(theme::TEXT_TRAY)
                        .text_color(theme::color(theme::SLATE))
                        .child("Choose a disk or folder to scan."),
                )
                .child(div().pt(px(6.)).child(button(
                    "choose-folder",
                    "Choose Folder",
                    None,
                    ButtonStyle::Prominent,
                    true,
                    |_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx),
                )))
        };
        layer.occlude().bg(theme::color(theme::CANVAS)).flex().items_center().justify_center().child(content)
    }
}

/// A caption over a large monospaced value, 130 points wide (the explorer metrics).
fn metric(title: &'static str, value: String) -> impl IntoElement {
    div()
        .w(theme::MAP_METRIC_WIDTH)
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.))
        .child(div().h(px(14.)).text_size(theme::TEXT_SMALL).text_color(theme::color(theme::SLATE)).child(title))
        .child(theme::monospaced_digits(
            div().h(px(22.)).text_size(theme::TEXT_METRIC).font_weight(theme::WEIGHT_MEDIUM).child(value),
        ))
}
