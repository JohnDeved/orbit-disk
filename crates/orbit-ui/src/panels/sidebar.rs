//! Places column (Swift `ContentView.sidebar`, `FolderPlaces`): 210 points wide below the title
//! bar.
//!
//! - "Devices": one card per volume (name, capacity bar, available space) in a fixed 100-point
//!   area that scrolls, so volume metadata arriving never moves Places.
//! - "Places": Home, Downloads, Documents, Applications, the user's folders ("Remove from Places"
//!   on right click) and "Choose a folder…". The highlighted place is the deepest one containing
//!   the current folder, compared by whole path components.
//! - Folders dropped from Finder onto the column are added with `AddPlaces`; the backend keeps
//!   directories only and app-state drops duplicates and built-in places. While such a drag hovers
//!   a folder drop target the label reads "Release to add folders". An explorer-area target (Swift
//!   also accepts drops on the map) can reuse [`add_dropped_folders`] and [`track_folder_drag`]
//!   with [`FolderDropHover::content`].
//!
//! Observes [`Regions::SIDEBAR`]; [`Regions::HEADER`] only re-renders it when the current folder
//! changed (the selection follows it), so typing a search does not redraw Places.

use std::path::PathBuf;

use gpui::{
    App, Context, CursorStyle, DragMoveEvent, ExternalPaths, FileDropEvent, InteractiveElement, IntoElement,
    MouseButton, MouseUpEvent, ParentElement, Pixels, Point, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, anchored, canvas, deferred, div, prelude::*, px, relative,
};
use orbit_app_state::places::selected_place;
use orbit_app_state::{AppState, Event, Regions};
use orbit_core::paths::{file_name, join};

use crate::actions::OpenFolder;
use crate::components::{section_label, tooltip, truncate_middle};
use crate::frame_stats;
use crate::host::{Host, path_bytes};
use crate::icons::{Icon, icon};
use crate::theme;

pub const REGIONS: Regions = Regions::SIDEBAR.union(Regions::HEADER);

/// Which folder drop targets a Finder drag hovers (the sidebar and an explorer-area target share
/// the "Release to add folders" label).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderDropHover {
    pub sidebar: bool,
    /// An explorer-area drop target (none is mounted yet).
    pub content: bool,
}

impl gpui::Global for FolderDropHover {}

impl FolderDropHover {
    pub fn get(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }

    /// Applies `change`; observers (the sidebar) hear about it only when something changed.
    pub fn update(change: impl FnOnce(&mut Self), cx: &mut App) {
        let before = Self::get(cx);
        let mut after = before;
        change(&mut after);
        if after != before {
            cx.set_global(after);
        }
    }
}

/// Sends dropped Finder items as `AddPlaces` (validated by the backend, deduplicated by app-state).
pub fn add_dropped_folders(host: &Host, paths: &[PathBuf], cx: &mut App) {
    let paths: Vec<Vec<u8>> = paths.iter().map(|path| path_bytes(path)).collect();
    if !paths.is_empty() {
        host.send(Event::AddPlaces(paths), cx);
    }
}

/// Tracks a Finder drag over one folder drop target (`on_drag_move::<ExternalPaths>`).
pub fn track_folder_drag(event: &DragMoveEvent<ExternalPaths>, set: fn(&mut FolderDropHover, bool), cx: &mut App) {
    let inside = event.bounds.contains(&event.event.position);
    FolderDropHover::update(|hover| set(hover, inside), cx);
}

/// The built-in places (Swift `ContentView.sidebar`).
fn builtin_places(home: &[u8]) -> [(&'static str, Icon, Vec<u8>); 4] {
    [
        ("Home", Icon::House, home.to_vec()),
        ("Downloads", Icon::CircleArrowDown, join(home, b"Downloads")),
        ("Documents", Icon::FileText, join(home, b"Documents")),
        ("Applications", Icon::LayoutGrid, b"/Applications".to_vec()),
    ]
}

/// The place to highlight: the deepest built-in or saved place containing the current folder.
pub fn highlighted_place(state: &AppState) -> Option<Vec<u8>> {
    let builtins = builtin_places(&state.config().home);
    let candidates =
        builtins.iter().map(|(_, _, path)| path.as_slice()).chain(state.places().iter().map(Vec::as_slice));
    selected_place(state.current_path(), candidates).map(<[u8]>::to_vec)
}

/// An open "Remove from Places" menu.
struct PlaceMenu {
    path: Vec<u8>,
    position: Point<Pixels>,
}

pub struct SidebarPanel {
    host: Host,
    /// The current folder at the last render (HEADER notifications that keep it are ignored).
    rendered_folder: Option<Vec<u8>>,
    /// The highlighted place at the last render.
    selected: Option<Vec<u8>>,
    menu: Option<PlaceMenu>,
    _subscriptions: Vec<Subscription>,
}

impl SidebarPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = host.observe(Regions::SIDEBAR, cx);
        subscriptions.push(cx.observe(host.signal(Regions::HEADER), |this: &mut Self, _, cx| {
            if this.host.state(cx).current_path() != this.rendered_folder.as_deref() {
                cx.notify();
            }
        }));
        subscriptions.push(cx.observe_global::<FolderDropHover>(|_, cx| cx.notify()));
        Self { host: host.clone(), rendered_folder: None, selected: None, menu: None, _subscriptions: subscriptions }
    }

    /// The highlighted place as last rendered.
    pub fn selected(&self) -> Option<&[u8]> {
        self.selected.as_deref()
    }

    /// Adds folders dropped from Finder (the `on_drop::<ExternalPaths>` handler).
    pub fn add_folders(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        FolderDropHover::update(|hover| *hover = FolderDropHover::default(), cx);
        add_dropped_folders(&self.host, paths, cx);
    }

    fn open(&self, path: Vec<u8>, cx: &mut App) {
        self.host.send(Event::OpenLocation { path, force: false }, cx);
    }

    fn volume_card(&self, index: usize, volume: &orbit_app_state::Volume, discovering: bool) -> impl IntoElement {
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
        div()
            .id(("volume", index))
            .px(px(12.))
            .pb(px(14.))
            .cursor(CursorStyle::PointingHand)
            .tooltip(tooltip(format!("Open saved map of {}", volume.name)))
            .on_click(move |_, _, cx| host.send(Event::OpenLocation { path: path.clone(), force: false }, cx))
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
                            .child(div().min_w_0().truncate().child(volume.name.clone())),
                    )
                    .child(
                        div().h(px(4.)).w_full().rounded(px(2.)).bg(theme::white(0.14)).when(
                            volume.capacity > 0,
                            |el| {
                                el.child(
                                    div().h_full().w(relative(used)).rounded(px(2.)).bg(theme::color(theme::CORAL)),
                                )
                            },
                        ),
                    )
                    .child(theme::monospaced_digits(
                        div()
                            .text_size(theme::TEXT_SMALL)
                            .text_color(theme::color(theme::SLATE))
                            .truncate()
                            .child(detail),
                    )),
            )
    }
}

/// A place row (Swift `PlaceRowButton`): full-width hit area, inset rounded background, coral
/// when selected. The 1-point outline is always laid out so selection never shifts the label.
fn place_row(
    id: impl Into<SharedString>,
    title: SharedString,
    glyph: Icon,
    selected: bool,
    vertical_padding: f32,
) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let group = id.clone();
    let fg = if selected { theme::color(theme::CORAL) } else { theme::color(theme::TEXT) };
    let (rest, hover, pressed) = if selected {
        (
            theme::with_alpha(theme::CORAL, 0.16),
            theme::with_alpha(theme::CORAL, 0.20),
            theme::with_alpha(theme::CORAL, 0.25),
        )
    } else {
        (theme::white(0.0), theme::white(0.07), theme::white(0.12))
    };
    div().id(id).group(group.clone()).px(px(12.)).py(px(2.)).cursor(CursorStyle::PointingHand).child(
        div()
            .id("row-fill")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(vertical_padding - 2.))
            .rounded(theme::RADIUS_ROW)
            .border_1()
            .border_color(if selected { theme::with_alpha(theme::CORAL, 0.45) } else { theme::white(0.0) })
            .bg(rest)
            .group_hover(group.clone(), move |s| s.bg(hover))
            .group_active(group, move |s| s.bg(pressed))
            .text_size(theme::TEXT_ROW)
            .text_color(fg)
            .child(icon(glyph).text_color(fg))
            .child(div().min_w_0().whitespace_nowrap().overflow_hidden().child(title)),
    )
}

/// Width left for a place title: column − row insets − icon − gap.
const PLACE_TITLE_WIDTH: f32 = 210. - 24. - 24. - 14. - 8. - 2.;

impl Render for SidebarPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(Regions::SIDEBAR);
        let state = self.host.state(cx);
        self.rendered_folder = state.current_path().map(<[u8]>::to_vec);
        self.selected = highlighted_place(state);
        let discovering = state.is_discovering_volumes();
        let volumes = state.volumes().to_vec();
        let builtins = builtin_places(&state.config().home);
        let saved: Vec<Vec<u8>> = state.places().to_vec();
        let drop_hover = FolderDropHover::get(cx);
        let targeted = drop_hover.sidebar || drop_hover.content;

        let mut devices = div().id("devices").h(theme::SIDEBAR_DEVICES_HEIGHT).flex_none().overflow_y_scroll();
        for (i, volume) in volumes.iter().enumerate() {
            devices = devices.child(self.volume_card(i, volume, discovering));
        }

        let mut places = div().id("places").flex_1().min_h_0().overflow_y_scroll().pb(theme::SPACE_XL);
        for (i, (title, glyph, path)) in builtins.into_iter().enumerate() {
            let selected = self.selected.as_deref() == Some(&path[..]);
            let open = cx.listener(move |this, _, _, cx| this.open(path.clone(), cx));
            places = places.child(place_row(format!("builtin-{i}"), title.into(), glyph, selected, 11.).on_click(open));
        }
        for (i, path) in saved.into_iter().enumerate() {
            let selected = self.selected.as_deref() == Some(&path[..]);
            let name = theme::display(if file_name(&path).is_empty() { &path } else { file_name(&path) });
            let title =
                truncate_middle(&name, px(PLACE_TITLE_WIDTH), theme::TEXT_ROW, gpui::FontWeight::NORMAL, window);
            let menu_path = path.clone();
            let open = cx.listener({
                let path = path.clone();
                move |this, _, _, cx| this.open(path.clone(), cx)
            });
            places = places.child(
                place_row(format!("place-{i}"), title, Icon::Folder, selected, 11.)
                    .tooltip(tooltip(theme::display(&path)))
                    .on_click(open)
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            this.menu = Some(PlaceMenu { path: menu_path.clone(), position: event.position });
                            cx.notify();
                        }),
                    ),
            );
        }
        places = places.child(
            place_row("choose-folder", "Choose a folder…".into(), Icon::FolderPlus, false, 12.)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
        );

        let menu = self.menu.as_ref().map(|menu| {
            let path = menu.path.clone();
            deferred(
                anchored().position(menu.position).child(
                    div()
                        .id("place-menu")
                        .occlude()
                        .min_w(px(170.))
                        .p(px(4.))
                        .rounded(px(7.))
                        .bg(theme::color(theme::PANEL))
                        .border_1()
                        .border_color(theme::white(0.14))
                        .text_size(theme::TEXT_ROW)
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .id("remove-place")
                                .px(px(10.))
                                .py(px(4.))
                                .rounded(px(4.))
                                .cursor(CursorStyle::PointingHand)
                                .hover(|s| {
                                    s.bg(theme::with_alpha(theme::CORAL, 0.85)).text_color(theme::color(theme::INK))
                                })
                                .child("Remove from Places")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.menu = None;
                                    this.host.send(Event::RemovePlace(path.clone()), cx);
                                    cx.notify();
                                })),
                        ),
                ),
            )
            .with_priority(1)
        });

        div()
            .id("sidebar")
            .size_full()
            .flex()
            .flex_col()
            .on_drag_move(|event: &DragMoveEvent<ExternalPaths>, _, cx| {
                track_folder_drag(event, |hover, inside| hover.sidebar = inside, cx)
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.add_folders(paths.paths(), cx)))
            .child(div().px(theme::SPACE_XL).pt(px(20.)).pb(theme::SPACE_M).child(section_label("Devices")))
            .child(devices)
            .child(
                div().px(theme::SPACE_XL).pt(px(22.)).pb(px(10.)).child(
                    section_label(if targeted { "Release to add folders" } else { "Places" })
                        .truncate()
                        .when(targeted, |el| el.text_color(theme::color(theme::CORAL))),
                ),
            )
            .child(places)
            .children(menu)
            .child(drop_reset_watcher())
    }
}

/// Clears [`FolderDropHover`] when a Finder drag leaves the window or any drag is released
/// (window-level listeners: neither event reaches a target's `on_drag_move`).
fn drop_reset_watcher() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, window, _| {
            window.on_mouse_event(|event: &FileDropEvent, _, _, cx| {
                if matches!(event, FileDropEvent::Exited) {
                    FolderDropHover::update(|hover| *hover = FolderDropHover::default(), cx);
                }
            });
            window.on_mouse_event(|_: &MouseUpEvent, _, _, cx| {
                FolderDropHover::update(|hover| *hover = FolderDropHover::default(), cx);
            });
        },
    )
    .absolute()
    .size_0()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use gpui::{AppContext, TestAppContext};
    use orbit_app_state::testing::Harness;
    use orbit_app_state::{AppState, Backend, Config, Event};

    use super::SidebarPanel;
    use crate::host::{AppHost, Host};
    use crate::settings::{Settings, SettingsStore};

    fn host_with(state: AppState, backend: Arc<dyn Backend>, cx: &mut TestAppContext) -> Host {
        cx.update(|cx| {
            let entity =
                cx.new(|cx| AppHost::with_state(state, backend, SettingsStore::in_memory(Settings::default()), cx));
            Host::from_entity(entity, cx)
        })
    }

    /// Home is `/r`; `/r/docs` is a saved place; `/r` is mapped.
    fn mapped(places: &[&str]) -> Harness {
        let mut config = Config::new(b"/r".to_vec());
        config.places = places.iter().map(|p| p.as_bytes().to_vec()).collect();
        let mut h = Harness::new(config);
        h.backend.add_file(b"/r/docs/deep/x.txt", 30);
        h.backend.add_file(b"/r/docsx/y.txt", 20);
        h.backend.add_file(b"/r/big/a.bin", 100);
        h.backend.add_file(b"/r/file1", 10);
        h.open_mapped(b"/r");
        h.settle();
        h
    }

    #[gpui::test]
    fn the_deepest_place_containing_the_folder_is_selected(cx: &mut TestAppContext) {
        let h = mapped(&["/r/docs"]);
        let host = host_with(h.state, h.backend, cx);
        let (sidebar, cx) = cx.add_window_view({
            let host = host.clone();
            move |_, cx| SidebarPanel::new(&host, cx)
        });
        let visit = |path: &str, cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| host.send(Event::Open { path: Arc::from(path.as_bytes()) }, cx));
            cx.run_until_parked();
            assert_eq!(cx.read(|cx| host.state(cx).current_path().map(<[u8]>::to_vec)), Some(path.as_bytes().to_vec()));
            cx.read(|cx| sidebar.read(cx).selected().map(|p| String::from_utf8_lossy(p).into_owned()))
        };
        assert_eq!(visit("/r/docs/deep", cx).as_deref(), Some("/r/docs"), "the saved place beats Home");
        assert_eq!(visit("/r/docs", cx).as_deref(), Some("/r/docs"));
        assert_eq!(visit("/r", cx).as_deref(), Some("/r"), "Home");
        assert_eq!(visit("/r/docsx", cx).as_deref(), Some("/r"), "whole components: /r/docsx is not in /r/docs");
    }

    #[gpui::test]
    fn finder_drops_add_new_directories_once(cx: &mut TestAppContext) {
        let h = mapped(&["/r/docs"]);
        let host = host_with(h.state, h.backend, cx);
        let (sidebar, cx) = cx.add_window_view({
            let host = host.clone();
            move |_, cx| SidebarPanel::new(&host, cx)
        });
        let drop = |paths: &[&str], cx: &mut gpui::VisualTestContext| {
            let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
            sidebar.update(cx, |sidebar, cx| sidebar.add_folders(&paths, cx));
            cx.run_until_parked();
        };
        // A new folder (twice), an existing place, Home (built in) and a file.
        drop(&["/r/big", "/r/big", "/r/docs", "/r", "/r/file1"], cx);
        drop(&["/r/big", "/r/docsx"], cx);
        let places = cx.read(|cx| host.state(cx).places().to_vec());
        assert_eq!(places, [b"/r/docs".to_vec(), b"/r/big".to_vec(), b"/r/docsx".to_vec()]);
    }
}
