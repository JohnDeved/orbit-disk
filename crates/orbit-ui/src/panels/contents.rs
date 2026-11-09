//! Contents list: the right column's header ("Contents" / "Search results" and the sort menu),
//! the 48-point status block (count and sort state, search state, the hovered map path, the
//! 300-match note), the empty states, and a virtualized list of fixed 40-point rows. Observes
//! [`Regions::LIST`].
//!
//! Rows (Swift `FileRowView`): branch swatch, name, a secondary line shared by the collection
//! mark and the file count / type / state / search folder, size and share in monospaced digits,
//! and the 32 × 36 folder arrow (immediate navigation). Selected rows: solid warm fill and a thick
//! coral inset outline; hovered and map-related rows: neutral fill and a thin slate outline.
//!
//! Pointer handling lives on the list, not on each row: the row under the pointer comes from the
//! scroll offset (`tracking::row_at_y`), so hovering costs one event per row change and rows are
//! plain visuals plus their arrow button and help tag.
//! - Hover → `HoverRow` / `ClearRowHover`, suppressed while the list scrolls (wheel, live
//!   scrolling, momentum) and restored at the row under the pointer [`SETTLE_DELAY`] after the
//!   offset settles (`tracking::ScrollHover`, GPUI timer).
//! - Press → `GestureBegin` + `Select`; release → `GestureEnd`, and a double click opens once
//!   (`gesture::RowClicks`): folders `Open`, files `PreviewSelection`.
//! - Moving the press beyond the 5-point threshold sends `DragBegin { source: Items }` and hands
//!   the pointer to a GPUI drag carrying [`CollectionDrag`]; Escape cancels (`DragEnd`). GPUI's
//!   own 2-point drag threshold never fires first because the drag listener is attached only
//!   after our threshold was crossed.
//! - Right click / control click → context menu (Open Folder, Reveal in Finder, Quick Look —
//!   opens with the default application in v1 — and Add to Collection).
//! - Keyboard (focused list): ↑/↓ move the selection, Return opens a selected folder; Space and
//!   ⌘↓ are the window's Quick Look / Open Selected Folder actions.
//! - Map hover reveals the first related row with the least scrolling, only when it is not
//!   fully visible; list reloads and re-sorts (the background name sort) keep the first visible
//!   row in place (`tracking::Anchor`).
//!
//! Per frame the list formats nothing it formatted before: row text is cached per display row
//! and list revision (`rows::RowCache`), and selection/hover are resolved once per frame
//! (`tracking::RowKey`), never by building row paths.

mod rows;
mod tracking;

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Bounds, ClickEvent, Context, CursorStyle, DispatchPhase, FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Render, ScrollStrategy, ScrollWheelEvent, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Task, UniformListScrollHandle, Window, anchored, canvas, deferred, div, fill, point,
    prelude::*, px, uniform_list,
};
use orbit_app_state::gesture::{RowClicks, exceeds_drag_threshold};
use orbit_app_state::{AppState, DragId, DragSource, Event, GestureId, Regions, SortOrder};
use orbit_core::types::ScanState;
use orbit_sunburst::CollectionState;

use self::rows::{RowCache, RowText};
use self::tracking::{Anchor, RowKey, ScrollHover, Settle};
use crate::components::{CollectionDrag, DragPreview, tooltip};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::theme;

pub const REGIONS: Regions = Regions::LIST;

/// Row hover resumes this long after the scroll offset stopped changing
/// (Swift `ListHoverTracking.settleDelay`).
pub const SETTLE_DELAY: Duration = Duration::from_millis(180);

/// The OS click count already applies the system double-click interval; this only bounds it.
const DOUBLE_CLICK_INTERVAL: f64 = 5.0;
/// Scrollbar gutter, reserved even for short lists.
const GUTTER: Pixels = px(12.);
/// Search results show at most this many rows (`orbit_app_state` `SEARCH_LIMIT`).
const SEARCH_LIMIT: u64 = 300;
/// Gesture and drag ids of this panel start here (the map and the frame harness use small ids).
const TOKEN_BASE: u64 = 1 << 48;

pub struct ContentsPanel {
    host: Host,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    shared: Rc<Shared>,
    hover: ScrollHover,
    settle_task: Option<Task<()>>,
    /// A handler already reported this frame's scrolling.
    scroll_noted: bool,
    /// Last pointer position over the rows (window coordinates).
    pointer: Option<Point<Pixels>>,
    /// The row this panel last hovered: (display row, list revision).
    hovered: Option<(usize, u64)>,
    press: Option<Press>,
    clicks: RowClicks,
    clock: Instant,
    next_token: u64,
    thumb_drag: Option<ThumbDrag>,
    menu: Option<OpenMenu>,
    cache: RowCache,
    view: ListView,
    anchor: Anchor,
    _subscriptions: Vec<Subscription>,
}

/// State read by window-level listeners without touching the entity.
#[derive(Debug, Default)]
struct Shared {
    /// A row press or scrollbar drag is held: window-level moves and releases go to the panel.
    tracking: Cell<bool>,
    /// The pointer is over the rows (leaving is detected by the window-level listener).
    inside: Cell<bool>,
    rows_bounds: Cell<Bounds<Pixels>>,
    gutter: Cell<Bounds<Pixels>>,
    /// The scroll offset of the last painted frame: render compares it to see input scrolling.
    painted_offset: Cell<Point<Pixels>>,
}

/// A pointer press on a row.
#[derive(Debug)]
struct Press {
    display: usize,
    revision: u64,
    path: Arc<[u8]>,
    is_dir: bool,
    label: SharedString,
    size: u64,
    origin: Point<Pixels>,
    gesture: GestureId,
    /// Sent `DragBegin` with this id after the press moved beyond the threshold.
    drag: Option<DragId>,
    /// Escape cancelled the drag; the release neither drags nor clicks.
    cancelled: bool,
}

#[derive(Clone, Copy, Debug)]
struct ThumbDrag {
    start_y: Pixels,
    start_offset: Pixels,
}

#[derive(Debug)]
struct OpenMenu {
    menu: Menu,
    position: Point<Pixels>,
}

#[derive(Debug)]
enum Menu {
    Row { path: Arc<[u8]>, is_dir: bool, can_collect: bool },
    Sort,
}

/// What the list showed last frame, to tell reloads/re-sorts from navigation.
#[derive(Debug, Default)]
struct ListView {
    revision: u64,
    folder: Vec<u8>,
    filtering: bool,
    query: String,
    /// Address of the last `HoverPresentation` acted on (a new one is a new map hover).
    hover: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowStyle {
    Plain,
    Hovered,
    Selected,
}

impl ContentsPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self {
            host: host.clone(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            shared: Rc::default(),
            hover: ScrollHover::default(),
            settle_task: None,
            scroll_noted: false,
            pointer: None,
            hovered: None,
            press: None,
            clicks: RowClicks::default(),
            clock: Instant::now(),
            next_token: TOKEN_BASE,
            thumb_drag: None,
            menu: None,
            cache: RowCache::default(),
            view: ListView::default(),
            anchor: Anchor::default(),
            _subscriptions: host.observe(REGIONS, cx),
        }
    }

    /// Scrolls the list by `dy` (negative scrolls down), like a wheel event over it: moves the
    /// offset and starts hover suppression.
    pub fn scroll_by(&mut self, dy: Pixels, cx: &mut Context<Self>) {
        let base = self.base();
        let offset = base.offset();
        let count = self.host.state(cx).row_count();
        let viewport = base.bounds().size.height;
        let y = tracking::clamp_offset(offset.y + dy, count, viewport);
        base.set_offset(point(offset.x, y));
        self.note_scroll(cx);
        cx.notify();
    }

    /// The pointer moved over the list to `position` (window coordinates), as the list's
    /// mouse-move handler reports it.
    pub fn pointer_moved(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.pointer = Some(position);
        self.shared.inside.set(true);
        if self.press.is_some() || self.hover.suppressed() {
            return;
        }
        self.hover_at(position, cx);
    }

    /// Current vertical scroll offset (≤ 0).
    pub fn scroll_offset(&self) -> Pixels {
        self.base().offset().y
    }

    /// Row hover is suppressed because the list is scrolling.
    pub fn hover_suppressed(&self) -> bool {
        self.hover.suppressed()
    }

    /// Rows formatted so far (each row once per list revision while it stays near the view).
    pub fn rows_formatted(&self) -> u64 {
        self.cache.formatted
    }

    fn base(&self) -> gpui::ScrollHandle {
        self.scroll.0.borrow().base_handle.clone()
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    /// The display row under `position` (window coordinates).
    fn row_at(&self, position: Point<Pixels>, state: &AppState) -> Option<usize> {
        let base = self.base();
        let bounds = base.bounds();
        if !bounds.contains(&position) {
            return None;
        }
        tracking::row_at_y(position.y, bounds.top(), base.offset().y, state.row_count())
    }

    // -----------------------------------------------------------------------------------------
    // Hover and scrolling.

    fn note_scroll(&mut self, cx: &mut Context<Self>) {
        self.scroll_noted = true;
        let (began, epoch) = self.hover.scrolled();
        if began {
            self.hovered = None;
            if self.host.state(cx).row_hover().is_some() {
                let host = self.host.clone();
                cx.defer(move |cx| host.send(Event::ClearRowHover, cx));
            }
        }
        self.arm_settle(epoch, cx);
    }

    fn arm_settle(&mut self, epoch: u64, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(SETTLE_DELAY);
        self.settle_task = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| this.settle(epoch, cx)).ok();
        }));
    }

    fn settle(&mut self, epoch: u64, cx: &mut Context<Self>) {
        match self.hover.settle(epoch) {
            Settle::Stale => {}
            Settle::Wait => self.arm_settle(epoch, cx),
            Settle::Restore => {
                self.settle_task = None;
                cx.notify();
                if let Some(pointer) = self.pointer.filter(|_| self.shared.inside.get() && self.press.is_none()) {
                    self.hover_at(pointer, cx);
                }
            }
        }
    }

    fn hover_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let state = self.host.state(cx);
        let revision = state.list_revision();
        match self.row_at(position, state) {
            Some(display) => {
                if self.hovered == Some((display, revision)) && state.row_hover().is_some() {
                    return;
                }
                let Some(row) = state.row(display) else { return };
                let path: Arc<[u8]> = row.path().into();
                self.hovered = Some((display, revision));
                self.host.send(Event::HoverRow { path }, cx);
            }
            None => {
                if self.hovered.take().is_some() || state.row_hover().is_some() {
                    self.host.send(Event::ClearRowHover, cx);
                }
            }
        }
    }

    fn pointer_left(&mut self, cx: &mut Context<Self>) {
        self.pointer = None;
        self.shared.inside.set(false);
        if self.hovered.take().is_some() || self.host.state(cx).row_hover().is_some() {
            self.host.send(Event::ClearRowHover, cx);
        }
    }

    fn wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        self.hover.phase(event.touch_phase);
        self.pointer = Some(event.position);
        self.shared.inside.set(true);
        self.note_scroll(cx);
    }

    // -----------------------------------------------------------------------------------------
    // Press, click, drag.

    fn pressed(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if let Some(stale) = self.press.take() {
            // The release of the previous press never arrived (e.g. it happened outside the app).
            self.clicks.cancel();
            if let Some(drag) = stale.drag {
                self.host.send(Event::DragEnd { drag }, cx);
            }
            self.host.send(Event::GestureEnd(stale.gesture), cx);
        }
        if event.modifiers.control {
            self.open_row_menu(event.position, cx);
            return;
        }
        window.focus(&self.focus);
        let state = self.host.state(cx);
        let Some(row) = self.row_at(event.position, state).and_then(|display| state.row(display).map(|r| (display, r)))
        else {
            // A click on the empty space below the rows clears the selection (native lists).
            if state.selected().is_some() {
                self.host.send(Event::Select { path: None }, cx);
            }
            return;
        };
        let (display, row) = row;
        let path: Arc<[u8]> = row.path().into();
        let (is_dir, size) = (row.meta().flags.is_dir, row.meta().size);
        let label: SharedString = theme::display(row.name()).into();
        let revision = state.list_revision();
        let gesture = GestureId(self.token());
        let click_count = u32::try_from(event.click_count).unwrap_or(u32::MAX);
        self.clicks.press(Arc::clone(&path), self.clock.elapsed().as_secs_f64(), click_count, DOUBLE_CLICK_INTERVAL);
        self.press = Some(Press {
            display,
            revision,
            path: Arc::clone(&path),
            is_dir,
            label,
            size,
            origin: event.position,
            gesture,
            drag: None,
            cancelled: false,
        });
        self.shared.tracking.set(true);
        self.host.send(Event::GestureBegin(gesture), cx);
        self.host.send(Event::Select { path: Some(path) }, cx);
    }

    /// A window-level mouse move while a press or scrollbar drag is held.
    fn pointer_dragged(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if let Some(thumb) = self.thumb_drag {
            self.drag_thumb(thumb, event.position.y, cx);
            return;
        }
        let Some(press) = self.press.as_ref() else { return };
        if press.drag.is_some() || press.cancelled {
            return;
        }
        let xy = |p: Point<Pixels>| (f64::from(f32::from(p.x)), f64::from(f32::from(p.y)));
        if !exceeds_drag_threshold(xy(press.origin), xy(event.position)) {
            return;
        }
        self.clicks.moved();
        let drag = DragId(self.token());
        let Some(press) = self.press.as_mut() else { return };
        press.drag = Some(drag);
        let source = DragSource::Items(vec![Arc::clone(&press.path)]);
        let label = press.label.to_string();
        self.host.send(Event::DragBegin { drag, source, label }, cx);
        // The next frame attaches the GPUI drag; the next move starts it.
        cx.notify();
    }

    /// A window-level mouse up while a press or scrollbar drag is held.
    fn released(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.button != MouseButton::Left {
            return;
        }
        self.shared.tracking.set(false);
        if self.thumb_drag.take().is_some() {
            return;
        }
        let Some(press) = self.press.take() else { return };
        let state = self.host.state(cx);
        let inside = !press.cancelled
            && state.list_revision() == press.revision
            && self.row_at(event.position, state) == Some(press.display);
        let open = self.clicks.release(inside);
        let host = self.host.clone();
        // After this event's drop targets ran: a drag nobody accepted ends here.
        window.defer(cx, move |_, cx| {
            if let Some(drag) = press.drag
                && host.state(cx).drag_id() == Some(drag)
            {
                host.send(Event::DragEnd { drag }, cx);
            }
            host.send(Event::GestureEnd(press.gesture), cx);
            if let Some(path) = open {
                host.send(if press.is_dir { Event::Open { path } } else { Event::PreviewSelection }, cx);
            }
        });
        cx.notify();
    }

    /// Closes an open row or sort menu (Escape). Returns whether one was open.
    pub fn dismiss_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.menu.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    /// Escape during a row drag: the drag ends without a drop.
    fn cancel_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(press) = self.press.as_mut() else { return false };
        let Some(drag) = press.drag.take() else { return false };
        press.cancelled = true;
        self.clicks.cancel();
        cx.stop_active_drag(window);
        self.host.send(Event::DragEnd { drag }, cx);
        cx.notify();
        true
    }

    fn open_row(&mut self, display: usize, revision: u64, cx: &mut Context<Self>) {
        let state = self.host.state(cx);
        if state.list_revision() != revision {
            return;
        }
        let Some(row) = state.row(display) else { return };
        let path: Arc<[u8]> = row.path().into();
        self.host.send(Event::Open { path }, cx);
    }

    // -----------------------------------------------------------------------------------------
    // Keyboard.

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let plain = !keystroke.modifiers.modified();
        let handled = match keystroke.key.as_str() {
            "escape" => {
                let closed = self.menu.take().is_some();
                if closed {
                    cx.notify();
                }
                self.cancel_drag(window, cx) || closed
            }
            "down" if plain => self.step_selection(1, cx),
            "up" if plain => self.step_selection(-1, cx),
            "enter" if plain => {
                let open = self.host.state(cx).selected().is_some_and(|item| item.is_dir());
                if open {
                    self.host.send(Event::OpenSelection, cx);
                }
                open
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn step_selection(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let state = self.host.state(cx);
        let count = state.row_count();
        if count == 0 {
            return false;
        }
        let current = RowKey::of(state, state.selected().map(|item| &*item.path)).display(state);
        let next = match current {
            Some(row) => row.saturating_add_signed(delta).min(count - 1),
            None if delta > 0 => 0,
            None => count - 1,
        };
        if Some(next) == current {
            return true;
        }
        let Some(row) = state.row(next) else { return false };
        let path: Arc<[u8]> = row.path().into();
        self.reveal(next);
        self.host.send(Event::Select { path: Some(path) }, cx);
        true
    }

    /// Scrolls the least amount that shows `row` fully; nothing when it is visible.
    fn reveal(&self, row: usize) {
        let base = self.base();
        let viewport = base.bounds().size.height;
        if viewport <= px(0.) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Top);
            return;
        }
        let offset = base.offset();
        if let Some(y) = tracking::reveal_offset(row, offset.y, viewport) {
            base.set_offset(point(offset.x, y));
        }
    }

    // -----------------------------------------------------------------------------------------
    // Scrollbar.

    fn gutter_pressed(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let base = self.base();
        let viewport = base.bounds().size.height;
        let content = theme::ROW_HEIGHT * self.host.state(cx).row_count();
        let gutter = self.shared.gutter.get();
        let offset = base.offset();
        let Some(thumb) = tracking::thumb(gutter, viewport, content, offset.y) else { return };
        if !thumb.contains(&position) {
            // Jump so the thumb centers on the pointer, then drag from there.
            let travel = gutter.size.height - px(4.) - thumb.size.height;
            let progress = if travel > px(0.) {
                ((position.y - gutter.top() - px(2.) - thumb.size.height / 2.) / travel).clamp(0., 1.)
            } else {
                0.
            };
            base.set_offset(point(offset.x, -((content - viewport) * progress)));
        }
        self.thumb_drag = Some(ThumbDrag { start_y: position.y, start_offset: base.offset().y });
        self.shared.tracking.set(true);
        self.note_scroll(cx);
        cx.notify();
    }

    fn drag_thumb(&mut self, thumb: ThumbDrag, y: Pixels, cx: &mut Context<Self>) {
        let base = self.base();
        let viewport = base.bounds().size.height;
        let count = self.host.state(cx).row_count();
        let content = theme::ROW_HEIGHT * count;
        let gutter = self.shared.gutter.get();
        let Some(bounds) = tracking::thumb(gutter, viewport, content, base.offset().y) else { return };
        let travel = gutter.size.height - px(4.) - bounds.size.height;
        if travel <= px(0.) {
            return;
        }
        let scale = (content - viewport) / travel;
        let offset_y = tracking::clamp_offset(thumb.start_offset - (y - thumb.start_y) * scale, count, viewport);
        base.set_offset(point(base.offset().x, offset_y));
        self.note_scroll(cx);
        cx.notify();
    }

    // -----------------------------------------------------------------------------------------
    // Menus.

    fn open_row_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let state = self.host.state(cx);
        let Some(row) = self.row_at(position, state).and_then(|display| state.row(display)) else { return };
        let item = row.item();
        let can_collect = state.can_collect(&item);
        self.menu =
            Some(OpenMenu { menu: Menu::Row { is_dir: item.is_dir(), path: item.path, can_collect }, position });
        cx.notify();
    }

    fn menu_item(
        &self,
        id: &'static str,
        label: &'static str,
        enabled: bool,
        checked: bool,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &Context<Self>,
    ) -> AnyElement {
        let fg = if enabled { theme::color(theme::TEXT) } else { theme::with_alpha(theme::SLATE, 0.6) };
        div()
            .id(id)
            .h(px(24.))
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .rounded(px(4.))
            .text_color(fg)
            .child(
                div()
                    .w(px(12.))
                    .flex_none()
                    .when(checked, |el| el.child(icon(Icon::Check).size(px(12.)).text_color(fg))),
            )
            .child(label)
            .when(enabled, |el| {
                el.cursor(CursorStyle::PointingHand).hover(|s| s.bg(theme::white(0.1))).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.menu = None;
                        action(this, cx);
                        cx.notify();
                    },
                ))
            })
            .into_any_element()
    }

    fn menu(&self, open: &OpenMenu, state: &AppState, cx: &Context<Self>) -> impl IntoElement + use<> {
        let items: Vec<AnyElement> = match &open.menu {
            Menu::Row { path, is_dir, can_collect } => {
                let select = |path: &Arc<[u8]>, then: fn(Arc<[u8]>) -> Event| {
                    let path = Arc::clone(path);
                    move |this: &mut Self, cx: &mut Context<Self>| {
                        this.host.send(Event::Select { path: Some(Arc::clone(&path)) }, cx);
                        this.host.send(then(Arc::clone(&path)), cx);
                    }
                };
                let mut items = Vec::with_capacity(3);
                if *is_dir {
                    let path = Arc::clone(path);
                    items.push(self.menu_item(
                        "menu-open",
                        "Open Folder",
                        true,
                        false,
                        move |this, cx| this.host.send(Event::Open { path: Arc::clone(&path) }, cx),
                        cx,
                    ));
                }
                items.push(self.menu_item(
                    "menu-reveal",
                    "Reveal in Finder",
                    true,
                    false,
                    select(path, |_| Event::RevealSelection),
                    cx,
                ));
                if !*is_dir {
                    items.push(self.menu_item(
                        "menu-preview",
                        "Quick Look",
                        true,
                        false,
                        select(path, |_| Event::PreviewSelection),
                        cx,
                    ));
                }
                items.push(self.menu_item(
                    "menu-collect",
                    "Add to Collection",
                    *can_collect,
                    false,
                    select(path, |path| Event::Collect { paths: vec![path] }),
                    cx,
                ));
                items
            }
            Menu::Sort => {
                let by_name = state.sort_order() == SortOrder::Name;
                vec![
                    self.menu_item(
                        "menu-sort-size",
                        "Largest first",
                        true,
                        !by_name,
                        |this, cx| this.host.send(Event::SortChanged(SortOrder::LargestFirst), cx),
                        cx,
                    ),
                    self.menu_item(
                        "menu-sort-name",
                        "Name",
                        true,
                        by_name,
                        |this, cx| this.host.send(Event::SortChanged(SortOrder::Name), cx),
                        cx,
                    ),
                ]
            }
        };
        deferred(
            anchored().position(open.position).snap_to_window().child(
                div()
                    .id("contents-menu")
                    .occlude()
                    .min_w(px(180.))
                    .p(px(4.))
                    .rounded(px(7.))
                    .bg(theme::color(theme::SIDEBAR))
                    .border_1()
                    .border_color(theme::white(0.14))
                    .shadow_lg()
                    .text_size(theme::TEXT_ROW)
                    .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.menu = None;
                        cx.notify();
                    }))
                    .children(items),
            ),
        )
        .with_priority(1)
    }

    // -----------------------------------------------------------------------------------------
    // Frame.

    /// Keeps the list in place across reloads and re-sorts, resets it on navigation, reveals the
    /// row related to a new map hover, and notices input scrolling.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let base = self.base();
        let mut offset = base.offset();
        if offset != self.shared.painted_offset.get() && !self.scroll_noted {
            self.note_scroll(cx);
        }
        self.scroll_noted = false;
        let viewport = base.bounds().size.height;
        let state = self.host.state(cx);
        let count = state.row_count();
        let revision = state.list_revision();
        let filtering = state.filtering();
        let folder = state.current_path().unwrap_or_default();
        let navigated = folder != self.view.folder.as_slice()
            || filtering != self.view.filtering
            || (filtering && state.query() != self.view.query);
        let mut target = None;
        let mut reveal = None;
        if navigated {
            self.view.folder.clear();
            self.view.folder.extend_from_slice(folder);
            self.view.filtering = filtering;
            self.view.query.clear();
            self.view.query.push_str(state.query());
            self.anchor.clear();
            target = Some(px(0.));
            // Going up keeps the departed folder selected: show it.
            reveal = RowKey::of(state, state.selected().map(|item| &*item.path)).display(state);
        } else if revision != self.view.revision && offset.y < px(0.) {
            target = self.anchor.restore(state);
        }
        if let Some(y) = target {
            offset.y = if viewport > px(0.) { tracking::clamp_offset(y, count, viewport) } else { y.min(px(0.)) };
        }
        let reloaded = revision != self.view.revision;
        self.view.revision = revision;
        let hover = std::ptr::from_ref(state.hover()).addr();
        if hover != self.view.hover || (reloaded && !state.hover().is_empty()) {
            self.view.hover = hover;
            // A related row on screen is enough; only then search the rows (a scan in name order).
            let scroll_top = (-offset.y).max(px(0.));
            let first = (scroll_top / theme::ROW_HEIGHT).floor() as usize;
            let last = ((scroll_top + viewport) / theme::ROW_HEIGHT).floor() as usize;
            let shown = (first..=last.min(count.saturating_sub(1)))
                .any(|row| tracking::reveal_offset(row, offset.y, viewport).is_none() && state.is_row_related(row));
            if !shown && !state.hover().is_empty() {
                reveal = state.first_related_row().or(reveal);
            }
        }
        if let Some(row) = reveal {
            if viewport > px(0.) {
                if let Some(y) = tracking::reveal_offset(row, offset.y, viewport) {
                    offset.y = y;
                }
            } else {
                self.scroll.scroll_to_item(row, ScrollStrategy::Top);
            }
        }
        if offset != base.offset() {
            base.set_offset(offset);
        }
        self.anchor.capture(state, offset.y);
    }

    fn rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = self.host.state(cx);
        self.cache.begin(state, &range);
        let selected = RowKey::of(state, state.selected().map(|item| &*item.path));
        let hovered = if self.hover.suppressed() { RowKey::None } else { RowKey::of(state, state.row_hover()) };
        let revision = state.list_revision();
        let mut rows = Vec::with_capacity(range.len());
        for display in range {
            let Some(text) = self.cache.get(state, display) else { continue };
            let style = if selected.matches(state, display) {
                RowStyle::Selected
            } else if hovered.matches(state, display) || state.is_row_related(display) {
                RowStyle::Hovered
            } else {
                RowStyle::Plain
            };
            rows.push(self.row(display, revision, text, style, cx));
        }
        rows
    }

    fn row(&self, display: usize, revision: u64, text: RowText, style: RowStyle, cx: &Context<Self>) -> AnyElement {
        let (fill, outline) = match style {
            RowStyle::Selected => (theme::color(theme::ROW_SELECTED), Some((theme::color(theme::CORAL), px(2.5)))),
            RowStyle::Hovered => {
                (theme::color(theme::ROW_HOVER), Some((theme::color(theme::ROW_HOVER_OUTLINE), px(1.))))
            }
            RowStyle::Plain => (theme::white(0.0), None),
        };
        let slate = theme::color(theme::SLATE);
        let mark = match text.mark {
            CollectionState::None => None,
            CollectionState::Included => Some((Icon::CircleCheckFill, "In Collection")),
            CollectionState::Partial => Some((Icon::CircleMinusFill, "Partly collected")),
        };
        let trailing = if text.is_dir {
            div()
                .id(("open-row", display))
                .flex_none()
                .w(px(theme::ROW_ARROW.0))
                .h(px(theme::ROW_ARROW.1))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .bg(theme::white(0.07))
                .hover(|s| s.bg(theme::white(0.14)))
                .cursor(CursorStyle::PointingHand)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_row(display, revision, cx)))
                .tooltip(row_help(self.host.clone(), display, revision, true))
                .child(icon(Icon::ChevronRight).size(px(17.)).text_color(theme::color(theme::TEXT)))
                .into_any_element()
        } else {
            div()
                .flex_none()
                .w(px(theme::ROW_ARROW.0))
                .h(px(theme::ROW_ARROW.1))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(Icon::File).size(px(12.)).text_color(slate))
                .into_any_element()
        };
        div()
            .id(display)
            .h(theme::ROW_HEIGHT)
            .w_full()
            .pl(px(8.))
            .pr(px(4.))
            .cursor(CursorStyle::PointingHand)
            .tooltip(row_help(self.host.clone(), display, revision, false))
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(11.))
                    .px(px(5.))
                    .rounded(px(7.))
                    .bg(fill)
                    .child(div().flex_none().w(px(7.)).h(px(30.)).rounded(px(3.)).bg(text.swatch))
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
                                    .child(text.name),
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
                                    .when(!text.detail.is_empty(), |el| {
                                        el.child(div().min_w_0().truncate().text_color(slate).child(text.detail))
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
                                    .child(text.size),
                            ))
                            .child(theme::monospaced_digits(
                                div()
                                    .h(px(13.))
                                    .text_size(theme::TEXT_CAPTION)
                                    .text_color(slate)
                                    .whitespace_nowrap()
                                    .child(text.share),
                            )),
                    )
                    .child(trailing)
                    .when_some(outline, |el, (color, width)| {
                        el.child(div().absolute().inset_0().rounded(px(7.)).border(width).border_color(color))
                    }),
            )
            .into_any_element()
    }

    /// Paints the scrollbar thumb and records the painted offset and geometry.
    fn scrollbar(&self, count: usize, cx: &Context<Self>) -> impl IntoElement + use<> {
        let scroll = self.scroll.clone();
        let shared = Rc::clone(&self.shared);
        div()
            .id("contents-scrollbar")
            .w(GUTTER)
            .h_full()
            .flex_none()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.gutter_pressed(event.position, cx);
                }),
            )
            .child(
                canvas(
                    |_, _, _| {},
                    move |gutter, _, window, _| {
                        let base = scroll.0.borrow().base_handle.clone();
                        let offset = base.offset();
                        let list = base.bounds();
                        shared.painted_offset.set(offset);
                        shared.gutter.set(gutter);
                        shared.rows_bounds.set(list);
                        let content = theme::ROW_HEIGHT * count;
                        if let Some(thumb) = tracking::thumb(gutter, list.size.height, content, offset.y) {
                            window.paint_quad(fill(thumb, theme::white(0.28)).corner_radii(px(3.)));
                        }
                    },
                )
                .size_full(),
            )
    }

    /// Window-level listeners: drags and releases of a held press go on outside the list, and
    /// leaving the rows clears the row hover.
    fn window_listeners(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity().downgrade();
        let shared = Rc::clone(&self.shared);
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let (moved, moved_shared) = (this.clone(), Rc::clone(&shared));
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    if moved_shared.tracking.get() {
                        moved.update(cx, |this, cx| this.pointer_dragged(event, cx)).ok();
                    }
                    if moved_shared.inside.get() && !moved_shared.rows_bounds.get().contains(&event.position) {
                        moved_shared.inside.set(false);
                        moved.update(cx, |this, cx| this.pointer_left(cx)).ok();
                    }
                });
                let (released, released_shared) = (this.clone(), Rc::clone(&shared));
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble && released_shared.tracking.get() {
                        released.update(cx, |this, cx| this.released(event, window, cx)).ok();
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }
}

/// The help tag of a row (its path) or of its folder arrow ("Open <name>"), built on demand.
fn row_help(
    host: Host,
    display: usize,
    revision: u64,
    arrow: bool,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |window, cx| {
        let state = host.state(cx);
        let text = match state.row(display).filter(|_| state.list_revision() == revision) {
            Some(row) if arrow => format!("Open {}", theme::display(row.name())),
            Some(row) => theme::display(&row.path()),
            None => String::new(),
        };
        tooltip(text)(window, cx)
    }
}

/// First status line (Swift `filePanel`).
fn caption(state: &AppState) -> String {
    if state.filtering() {
        let empty = state.row_count() == 0;
        if state.is_searching() {
            return if empty { "Searching…" } else { "Searching… · showing previous results" }.to_owned();
        }
        if state.search_outdated() {
            return match (empty, state.search_failed()) {
                (true, true) => "Search unavailable · Refresh to retry",
                (true, false) => "Search incomplete",
                (false, true) => "Previous results · Refresh to retry",
                (false, false) => "Previous results",
            }
            .to_owned();
        }
        let matches = state.search_match_count();
        return format!(
            "{} {} {}{}",
            theme::format_count(matches),
            if matches == 1 { "match" } else { "matches" },
            if state.is_partial() { "so far" } else { "in this index" },
            if state.is_sorting_rows() { " · sorting by name…" } else { "" }
        );
    }
    let Some(folder) = state.folder() else { return String::new() };
    let order = if state.is_sorting_rows() {
        "sorting by name…"
    } else if folder.folder_meta().state != ScanState::Complete {
        "sizes measured so far"
    } else if state.sort_order() == SortOrder::Name {
        "sorted by name"
    } else {
        "largest first"
    };
    let rows = state.row_count() as u64;
    format!("{} {} · {order}", theme::format_count(rows), if rows == 1 { "item" } else { "items" })
}

/// Second status line: the hovered map path (`true`), or the search limit note.
fn note(state: &AppState) -> Option<(bool, SharedString)> {
    if let Some(description) = state.hover_description() {
        return Some((true, SharedString::from(description.to_owned())));
    }
    let limited = state.filtering()
        && !state.is_searching()
        && !state.search_outdated()
        && state.search_match_count() > SEARCH_LIMIT;
    limited.then(|| (false, "Showing the 300 largest matches. Refine your search for more.".into()))
}

/// The empty list's icon, message and optional hint; `None` before any folder is shown.
fn empty_state(state: &AppState) -> Option<(Icon, &'static str, Option<&'static str>)> {
    if state.filtering() {
        let text = if state.is_searching() {
            "Searching…"
        } else if state.search_outdated() {
            if state.search_failed() { "Search unavailable. Refresh to retry." } else { "Search incomplete" }
        } else if state.is_partial() {
            "No matches found so far"
        } else {
            "No matching files"
        };
        return Some((Icon::Search, text, None));
    }
    let meta = state.folder()?.folder_meta();
    let text = if state.is_searching() {
        "Searching…"
    } else if !meta.flags.accessible {
        "Contents unavailable. See skipped items."
    } else if meta.state == ScanState::Pending {
        "Waiting to map this folder"
    } else if meta.state != ScanState::Complete {
        "Reading this folder…"
    } else {
        "This folder is empty"
    };
    let hint =
        (meta.state != ScanState::Complete && !state.is_scanning()).then_some("Restart mapping to finish this folder.");
    Some((Icon::Folder, text, hint))
}

impl Render for ContentsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        self.sync(cx);
        let state = self.host.state(cx);
        let count = state.row_count();
        let filtering = state.filtering();
        let title = if filtering { "Search results" } else { "Contents" };
        let sort = if state.sort_order() == SortOrder::Name { "Name" } else { "Size" };
        let caption: SharedString = caption(state).into();
        let note = note(state);
        let empty = (count == 0).then(|| empty_state(state));
        let drag = self.press.as_ref().and_then(|press| {
            let drag = press.drag.filter(|drag| state.drag_id() == Some(*drag))?;
            Some(CollectionDrag {
                drag,
                label: press.label.clone(),
                detail: theme::format_bytes(press.size).into(),
                icon: if press.is_dir { Icon::FolderFill } else { Icon::FileFill },
            })
        });
        let menu = self.menu.as_ref().map(|open| self.menu(open, state, cx));
        let slate = theme::color(theme::SLATE);

        let header = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .px(theme::SPACE_L)
            .pt(theme::SPACE_L)
            .pb(px(10.))
            .child(div().flex_1().text_size(theme::TEXT_HEADING).font_weight(theme::WEIGHT_SEMIBOLD).child(title))
            .child(
                // A borderless pop-up menu button: coral arrow, label and menu chevron. Fixed
                // width, so switching between "Size" and "Name" never moves the title.
                div()
                    .id("contents-sort")
                    .w(px(72.))
                    .h(px(22.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap(px(3.))
                    .text_size(theme::TEXT_ROW)
                    .font_weight(theme::WEIGHT_MEDIUM)
                    .text_color(theme::color(theme::CORAL))
                    .rounded(px(5.))
                    .hover(|s| s.text_color(theme::with_alpha(theme::CORAL, 0.8)))
                    .cursor(CursorStyle::PointingHand)
                    .tooltip(tooltip("Sort contents"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.menu = Some(OpenMenu { menu: Menu::Sort, position: event.position() });
                        cx.notify();
                    }))
                    .child(icon(Icon::ArrowDown).size(px(14.)).text_color(theme::color(theme::CORAL)))
                    .child(sort)
                    .child(icon(Icon::ChevronDown).size(px(10.)).text_color(theme::color(theme::CORAL))),
            );

        let status = div()
            .flex_none()
            .h(px(58.))
            .px(theme::SPACE_L)
            .pb(px(10.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(theme::TEXT_SMALL)
            .child(div().h(px(14.)).text_color(slate).truncate().child(caption))
            .when_some(note, |el, (hover, text)| {
                el.child(if hover {
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(4.))
                        .text_color(theme::color(theme::CORAL))
                        .child(icon(Icon::Pointer).size(px(11.)).mt(px(1.)).text_color(theme::color(theme::CORAL)))
                        .child(div().flex_1().min_w_0().line_clamp(2).child(text))
                } else {
                    div().text_color(slate).line_clamp(2).child(text)
                })
            });

        let body = match empty {
            Some(empty) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .px(px(22.))
                .when_some(empty, |el, (glyph, text, hint)| {
                    el.child(icon(glyph).size(px(25.)).text_color(slate))
                        .child(div().text_size(theme::TEXT_ROW).text_color(slate).child(text))
                        .when_some(hint, |el, hint| {
                            el.child(div().text_size(theme::TEXT_SMALL).text_color(slate).text_center().child(hint))
                        })
                })
                .into_any_element(),
            None => div()
                .id("contents-list")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .pt(px(4.))
                .track_focus(&self.focus)
                .key_context("ContentsList")
                .on_key_down(cx.listener(Self::key_down))
                .child(
                    div()
                        .id("contents-rows")
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                            this.pointer_moved(event.position, cx);
                        }))
                        .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| this.wheel(event, cx)))
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::pressed))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.open_row_menu(event.position, cx);
                            }),
                        )
                        // Lets GPUI record the press, so the drag attached after our threshold can start.
                        .on_click(|_, _, _| {})
                        .when_some(drag, |el, payload| {
                            el.on_drag(payload, |payload, offset, _, cx| cx.new(|_| DragPreview::new(payload, offset)))
                        })
                        .child(
                            uniform_list(
                                "contents-rows-list",
                                count,
                                cx.processor(|this, range: Range<usize>, _, cx| this.rows(range, cx)),
                            )
                            .track_scroll(self.scroll.clone())
                            .size_full(),
                        ),
                )
                .child(self.scrollbar(count, cx))
                .into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(status)
            .child(div().flex_none().mx(theme::SPACE_L).h(px(1.)).bg(theme::white(0.1)))
            .child(body)
            .child(self.window_listeners(cx))
            .children(menu)
    }
}
