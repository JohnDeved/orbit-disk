//! The Settings window (Swift `UpdateSettingsView`, 420 points wide): automatic update checks,
//! the daily schedule, a manual check, and what restarting means (the current collection is not
//! saved). Uses `Shell::updater`; builds without an updater say so.

use std::rc::Rc;

use gpui::{
    App, AppContext, Bounds, Context, CursorStyle, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, div, px, size,
};
use orbit_platform::Shell;

use crate::actions::CloseWindow;
use crate::components::{ButtonStyle, button};
use crate::host::{Host, HostEvent};
use crate::icons::{Icon, icon};
use crate::theme;

pub struct SettingsWindow {
    shell: Rc<dyn Shell>,
    host: Host,
    _subscriptions: Vec<Subscription>,
}

impl SettingsWindow {
    pub fn new(shell: Rc<dyn Shell>, host: Host, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let removal = cx.subscribe(host.entity(), |_, _, _: &HostEvent, cx| cx.notify());
        // The updater has no change notifications: re-read its state whenever the window
        // becomes key (a check may have finished or started meanwhile).
        let activation = cx.observe_window_activation(window, |_, _, cx| cx.notify());
        Self { shell, host, _subscriptions: vec![removal, activation] }
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let removing = self.host.state(cx).removal_active();
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(theme::SPACE_XL)
            .bg(theme::color(theme::PANEL))
            .text_color(theme::color(theme::TEXT))
            .font_family(theme::FONT_FAMILY)
            .text_size(theme::TEXT_ROW)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .child(div().text_size(theme::TEXT_ROW).font_weight(theme::WEIGHT_SEMIBOLD).child("Updates"));
        let Some(updater) = self.shell.updater() else {
            return root.child(
                div()
                    .text_color(theme::color(theme::SLATE))
                    .child("Updates are not available in this build of Orbit Disk."),
            );
        };
        let state = updater.state();
        let shell = Rc::clone(&self.shell);
        let toggle = div()
            .id("automatic-updates")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .cursor(CursorStyle::PointingHand)
            .child(
                div()
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(theme::with_alpha(theme::SLATE, 0.6))
                    .when(state.automatic_checks, |el| {
                        el.bg(theme::color(theme::CORAL))
                            .child(icon(Icon::Check).size(px(12.)).text_color(theme::color(theme::CANVAS)))
                    }),
            )
            .child("Automatically check for updates")
            .on_click(cx.listener(move |_, _, _, cx| {
                if let Some(updater) = shell.updater() {
                    updater.set_automatic_checks(!updater.state().automatic_checks);
                }
                cx.notify();
            }));
        let shell = Rc::clone(&self.shell);
        root.child(toggle)
            .child(
                div()
                    .text_size(theme::TEXT_BODY)
                    .text_color(theme::color(theme::SLATE))
                    .child("Checks for updates daily while Orbit Disk is open."),
            )
            // Reserved height: the text changes while a removal postpones the restart.
            .child(div().h(px(36.)).text_size(theme::TEXT_BODY).text_color(theme::color(theme::SLATE)).child(
                if state.waiting_for_removal {
                    "The update will restart Orbit Disk after file removal finishes."
                } else {
                    "Restarting clears the current collection."
                },
            ))
            .child(div().flex().child(button(
                "check-now",
                "Check for Updates…",
                None,
                ButtonStyle::Bordered,
                state.can_check && !removing,
                move |_, _, _| {
                    if let Some(updater) = shell.updater() {
                        updater.check_now();
                    }
                },
            )))
    }
}

/// Opens the Settings window (420 points wide, not resizable).
pub fn open(shell: Rc<dyn Shell>, host: Host, cx: &mut App) -> gpui::Result<WindowHandle<SettingsWindow>> {
    let bounds = Bounds::centered(None, size(px(420.), px(250.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Settings".into()), ..Default::default() }),
            is_resizable: false,
            is_minimizable: false,
            kind: WindowKind::Normal,
            focus: true,
            show: true,
            ..Default::default()
        },
        |window, cx| cx.new(|cx| SettingsWindow::new(shell, host, window, cx)),
    )
}

use gpui::prelude::*;
