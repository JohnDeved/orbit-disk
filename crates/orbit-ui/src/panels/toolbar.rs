//! Toolbar and breadcrumbs (Swift `ContentView` `headerLocation`, `header`, `explorer` header row).
//!
//! [`ToolbarPanel`]: the 52-point unified title bar row right of the sidebar. The location block
//! (name and middle-truncated path, 280 points, inset 142 after the traffic lights), then the
//! search field (210 × 34), Refresh and Open Folder… in fixed-width slots: navigation, labels and
//! enabled states never shift the title bar. Without a map the search and Refresh slots stay
//! reserved but empty. Observes [`Regions::HEADER`].
//!
//! [`SearchField`]: a GPUI text input (key context `TextInput`, so Space types a space). Every edit
//! sends `QueryChanged`; app-state debounces the search. Grapheme-cluster cursor movement, ⌥
//! word motion and deletion, ⌘Z / ⇧⌘Z undo and redo ([`TextEdit`]). Escape clears, the clear
//! button too; a focused field always consumes Escape (it never stops mapping). A
//! keystroke re-renders the field (and its toolbar parent) plus what app-state notifies for the
//! query (list, inspector, header) — nothing else.
//!
//! [`BreadcrumbBar`]: the 48-point explorer header: Up, horizontally scrolling breadcrumbs (kept
//! scrolled to the current folder), a 14-point slot for the opening indicator and a fixed
//! 138-point file count. Observes [`Regions::HEADER`].

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, InteractiveElement, IntoElement, KeyDownEvent,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, ParentElement, Pixels, Point,
    Render, ScrollHandle, ShapedLine, SharedString, StatefulInteractiveElement, Style, Styled, Subscription, TextRun,
    UTF16Selection, UnderlineStyle, Window, div, fill, point, prelude::*, px, relative, size,
};
use orbit_app_state::{Event, Regions};
use orbit_core::paths::file_name;
use orbit_core::types::ScanState;
use orbit_core::view::FolderTree;

use crate::actions::{Copy, Cut, OpenFolder, Paste, Redo, SelectAll, TEXT_INPUT_CONTEXT, Undo};
use crate::components::{ButtonStyle, activity_indicator, button, tooltip, truncate_middle};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::text_edit::{EditKind, TextEdit};
use crate::theme;

pub const REGIONS: Regions = Regions::HEADER;

pub struct ToolbarPanel {
    host: Host,
    search: Entity<SearchField>,
    _subscriptions: Vec<Subscription>,
}

impl ToolbarPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| SearchField::new(host, cx));
        Self { host: host.clone(), search, _subscriptions: host.observe(REGIONS, cx) }
    }

    /// ⌘F.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.search.read(cx).focus.clone();
        if self.search.read(cx).enabled {
            window.focus(&focus);
        }
    }

    pub fn search_field(&self) -> &Entity<SearchField> {
        &self.search
    }
}

impl Focusable for ToolbarPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.read(cx).focus.clone()
    }
}

impl Render for ToolbarPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let host = self.host.read(cx);
        let state = host.state();
        let has_root = state.root_path().is_some();
        let (name, path) = match state.folder() {
            Some(folder) => {
                let tree = folder.tree();
                let path = folder.folder_path();
                let name = tree.name(FolderTree::FOLDER);
                let name = if name.is_empty() { file_name(path) } else { name };
                (theme::display(if name.is_empty() { path } else { name }), theme::display(path))
            }
            None => (String::new(), String::new()),
        };
        let notice = host.notice().cloned();
        let subtitle = match &notice {
            Some(notice) => notice.clone(),
            None => {
                truncate_middle(&path, theme::TOOLBAR_TITLE_WIDTH, theme::TEXT_SMALL, gpui::FontWeight::NORMAL, window)
            }
        };
        let can_refresh = has_root && !state.is_scanning() && !state.is_refreshing() && !state.is_removing();
        let partial = state.is_partial();
        let search_enabled = has_root && !(state.is_scanning() && !partial);
        let query = state.query().to_owned();
        self.search.update(cx, |field, _| field.sync(&query, partial, search_enabled));

        let title_inset = theme::TRAFFIC_LIGHT_AREA + theme::TOOLBAR_TITLE_INSET - f32::from(theme::SIDEBAR_WIDTH);
        let refresh = self.host.clone();
        let controls = if has_root {
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(theme::TOOLBAR_SPACING)
                .child(
                    div()
                        .id("search-slot")
                        .flex_none()
                        .tooltip(tooltip(if partial {
                            "Search files mapped so far"
                        } else {
                            "Search the saved file index"
                        }))
                        .child(self.search.clone()),
                )
                .child(
                    div()
                        .id("refresh-slot")
                        .w(theme::TOOLBAR_REFRESH_WIDTH)
                        .flex_none()
                        .flex()
                        .justify_end()
                        .tooltip(tooltip("Refresh the full map (⌘R)"))
                        .child(button(
                            "refresh",
                            "Refresh",
                            Some(Icon::Refresh),
                            ButtonStyle::Bordered,
                            can_refresh,
                            move |_, _, cx| refresh.send(Event::Refresh, cx),
                        )),
                )
        } else {
            // Reserved, empty: the toolbar does not shift when a map opens.
            div()
                .flex_none()
                .w(theme::TOOLBAR_SEARCH_WIDTH + theme::TOOLBAR_SPACING + theme::TOOLBAR_REFRESH_WIDTH)
                .h(theme::TOOLBAR_SEARCH_HEIGHT)
        };
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
                    .h(px(40.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(3.))
                    .child(
                        div()
                            .h(px(21.))
                            .text_size(theme::TEXT_TITLE)
                            .font_weight(theme::WEIGHT_SEMIBOLD)
                            .truncate()
                            .child(name),
                    )
                    .child(
                        div()
                            .h(px(14.))
                            .text_size(theme::TEXT_SMALL)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_color(if notice.is_some() {
                                theme::color(theme::CORAL)
                            } else {
                                theme::color(theme::SLATE)
                            })
                            .child(subtitle),
                    ),
            )
            .child(div().flex_1().min_w_0())
            .child(controls)
            .child(div().w(theme::TOOLBAR_OPEN_WIDTH).flex_none().flex().justify_end().child(button(
                "open-folder",
                "Open Folder…",
                Some(Icon::FolderPlus),
                ButtonStyle::Bordered,
                true,
                |_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx),
            )))
    }
}

// ---------------------------------------------------------------------------------------------
// Search field.

/// The toolbar's search text input (after GPUI's `examples/input.rs`). Editing, grapheme and
/// word motion and undo/redo live in [`TextEdit`].
pub struct SearchField {
    host: Host,
    focus: FocusHandle,
    edit: TextEdit,
    /// IME composition range.
    marked: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// Horizontal scroll of a query longer than the field.
    scroll_x: Pixels,
    selecting: bool,
    partial: bool,
    enabled: bool,
}

impl SearchField {
    fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self {
            host: host.clone(),
            focus: cx.focus_handle(),
            edit: TextEdit::default(),
            marked: None,
            last_layout: None,
            last_bounds: None,
            scroll_x: px(0.),
            selecting: false,
            partial: false,
            enabled: false,
        }
    }

    pub fn text(&self) -> &str {
        self.edit.text()
    }

    /// Takes the state's query when it changed elsewhere (not while composing).
    fn sync(&mut self, query: &str, partial: bool, enabled: bool) {
        self.partial = partial;
        self.enabled = enabled;
        if self.edit.text() != query && self.marked.is_none() {
            self.edit.reset(query);
        }
    }

    /// Tells the state about a changed query.
    fn edited(&mut self, changed: bool, cx: &mut Context<Self>) {
        cx.notify();
        if changed {
            self.host.send(Event::QueryChanged(self.edit.text().to_owned()), cx);
        }
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        self.edit.end_composition();
        let changed = self.edit.replace(0..self.edit.text().len(), "", EditKind::Other);
        self.edited(changed, cx);
    }

    fn replace(&mut self, range: Range<usize>, text: &str, kind: EditKind, cx: &mut Context<Self>) {
        self.marked = None;
        let changed = self.edit.replace(range, &text.replace('\n', " "), kind);
        self.edited(changed, cx);
    }

    /// Deletes the selection, or from the cursor to `target` when nothing is selected.
    fn delete_to(&mut self, target: usize, cx: &mut Context<Self>) {
        let selected = self.edit.selected();
        let range = if selected.is_empty() {
            let cursor = self.edit.cursor();
            cursor.min(target)..cursor.max(target)
        } else {
            selected
        };
        self.replace(range, "", EditKind::Deletion, cx);
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            // A focused text input owns Escape: it clears the query and never stops mapping.
            if self.enabled && self.marked.is_none() && !self.edit.text().is_empty() {
                self.clear(cx);
            }
            cx.stop_propagation();
            return;
        }
        if !self.enabled || self.marked.is_some() {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        let (shift, word, line) = (modifiers.shift, modifiers.alt, modifiers.platform);
        let edit = &self.edit;
        let cursor = edit.cursor();
        let handled = match key {
            "backspace" => {
                let target = if line {
                    0
                } else if word {
                    edit.previous_word(cursor)
                } else {
                    edit.previous_grapheme(cursor)
                };
                self.delete_to(target, cx);
                true
            }
            "delete" => {
                let len = edit.text().len();
                let target = if line {
                    len
                } else if word {
                    edit.next_word(cursor)
                } else {
                    edit.next_grapheme(cursor)
                };
                self.delete_to(target, cx);
                true
            }
            "left" | "right" | "home" | "end" | "up" | "down" => {
                let selected = edit.selected();
                let len = edit.text().len();
                let target = match key {
                    "home" | "up" => 0,
                    "end" | "down" => len,
                    "left" if line => 0,
                    "right" if line => len,
                    "left" if word => edit.previous_word(cursor),
                    "right" if word => edit.next_word(cursor),
                    "left" if !shift && !selected.is_empty() => selected.start,
                    "right" if !shift && !selected.is_empty() => selected.end,
                    "left" => edit.previous_grapheme(cursor),
                    _ => edit.next_grapheme(cursor),
                };
                if shift {
                    self.edit.select_to(target);
                } else {
                    self.edit.move_to(target);
                }
                cx.notify();
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.edit.select_all();
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.edit.selected();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.edit.text()[selected].to_owned()));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.edit.selected();
        if !selected.is_empty() && self.enabled {
            cx.write_to_clipboard(ClipboardItem::new_string(self.edit.text()[selected.clone()].to_owned()));
            self.replace(selected, "", EditKind::Other, cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
            && self.enabled
        {
            self.replace(self.edit.selected(), &text, EditKind::Other, cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.enabled && self.marked.is_none() && self.edit.undo() {
            self.edited(true, cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.enabled && self.marked.is_none() && self.edit.redo() {
            self.edited(true, cx);
        }
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else { return 0 };
        let text = self.edit.text();
        if text.is_empty() || position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return text.len();
        }
        self.edit.snap(line.closest_index_for_x(position.x - bounds.left() + self.scroll_x))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled {
            return;
        }
        window.focus(&self.focus);
        self.selecting = true;
        let index = self.index_for_position(event.position);
        if event.click_count >= 2 {
            self.edit.select_all();
        } else if event.modifiers.shift {
            self.edit.select_to(index);
        } else {
            self.edit.move_to(index);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.edit.select_to(self.index_for_position(event.position));
            cx.notify();
        }
    }

    fn utf16_to_utf8(&self, offset: usize) -> usize {
        utf16_to_utf8_in(self.edit.text(), offset)
    }

    fn utf8_to_utf16(&self, offset: usize) -> usize {
        let text = self.edit.text();
        text[..offset.min(text.len())].chars().map(char::len_utf16).sum()
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.utf16_to_utf8(range.start)..self.utf16_to_utf8(range.end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.utf8_to_utf16(range.start)..self.utf8_to_utf16(range.end)
    }
}

fn utf16_to_utf8_in(text: &str, offset: usize) -> usize {
    let mut utf16 = 0;
    for (i, c) in text.char_indices() {
        if utf16 >= offset {
            return i;
        }
        utf16 += c.len_utf16();
    }
    text.len()
}

impl Focusable for SearchField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for SearchField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range);
        actual.replace(self.range_to_utf16(&range));
        Some(self.edit.text()[range].to_owned())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.edit.selected()), reversed: self.edit.reversed() })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
        self.edit.end_composition();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled {
            return;
        }
        let range = range
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.edit.selected());
        self.replace(range, text, EditKind::Typing, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled {
            return;
        }
        let range = range
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.edit.selected());
        // Composition text is shown but neither searched nor recorded until committed.
        let current = self.edit.text();
        let content = format!("{}{}{}", &current[..range.start], text, &current[range.end..]);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        let selection = selected
            .as_ref()
            .map(|sel| range.start + utf16_to_utf8_in(text, sel.start)..range.start + utf16_to_utf8_in(text, sel.end))
            .unwrap_or_else(|| range.start + text.len()..range.start + text.len());
        self.edit.compose(content, selection);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(range.start) - self.scroll_x, bounds.top()),
            point(bounds.left() + line.x_for_index(range.end) - self.scroll_x, bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let index = line.index_for_x(position.x - bounds.left() + self.scroll_x)?;
        Some(self.utf8_to_utf16(index))
    }
}

impl Render for SearchField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let has_text = !self.edit.text().is_empty();
        let enabled = self.enabled;
        div()
            .id("search")
            .key_context(TEXT_INPUT_CONTEXT)
            .track_focus(&self.focus)
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
            .border_color(if focused && enabled {
                theme::color(theme::CORAL)
            } else {
                theme::with_alpha(theme::SLATE, 0.6)
            })
            .when(!enabled, |el| el.opacity(0.5))
            .text_size(theme::TEXT_ROW)
            .cursor(if enabled { CursorStyle::IBeam } else { CursorStyle::Arrow })
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .when(self.edit.can_undo(), |el| el.on_action(cx.listener(Self::undo)))
            .when(self.edit.can_redo(), |el| el.on_action(cx.listener(Self::redo)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(icon(Icon::Search).text_color(theme::color(theme::SLATE)))
            .child(div().flex_1().min_w_0().h(px(16.)).overflow_hidden().child(TextLine { field: cx.entity() }))
            .child(div().id("clear-search").size(px(14.)).flex_none().when(has_text, |el| {
                el.cursor(CursorStyle::PointingHand)
                    .tooltip(tooltip("Clear search"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.clear(cx)))
                    .child(icon(Icon::CircleXFill).text_color(theme::color(theme::SLATE)))
            }))
    }
}

/// Paints the field's text, selection and caret and registers the platform input handler.
struct TextLine {
    field: Entity<SearchField>,
}

struct TextLinePrepaint {
    line: ShapedLine,
    origin: Point<Pixels>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextLine {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextLine {
    type RequestLayoutState = ();
    type PrepaintState = TextLinePrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> TextLinePrepaint {
        let field = self.field.read(cx);
        let style = window.text_style();
        let placeholder = if field.partial { "Search mapped files" } else { "Search indexed files" };
        let content = field.edit.text();
        let (text, color): (SharedString, gpui::Hsla) = if content.is_empty() {
            (placeholder.into(), theme::color(theme::SLATE).into())
        } else {
            (content.to_owned().into(), theme::color(theme::TEXT).into())
        };
        let mut font = style.font();
        font.family = theme::FONT_FAMILY.into();
        let run =
            TextRun { len: text.len(), font, color, background_color: None, underline: None, strikethrough: None };
        let runs = match field.marked.as_ref().filter(|_| !content.is_empty()) {
            Some(marked) => [
                TextRun { len: marked.start, ..run.clone() },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.), wavy: false }),
                    ..run.clone()
                },
                TextRun { len: text.len() - marked.end, ..run },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(text, font_size, &runs, None);
        let (cursor, selected) =
            if content.is_empty() { (0, 0..0) } else { (field.edit.cursor(), field.edit.selected()) };
        // Keep the caret visible in a query longer than the field.
        let cursor_x = line.x_for_index(cursor);
        let width = bounds.size.width - px(2.);
        let scroll_x = if cursor_x > width { cursor_x - width } else { px(0.) };
        self.field.update(cx, |field, _| field.scroll_x = scroll_x);
        let line_height = font_size * 1.25;
        let origin = point(bounds.left() - scroll_x, bounds.top() + (bounds.size.height - line_height) / 2.);
        let (selection, cursor) = if selected.is_empty() {
            let caret = fill(
                Bounds::new(point(origin.x + cursor_x, origin.y), size(px(1.5), line_height)),
                theme::color(theme::TEXT),
            );
            (None, Some(caret))
        } else {
            let selection = fill(
                Bounds::from_corners(
                    point(origin.x + line.x_for_index(selected.start), origin.y),
                    point(origin.x + line.x_for_index(selected.end), origin.y + line_height),
                ),
                theme::with_alpha(theme::CORAL, 0.35),
            );
            (Some(selection), None)
        };
        TextLinePrepaint { line, origin, cursor, selection }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut TextLinePrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let field = self.field.read(cx);
        let focus = field.focus.clone();
        let enabled = field.enabled;
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.field.clone()), cx);
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let line_height = prepaint.cursor.as_ref().map_or(bounds.size.height, |c| c.bounds.size.height);
        prepaint.line.paint(prepaint.origin, line_height, window, cx).ok();
        if enabled
            && focus.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        let line = prepaint.line.clone();
        self.field.update(cx, |field, _| {
            field.last_layout = Some(line);
            field.last_bounds = Some(bounds);
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Breadcrumbs.

pub struct BreadcrumbBar {
    host: Host,
    scroll: ScrollHandle,
    /// The folder of the last render: a new folder scrolls the trail to its end.
    rendered_folder: Option<Vec<u8>>,
    _subscriptions: Vec<Subscription>,
}

impl BreadcrumbBar {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self {
            host: host.clone(),
            scroll: ScrollHandle::new(),
            rendered_folder: None,
            _subscriptions: host.observe(REGIONS, cx).into_iter().chain([theme::observe_motion(cx)]).collect(),
        }
    }
}

/// `(path, name)` from the mapped root down to the current folder.
fn crumbs(folder: &orbit_app_state::PreparedFolder) -> Vec<(Vec<u8>, Vec<u8>)> {
    let root = folder.root_path();
    let current = folder.folder_path();
    let mut paths = vec![root.to_vec()];
    if current.len() > root.len() && current.starts_with(root) {
        let mut prefix = root.to_vec();
        for part in current[root.len()..].split(|b| *b == b'/').filter(|p| !p.is_empty()) {
            if prefix.last() != Some(&b'/') {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(part);
            paths.push(prefix.clone());
        }
    }
    let tree = folder.tree();
    let mut names: Vec<&[u8]> = tree.ancestors.iter().map(|&ix| tree.name(ix)).collect();
    names.push(tree.name(FolderTree::FOLDER));
    let named = names.len() == paths.len();
    paths
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            let name = if named && !names[i].is_empty() { names[i] } else { file_name(&path) };
            let name = if name.is_empty() { path.clone() } else { name.to_vec() };
            (path, name)
        })
        .collect()
}

impl Render for BreadcrumbBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let reduce_motion = theme::reduce_motion(cx);
        let state = self.host.state(cx);
        let can_go_up = state.can_go_up();
        let opening = state.is_opening();
        let (crumbs, files) = match state.folder() {
            Some(folder) => {
                let meta = folder.folder_meta();
                let files = if state.updating_removal_sizes() {
                    "Updating sizes…".to_owned()
                } else {
                    format!(
                        "{} {}{}",
                        theme::format_count(meta.file_count),
                        if meta.file_count == 1 { "file" } else { "files" },
                        if meta.state == ScanState::Complete { "" } else { " found" }
                    )
                };
                (crumbs(folder), files)
            }
            None => (Vec::new(), String::new()),
        };
        let current = state.current_path().map(<[u8]>::to_vec);
        if current != self.rendered_folder {
            self.rendered_folder = current;
            if !crumbs.is_empty() {
                self.scroll.scroll_to_item(crumbs.len() * 2 - 2);
            }
        }
        let last = crumbs.len().saturating_sub(1);
        let mut trail = div()
            .id("crumbs")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.))
            .overflow_x_scroll()
            .track_scroll(&self.scroll)
            .text_size(theme::TEXT_BODY);
        for (i, (path, name)) in crumbs.into_iter().enumerate() {
            if i > 0 {
                trail = trail.child(icon(Icon::ChevronRight).size(px(9.)).text_color(theme::color(theme::SLATE)));
            }
            let host = self.host.clone();
            let path: std::sync::Arc<[u8]> = path.into();
            trail = trail.child(
                div()
                    .id(("crumb", i))
                    .flex_none()
                    .whitespace_nowrap()
                    .cursor(CursorStyle::PointingHand)
                    .text_color(if i == last { theme::color(theme::TEXT) } else { theme::color(theme::SLATE) })
                    .hover(|s| s.text_color(theme::color(theme::TEXT)))
                    .child(theme::display(&name))
                    .on_click(move |_, _, cx| host.send(Event::Open { path: path.clone() }, cx)),
            );
        }
        let up = self.host.clone();
        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.))
            .px(px(28.))
            .child(
                div()
                    .id("go-up-slot")
                    .w(theme::BREADCRUMB_UP_WIDTH)
                    .flex_none()
                    .tooltip(tooltip("Go up one folder (⌘[)"))
                    .child(button(
                        "go-up",
                        "Up",
                        Some(Icon::ArrowUp),
                        ButtonStyle::Bordered,
                        can_go_up,
                        move |_, _, cx| up.send(Event::GoUp, cx),
                    )),
            )
            .child(trail)
            .child(div().id("opening").size(px(14.)).flex_none().flex().items_center().justify_center().when(
                opening,
                |el| {
                    el.tooltip(tooltip("Opening folder")).child(activity_indicator(
                        "opening-activity",
                        px(12.),
                        reduce_motion,
                        Icon::Loader,
                    ))
                },
            ))
            .child(theme::monospaced_digits(
                div()
                    .w(theme::BREADCRUMB_COUNT_WIDTH)
                    .flex_none()
                    .flex()
                    .justify_end()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_size(theme::TEXT_BODY)
                    .text_color(theme::color(theme::SLATE))
                    .child(files),
            ))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};
    use orbit_app_state::testing::Harness;
    use orbit_app_state::{Config, Regions};

    use crate::frame_stats;
    use crate::host::{AppHost, Host};
    use crate::main_window::MainWindow;
    use crate::settings::{Settings, SettingsStore};

    /// Keystrokes in the search field reach app-state as `QueryChanged`, and only the regions
    /// app-state notifies for a query (header, list, inspector) re-render: not the map, the
    /// sidebar, the footer or the tray.
    #[gpui::test]
    fn typing_a_query_rerenders_only_the_search_regions(cx: &mut TestAppContext) {
        let mut h = Harness::new(Config::new(b"/home".to_vec()));
        h.backend.add_file(b"/r/big/a.bin", 100);
        h.backend.add_file(b"/r/file1", 10);
        h.open_mapped(b"/r");
        h.settle();
        let host = cx.update(|cx| {
            let entity =
                cx.new(|cx| AppHost::with_state(h.state, h.backend, SettingsStore::in_memory(Settings::default()), cx));
            Host::from_entity(entity, cx)
        });
        let (root, cx) = cx.add_window_view({
            let host = host.clone();
            move |window, cx| MainWindow::new(host, None, window, cx)
        });
        cx.run_until_parked();
        let toolbar = cx.read(|cx| root.read(cx).panels().toolbar.clone());
        cx.update(|window, cx| toolbar.update(cx, |toolbar, cx| toolbar.focus_search(window, cx)));
        cx.run_until_parked();

        // Every panel but the header: renders never exceed what app-state notified.
        let others =
            [Regions::MAP, Regions::LIST, Regions::INSPECTOR, Regions::SIDEBAR, Regions::FOOTER, Regions::TRAY];
        let counts = |cx: &mut gpui::VisualTestContext| {
            cx.read(|cx| others.map(|r| (frame_stats::render_count(r), host.signal(r).read(cx).notifications)))
        };
        let before = counts(cx);
        cx.simulate_input("fil");
        cx.simulate_keystrokes("backspace");
        let after = counts(cx);
        cx.read(|cx| {
            assert_eq!(host.state(cx).query(), "fi");
            assert_eq!(toolbar.read(cx).search_field().read(cx).text(), "fi");
        });
        for ((region, (renders0, notified0)), (renders1, notified1)) in others.iter().zip(before).zip(after) {
            assert!(
                renders1 - renders0 <= notified1 - notified0,
                "{}: {} renders for {} notifications",
                crate::regions::name(*region),
                renders1 - renders0,
                notified1 - notified0
            );
        }
        let quiet = [Regions::SIDEBAR, Regions::FOOTER, Regions::TRAY];
        let quiet_renders =
            |counts: [(u64, u64); 6]| quiet.map(|r| counts[others.iter().position(|o| *o == r).unwrap()].0);
        assert_eq!(quiet_renders(after), quiet_renders(before), "sidebar, footer and tray did not re-render");
    }
}
