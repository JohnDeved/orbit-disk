//! PLACEHOLDER — toolbar and breadcrumbs (owner: toolbar/sidebar/footer panel agent).
//!
//! [`ToolbarPanel`]: the 52-point unified title bar row right of the sidebar: location title and
//! path (280 points, inset 142 after the traffic lights), search field (210 × 34), Refresh and
//! Open Folder…. Status/notice text. Observes [`Regions::HEADER`].
//!
//! [`BreadcrumbBar`]: the explorer header (48 points): Up, breadcrumbs, opening indicator and the
//! file count. Observes [`Regions::HEADER`].
//!
//! Still to do: the search text input (key context `TextInput` so Space reaches it; Escape
//! clears; `QueryChanged`), the coral focus ring, "Search mapped files" during partial maps,
//! horizontal breadcrumb scrolling, the opening spinner, help texts.

use gpui::{
    Context, CursorStyle, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use orbit_app_state::{Event, Regions};
use orbit_core::paths::file_name;

use crate::actions::OpenFolder;
use crate::components::{ButtonStyle, button};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::theme;

pub const REGIONS: Regions = Regions::HEADER;

pub struct ToolbarPanel {
    host: Host,
    search_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ToolbarPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self { host: host.clone(), search_focus: cx.focus_handle(), _subscriptions: host.observe(REGIONS, cx) }
    }

    /// ⌘F.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.search_focus);
        cx.notify();
    }
}

impl Focusable for ToolbarPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.search_focus.clone()
    }
}

impl Render for ToolbarPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let host = self.host.read(cx);
        let state = host.state();
        let current = state.current_path().map(<[u8]>::to_vec);
        let has_root = state.root_path().is_some();
        let name = current.as_deref().map(|p| theme::display(if file_name(p).is_empty() { p } else { file_name(p) }));
        let path = current.as_deref().map(theme::display);
        let status =
            host.notice().map(|s| s.to_string()).or_else(|| state.error().or(state.status()).map(str::to_owned));
        let can_refresh = has_root && !state.is_scanning() && !state.is_refreshing() && !state.is_removing();
        let placeholder = if state.is_partial() { "Search mapped files" } else { "Search indexed files" };
        let query = state.query().to_owned();
        let focused = self.search_focus.is_focused(window);
        let refresh = self.host.clone();
        let title_inset = theme::TRAFFIC_LIGHT_AREA + theme::TOOLBAR_TITLE_INSET - f32::from(theme::SIDEBAR_WIDTH);
        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .pl(px(title_inset))
            .pr(theme::SPACE_M)
            .gap(theme::TOOLBAR_SPACING)
            .child(
                div()
                    .w(theme::TOOLBAR_TITLE_WIDTH)
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(theme::TEXT_TITLE)
                            .font_weight(theme::WEIGHT_SEMIBOLD)
                            .truncate()
                            .children(name),
                    )
                    .child(
                        div()
                            .text_size(theme::TEXT_SMALL)
                            .text_color(theme::color(theme::SLATE))
                            .truncate()
                            .children(status.or(path)),
                    ),
            )
            .child(div().flex_1())
            .when(has_root, |el| {
                el.child(
                    div()
                        .id("search")
                        .track_focus(&self.search_focus)
                        .key_context("TextInput")
                        .w(theme::TOOLBAR_SEARCH_WIDTH)
                        .h(theme::TOOLBAR_SEARCH_HEIGHT)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(7.))
                        .px(px(9.))
                        .rounded(theme::RADIUS_INPUT)
                        .bg(theme::color(theme::PANEL))
                        .border_1()
                        .border_color(if focused {
                            theme::color(theme::CORAL)
                        } else {
                            theme::with_alpha(theme::SLATE, 0.6)
                        })
                        .text_size(theme::TEXT_ROW)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(icon(Icon::Search).text_color(theme::color(theme::SLATE)))
                        .child(if query.is_empty() {
                            div().text_color(theme::color(theme::SLATE)).child(placeholder)
                        } else {
                            div().child(query)
                        }),
                )
                .child(button(
                    "refresh",
                    "Refresh",
                    Some(Icon::Refresh),
                    ButtonStyle::Bordered,
                    can_refresh,
                    move |_, _, cx| refresh.send(Event::Refresh, cx),
                ))
            })
            .child(button(
                "open-folder",
                "Open Folder…",
                Some(Icon::FolderPlus),
                ButtonStyle::Bordered,
                true,
                |_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx),
            ))
    }
}

pub struct BreadcrumbBar {
    host: Host,
    _subscriptions: Vec<Subscription>,
}

impl BreadcrumbBar {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self { host: host.clone(), _subscriptions: host.observe(REGIONS, cx) }
    }
}

impl Render for BreadcrumbBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let state = self.host.state(cx);
        let can_go_up = state.can_go_up();
        let (crumbs, files) = match (state.root_path(), state.folder()) {
            (Some(root), Some(folder)) => {
                let current = folder.folder_path();
                let mut crumbs = vec![root.to_vec()];
                if current.len() > root.len() {
                    let rest = &current[root.len()..];
                    let mut prefix = root.to_vec();
                    for part in rest.split(|b| *b == b'/').filter(|p| !p.is_empty()) {
                        if prefix.last() != Some(&b'/') {
                            prefix.push(b'/');
                        }
                        prefix.extend_from_slice(part);
                        crumbs.push(prefix.clone());
                    }
                }
                let meta = folder.folder_meta();
                let complete = meta.state == orbit_core::types::ScanState::Complete;
                let files = format!(
                    "{} {}{}",
                    theme::format_count(meta.file_count),
                    if meta.file_count == 1 { "file" } else { "files" },
                    if complete { "" } else { " found" }
                );
                (crumbs, files)
            }
            _ => (Vec::new(), String::new()),
        };
        let last = crumbs.len().saturating_sub(1);
        let up = self.host.clone();
        let mut trail =
            div().flex_1().min_w_0().flex().flex_row().items_center().gap(px(7.)).text_size(theme::TEXT_BODY);
        for (i, crumb) in crumbs.into_iter().enumerate() {
            if i > 0 {
                trail = trail.child(icon(Icon::ChevronRight).size(px(9.)).text_color(theme::color(theme::SLATE)));
            }
            let name: SharedString =
                theme::display(if file_name(&crumb).is_empty() { &crumb } else { file_name(&crumb) }).into();
            let host = self.host.clone();
            let path: std::sync::Arc<[u8]> = crumb.into();
            trail = trail.child(
                div()
                    .id(("crumb", i))
                    .flex_none()
                    .cursor(CursorStyle::PointingHand)
                    .text_color(if i == last { theme::color(theme::TEXT) } else { theme::color(theme::SLATE) })
                    .child(name)
                    .on_click(move |_, _, cx| host.send(Event::Open { path: path.clone() }, cx)),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.))
            .px(px(28.))
            .child(button("go-up", "Up", Some(Icon::ArrowUp), ButtonStyle::Bordered, can_go_up, move |_, _, cx| {
                up.send(Event::GoUp, cx)
            }))
            .child(trail)
            .child(theme::monospaced_digits(
                div()
                    .w(px(138.))
                    .flex()
                    .justify_end()
                    .text_size(theme::TEXT_BODY)
                    .text_color(theme::color(theme::SLATE))
                    .child(files),
            ))
    }
}

use gpui::prelude::*;
