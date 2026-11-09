//! The main window: transparent unified title bar, the fixed layout skeleton, window actions.
//!
//! ```text
//! ┌ sidebar 210 ┬ toolbar (52, HEADER) ───────────────────────────────────────────────┐
//! │ (title bar) ├ breadcrumbs (48, HEADER) ────────────────────────────────────────────┤
//! │ SIDEBAR     │ map (MAP, flexible)                 │ contents (LIST, 340 wide)      │
//! │             │                                     ├ inspector (INSPECTOR, 164)     │
//! │             ├ footer (FOOTER, 51) ─────────────────────────────────────────────────┤
//! │             ├ tray (TRAY, 12 gap + 80 + 12) ─────────────────────────────────────────┤
//! └─────────────┴──────────────────────────────────────────────────────────────────────┘
//!   sheets (SHEETS) overlay the whole window.
//! ```
//!
//! Every panel is a cached view: the root re-renders cheaply each frame something below it
//! changed, and only notified panels run their `render`.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyView, App, AppContext, Bounds, Context, FocusHandle, InteractiveElement, IntoElement, MouseButton,
    ParentElement, PathPromptOptions, Render, StyleRefinement, Styled, Subscription, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions, div, point, prelude::*, px, size,
};
use orbit_app_state::{Event, Regions};

use crate::actions::*;
use crate::frame_stats::{self, timed};
use crate::harness::{FrameHarness, HarnessScript};
use crate::host::{Host, path_bytes};
use crate::lifecycle;
use crate::panels::Panels;
use crate::panels::welcome::WelcomePanel;
use crate::theme;

pub struct MainWindow {
    host: Host,
    focus: FocusHandle,
    panels: Panels,
    /// Welcome / first-scan screen and the explorer's Finder drop target (over the explorer).
    welcome: gpui::Entity<WelcomePanel>,
    harness: Option<Rc<RefCell<FrameHarness>>>,
    _subscriptions: Vec<Subscription>,
}

impl MainWindow {
    pub fn new(host: Host, harness: Option<HarnessScript>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        // Action availability follows the header (navigation) and inspector (selection) state.
        let subscriptions = host.observe(Regions::HEADER | Regions::INSPECTOR, cx);
        let panels = Panels::new(&host, cx);
        let welcome = cx.new(|cx| WelcomePanel::new(&host, cx));
        let harness =
            harness.map(|script| Rc::new(RefCell::new(FrameHarness::new(host.clone(), panels.clone(), script))));
        Self { host, focus, panels, welcome, harness, _subscriptions: subscriptions }
    }

    pub fn panels(&self) -> &Panels {
        &self.panels
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    fn open_folder(&mut self, _: &OpenFolder, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Map".into()),
        });
        let host = self.host.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                cx.update(|cx| host.send(Event::OpenLocation { path: path_bytes(&path), force: false }, cx)).ok();
            }
        })
        .detach();
    }

    fn send(&self, event: Event, cx: &mut App) {
        self.host.send(event, cx);
    }
}

/// A panel slot: the cached view sized by `style`, wrapped in frame timing.
fn slot(region: Regions, view: impl Into<AnyView>, style: StyleRefinement) -> impl IntoElement {
    timed(region, view.into().cached(style))
}

fn style() -> StyleRefinement {
    StyleRefinement::default()
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::begin_frame();
        if let Some(harness) = &self.harness {
            window.request_animation_frame();
            let harness = Rc::clone(harness);
            window.on_next_frame(move |window, cx| harness.borrow_mut().tick(window, cx));
        }
        let state = self.host.state(cx);
        let has_root = state.root_path().is_some();
        let can_refresh = has_root && !state.is_scanning() && !state.is_refreshing() && !state.is_removing();
        let can_go_up = state.can_go_up();
        let selected = state.selected();
        let has_selection = selected.is_some();
        let selected_dir = selected.is_some_and(|item| item.is_dir());
        let can_collect = state.can_collect_selection();
        let panels = &self.panels;

        let titlebar_drag = |el: gpui::Div| {
            el.on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
        };

        div()
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .bg(theme::color(theme::CANVAS))
            .text_color(theme::color(theme::TEXT))
            .font_family(theme::FONT_FAMILY)
            .key_context(WINDOW_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.panels.toolbar.update(cx, |toolbar, cx| toolbar.focus_search(window, cx));
            }))
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| {
                if lifecycle::may_quit(&this.host, cx) {
                    window.remove_window();
                }
            }))
            .when(can_refresh, |el| {
                el.on_action(cx.listener(|this, _: &RefreshFullMap, _, cx| this.send(Event::Refresh, cx)))
            })
            .when(can_go_up, |el| el.on_action(cx.listener(|this, _: &GoUp, _, cx| this.send(Event::GoUp, cx))))
            .when(selected_dir, |el| {
                el.on_action(cx.listener(|this, _: &OpenSelectedFolder, _, cx| this.send(Event::OpenSelection, cx)))
            })
            .when(has_selection, |el| {
                el.on_action(cx.listener(|this, _: &RevealInFinder, _, cx| this.send(Event::RevealSelection, cx)))
            })
            .when(has_selection && !selected_dir, |el| {
                el.on_action(cx.listener(|this, _: &QuickLook, _, cx| this.send(Event::PreviewSelection, cx)))
            })
            .when(can_collect, |el| {
                el.on_action(cx.listener(|this, _: &AddToCollection, _, cx| {
                    if let Some(item) = this.host.state(cx).selected() {
                        let paths = vec![item.path.clone()];
                        this.send(Event::Collect { paths }, cx);
                    }
                }))
            })
            .child(
                div()
                    .w(theme::SIDEBAR_WIDTH)
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .bg(theme::color(theme::SIDEBAR))
                    .border_r_1()
                    .border_color(theme::white(theme::DIVIDER_ALPHA))
                    .child(titlebar_drag(div().h(theme::TITLEBAR_HEIGHT).w_full().flex_none()))
                    .child(slot(Regions::SIDEBAR, panels.sidebar.clone(), style().flex_1().w_full().min_h_0())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(titlebar_drag(div().h(theme::TITLEBAR_HEIGHT).w_full().flex_none()).child(slot(
                        Regions::HEADER,
                        panels.toolbar.clone(),
                        style().size_full(),
                    )))
                    .child(slot(
                        Regions::HEADER,
                        panels.breadcrumbs.clone(),
                        style().h(theme::BREADCRUMB_HEIGHT).w_full().flex_none(),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_row()
                            .child(slot(Regions::MAP, panels.map.clone(), style().flex_1().min_w_0().h_full()))
                            .child(
                                div()
                                    .w(theme::CONTENTS_WIDTH)
                                    .flex_none()
                                    .h_full()
                                    .mr(theme::SPACE_XL)
                                    .mb(theme::SPACE_L)
                                    .flex()
                                    .flex_col()
                                    .bg(theme::color(theme::PANEL))
                                    .rounded(theme::RADIUS_PANEL)
                                    .overflow_hidden()
                                    .child(slot(
                                        Regions::LIST,
                                        panels.contents.clone(),
                                        style().flex_1().w_full().min_h_0(),
                                    ))
                                    .child(slot(
                                        Regions::INSPECTOR,
                                        panels.inspector.clone(),
                                        style().h(theme::INSPECTOR_HEIGHT).w_full().flex_none(),
                                    )),
                            ),
                    )
                    .child(slot(
                        Regions::FOOTER,
                        panels.footer.clone(),
                        style().h(theme::FOOTER_HEIGHT).w_full().flex_none(),
                    ))
                    .child(div().h(px(1.)).w_full().flex_none().bg(theme::white(theme::DIVIDER_ALPHA)))
                    .child(div().w_full().flex_none().bg(theme::color(theme::SIDEBAR)).child(slot(
                        Regions::TRAY,
                        panels.tray.clone(),
                        style().h(theme::TRAY_HEIGHT + theme::TRAY_GAP + px(theme::TRAY_MARGIN.1)).w_full(),
                    )))
                    // Over breadcrumbs, map and contents: from below the title bar to the footer.
                    .child(AnyView::from(self.welcome.clone()).cached(
                        style().absolute().top(theme::TITLEBAR_HEIGHT).left_0().right_0().bottom(
                            theme::FOOTER_HEIGHT
                                + px(1.)
                                + theme::TRAY_HEIGHT
                                + theme::TRAY_GAP
                                + px(theme::TRAY_MARGIN.1),
                        ),
                    )),
            )
            .child(slot(Regions::SHEETS, panels.sheets.clone(), style().absolute().top_0().left_0().size_full()))
            .child(frame_stats::end_marker())
    }
}

/// Window options of the main window (`DESIGN.md`: 1280 × 810, minimum 1060 × 680).
pub fn window_options(cx: &App) -> WindowOptions {
    let (w, h) = theme::WINDOW_DEFAULT;
    let (min_w, min_h) = theme::WINDOW_MIN;
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))),
        titlebar: Some(TitlebarOptions {
            title: Some("Orbit Disk".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(theme::TRAFFIC_LIGHTS.0), px(theme::TRAFFIC_LIGHTS.1))),
        }),
        focus: true,
        show: true,
        window_min_size: Some(size(px(min_w), px(min_h))),
        window_background: WindowBackgroundAppearance::Opaque,
        app_id: Some("local.orbitdisk.app".into()),
        ..Default::default()
    }
}

/// Opens the main window. Closing it is refused while a removal runs.
pub fn open(host: &Host, harness: Option<HarnessScript>, cx: &mut App) -> gpui::Result<WindowHandle<MainWindow>> {
    let options = window_options(cx);
    let host = host.clone();
    cx.open_window(options, move |window, cx| {
        let guard = host.clone();
        window.on_window_should_close(cx, move |_, cx| lifecycle::may_quit(&guard, cx));
        cx.new(|cx| MainWindow::new(host, harness, window, cx))
    })
}
