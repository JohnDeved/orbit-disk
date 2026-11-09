//! PLACEHOLDER — sidebar (owner: toolbar/sidebar/footer panel agent).
//!
//! The 210-point places column below the title bar: "Devices" (volumes with a usage bar, fixed
//! 100-point area) and "Places" (Home, Downloads, Documents, Applications, saved places,
//! "Choose a folder…"). Observes [`Regions::SIDEBAR`] and [`Regions::HEADER`] (the selected
//! place follows the current folder). Click → `OpenLocation`. Still to do: hover/pressed styles,
//! folder drops (`AddPlaces`), "Remove from Places" context menu, help texts, scrolling.

use gpui::{
    Context, CursorStyle, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px, relative,
};
use orbit_app_state::{Event, Regions};
use orbit_core::paths::{file_name, join};

use crate::actions::OpenFolder;
use crate::components::section_label;
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::theme;

pub const REGIONS: Regions = Regions::SIDEBAR.union(Regions::HEADER);

pub struct SidebarPanel {
    host: Host,
    _subscriptions: Vec<Subscription>,
}

impl SidebarPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self { host: host.clone(), _subscriptions: host.observe(REGIONS, cx) }
    }
}

fn place_row(id: SharedString, title: SharedString, glyph: Icon, selected: bool) -> gpui::Stateful<gpui::Div> {
    let fg = if selected { theme::color(theme::CORAL) } else { theme::color(theme::TEXT) };
    div().id(id).px(px(12.)).py(px(2.)).cursor(CursorStyle::PointingHand).child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(9.))
            .rounded(theme::RADIUS_ROW)
            .text_size(theme::TEXT_ROW)
            .text_color(fg)
            .when(selected, |el| {
                el.bg(theme::with_alpha(theme::CORAL, 0.16))
                    .border_1()
                    .border_color(theme::with_alpha(theme::CORAL, 0.45))
            })
            .child(icon(glyph).text_color(fg))
            .child(div().truncate().child(title)),
    )
}

impl Render for SidebarPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(Regions::SIDEBAR);
        let state = self.host.state(cx);
        let home = state.config().home.clone();
        let selected_place = state.selected_place().map(<[u8]>::to_vec);
        let discovering = state.is_discovering_volumes();
        let volumes: Vec<_> = state.volumes().to_vec();
        let mut places: Vec<(String, Icon, Vec<u8>)> = vec![
            ("Home".into(), Icon::House, home.clone()),
            ("Downloads".into(), Icon::CircleArrowDown, join(&home, b"Downloads")),
            ("Documents".into(), Icon::FileText, join(&home, b"Documents")),
            ("Applications".into(), Icon::LayoutGrid, b"/Applications".to_vec()),
        ];
        places.extend(state.places().iter().map(|p| (theme::display(file_name(p)), Icon::Folder, p.clone())));

        let mut column = div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().px(theme::SPACE_XL).pt(px(20.)).pb(theme::SPACE_M).child(section_label("Devices")));
        let mut devices = div().h(px(100.)).flex().flex_col();
        for (i, volume) in volumes.into_iter().enumerate() {
            let used = if volume.capacity > 0 {
                (volume.capacity - volume.available.min(volume.capacity)) as f32 / volume.capacity as f32
            } else {
                0.0
            };
            let detail = if volume.capacity > 0 {
                format!("{} available", theme::format_bytes(volume.available))
            } else if discovering {
                "Checking available space…".to_owned()
            } else {
                "Storage info unavailable".to_owned()
            };
            let host = self.host.clone();
            let path = volume.path.clone();
            devices = devices.child(
                div()
                    .id(("volume", i))
                    .px(px(12.))
                    .pb(px(14.))
                    .cursor(CursorStyle::PointingHand)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .p(px(13.))
                            .rounded(theme::RADIUS_CARD)
                            .bg(theme::white(0.045))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(8.))
                                    .items_center()
                                    .text_size(theme::TEXT_ROW)
                                    .font_weight(theme::WEIGHT_MEDIUM)
                                    .child(icon(Icon::HardDrive))
                                    .child(div().truncate().child(volume.name.clone())),
                            )
                            .child(
                                div().h(px(4.)).w_full().rounded(px(2.)).bg(theme::white(0.14)).child(
                                    div().h_full().w(relative(used)).rounded(px(2.)).bg(theme::color(theme::CORAL)),
                                ),
                            )
                            .child(theme::monospaced_digits(
                                div().text_size(theme::TEXT_SMALL).text_color(theme::color(theme::SLATE)).child(detail),
                            )),
                    )
                    .on_click(move |_, _, cx| host.send(Event::OpenLocation { path: path.clone(), force: false }, cx)),
            );
        }
        column = column
            .child(devices)
            .child(div().px(theme::SPACE_XL).pt(px(22.)).pb(px(10.)).child(section_label("Places")));
        for (i, (title, glyph, path)) in places.into_iter().enumerate() {
            let selected = selected_place.as_deref() == Some(&path[..]);
            let host = self.host.clone();
            column = column.child(
                place_row(SharedString::from(format!("place-{i}")), title.into(), glyph, selected)
                    .on_click(move |_, _, cx| host.send(Event::OpenLocation { path: path.clone(), force: false }, cx)),
            );
        }
        column.child(
            place_row("choose-folder".into(), "Choose a folder…".into(), Icon::FolderPlus, false)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
        )
    }
}

use gpui::prelude::*;
