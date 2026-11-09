//! Inspector: the bottom of the right column, a fixed 164 points (`ContentView.inspector`).
//! Observes [`Regions::INSPECTOR`].
//!
//! The selected item's name and path, the Finder and Open (folders) / Preview (files) buttons,
//! and the full-width coral "Add to Collection" button ("In Collection" once included). Every
//! action is shown — disabled — before a selection, so selecting a file never resizes the list.
//! Preview opens with the default application in v1 (no Quick Look panel).
//!
//! The path is selectable as a whole: clicking it selects it, ⌘C copies it.

use std::sync::Arc;

use gpui::{
    ClipboardItem, Context, CursorStyle, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::*, px,
};
use orbit_app_state::{AppState, Event, Regions};
use orbit_sunburst::CollectionState;

use crate::actions::Copy;
use crate::components::{ButtonStyle, button, hairline, tooltip};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::Icon;
use crate::theme;

pub const REGIONS: Regions = Regions::INSPECTOR;

/// What the inspector shows for the current state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectorModel {
    pub name: SharedString,
    pub path: Option<Arc<[u8]>>,
    /// Finder and Open/Preview are enabled.
    pub has_selection: bool,
    /// The second button opens a folder ("Open") instead of previewing a file ("Preview").
    pub opens_folder: bool,
    /// The selection is collected: the main button reads "In Collection".
    pub collected: bool,
    pub can_collect: bool,
}

impl InspectorModel {
    pub fn of(state: &AppState) -> Self {
        let selected = state.selected();
        Self {
            name: selected.map_or_else(|| "Select an item".into(), |item| item.display_name().into()),
            path: selected.map(|item| Arc::clone(&item.path)),
            has_selection: selected.is_some(),
            opens_folder: selected.is_some_and(|item| item.is_dir()),
            collected: selected.is_some_and(|item| state.collection_state(&item.path) == CollectionState::Included),
            can_collect: state.can_collect_selection(),
        }
    }
}

pub struct InspectorPanel {
    host: Host,
    focus: FocusHandle,
    /// The path text is selected (for ⌘C) — for this path only.
    path_selected: Option<Arc<[u8]>>,
    _subscriptions: Vec<Subscription>,
}

impl InspectorPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self {
            host: host.clone(),
            focus: cx.focus_handle(),
            path_selected: None,
            _subscriptions: host.observe(REGIONS, cx),
        }
    }

    fn select_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.path_selected = self.host.state(cx).selected().map(|item| Arc::clone(&item.path));
        window.focus(&self.focus);
        cx.notify();
    }

    /// Copies the selected path; `false` when it is not selected.
    fn copy_path(&self, cx: &mut Context<Self>) -> bool {
        let Some(path) = &self.path_selected else { return false };
        cx.write_to_clipboard(ClipboardItem::new_string(theme::display(path)));
        true
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "c" && keystroke.modifiers.platform && self.copy_path(cx) {
            cx.stop_propagation();
        }
    }
}

/// A button with a help tag; fills its slot's width.
fn helped(id: &'static str, text: &'static str, button: impl IntoElement) -> impl IntoElement {
    div().id(id).flex_1().flex().flex_col().tooltip(tooltip(text)).child(button)
}

impl Render for InspectorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let model = InspectorModel::of(self.host.state(cx));
        if self.path_selected.is_some() && self.path_selected != model.path {
            self.path_selected = None;
        }
        let path_selected = self.path_selected.is_some() && self.focus.is_focused(window);
        let path: SharedString = model.path.as_deref().map(theme::display).unwrap_or_default().into();
        let (reveal, open, collect) = (self.host.clone(), self.host.clone(), self.host.clone());
        let second = if model.opens_folder {
            helped(
                "inspector-open-help",
                "Open selected folder (Return)",
                button(
                    "inspector-open",
                    "Open",
                    Some(Icon::ArrowUpRight),
                    ButtonStyle::Bordered,
                    true,
                    move |_, _, cx| open.send(Event::OpenSelection, cx),
                ),
            )
            .into_any_element()
        } else {
            helped(
                "inspector-preview-help",
                "Quick Look (Space)",
                button(
                    "inspector-preview",
                    "Preview",
                    Some(Icon::Eye),
                    ButtonStyle::Bordered,
                    model.has_selection,
                    move |_, _, cx| open.send(Event::PreviewSelection, cx),
                ),
            )
            .into_any_element()
        };
        let (collect_label, collect_icon) =
            if model.collected { ("In Collection", Icon::Check) } else { ("Add to Collection", Icon::Plus) };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(theme::SPACE_L)
            .pb(theme::SPACE_L)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .when(self.path_selected.is_some(), |el| {
                el.on_action(cx.listener(|this, _: &Copy, _, cx| {
                    this.copy_path(cx);
                }))
            })
            .child(hairline(false))
            .child(
                div()
                    .h(px(15.))
                    .flex_none()
                    .text_size(theme::TEXT_BODY)
                    .font_weight(theme::WEIGHT_SEMIBOLD)
                    .truncate()
                    .child(model.name),
            )
            .child(
                div()
                    .id("inspector-path")
                    .h(px(26.))
                    .flex_none()
                    .text_size(theme::TEXT_CAPTION)
                    .text_color(theme::color(theme::SLATE))
                    .line_clamp(2)
                    .cursor(CursorStyle::IBeam)
                    .when(path_selected, |el| {
                        el.child(div().bg(theme::with_alpha(theme::CORAL, 0.35)).child(path.clone()))
                    })
                    .when(!path_selected, |el| el.child(path))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, window, cx| this.select_path(window, cx)),
                    )
                    .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        if this.path_selected.take().is_some() {
                            cx.notify();
                        }
                    })),
            )
            .child(
                div()
                    .h(px(28.))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .child(helped(
                        "inspector-finder-help",
                        "Reveal in Finder (⇧⌘R)",
                        button(
                            "inspector-finder",
                            "Finder",
                            Some(Icon::Folder),
                            ButtonStyle::Bordered,
                            model.has_selection,
                            move |_, _, cx| reveal.send(Event::RevealSelection, cx),
                        ),
                    ))
                    .child(second),
            )
            .child(div().flex_none().flex().flex_row().child(helped(
                "inspector-collect-help",
                "Add selected item (⌘K)",
                button(
                    "inspector-collect",
                    collect_label,
                    Some(collect_icon),
                    ButtonStyle::Prominent,
                    model.can_collect,
                    move |_, _, cx| {
                        let path = collect.state(cx).selected().map(|item| item.path.clone());
                        if let Some(path) = path {
                            collect.send(Event::Collect { paths: vec![path] }, cx);
                        }
                    },
                ),
            )))
    }
}
