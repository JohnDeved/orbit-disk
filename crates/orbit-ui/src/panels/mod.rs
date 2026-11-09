//! Panel views, one module each. Every panel:
//! - is its own GPUI view (`Entity<…Panel>`) built with `new(&Host, cx)`;
//! - subscribes to its region(s) with `host.observe(REGIONS, cx)` and keeps the subscriptions;
//! - calls `frame_stats::rendered(region)` first in `render`;
//! - reads `host.state(cx)` in `render` and emits with `host.send(Event::…, cx)` from handlers;
//! - is mounted by [`crate::main_window`] as a cached view with a fixed slot style, so it renders
//!   only when one of its regions is notified (or its own `cx.notify()` for local UI state).
//!
//! Panels never read or write each other. Shared visuals go to `components`/`theme`.

pub mod contents;
pub mod footer;
pub mod inspector;
pub mod map;
pub mod sidebar;
pub mod toolbar;
pub mod tray;
pub mod welcome;

use gpui::{AppContext, Context, Entity};

use crate::host::Host;
use crate::sheets::SheetsLayer;

/// The window's panel views.
#[derive(Clone)]
pub struct Panels {
    pub toolbar: Entity<toolbar::ToolbarPanel>,
    pub breadcrumbs: Entity<toolbar::BreadcrumbBar>,
    pub sidebar: Entity<sidebar::SidebarPanel>,
    pub map: Entity<map::MapPanel>,
    pub contents: Entity<contents::ContentsPanel>,
    pub inspector: Entity<inspector::InspectorPanel>,
    pub footer: Entity<footer::FooterPanel>,
    pub tray: Entity<tray::TrayPanel>,
    pub sheets: Entity<SheetsLayer>,
}

impl Panels {
    pub fn new<V: 'static>(host: &Host, cx: &mut Context<V>) -> Self {
        Self {
            toolbar: cx.new(|cx| toolbar::ToolbarPanel::new(host, cx)),
            breadcrumbs: cx.new(|cx| toolbar::BreadcrumbBar::new(host, cx)),
            sidebar: cx.new(|cx| sidebar::SidebarPanel::new(host, cx)),
            map: cx.new(|cx| map::MapPanel::new(host, cx)),
            contents: cx.new(|cx| contents::ContentsPanel::new(host, cx)),
            inspector: cx.new(|cx| inspector::InspectorPanel::new(host, cx)),
            footer: cx.new(|cx| footer::FooterPanel::new(host, cx)),
            tray: cx.new(|cx| tray::TrayPanel::new(host, cx)),
            sheets: cx.new(|cx| SheetsLayer::new(host, cx)),
        }
    }
}
