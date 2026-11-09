//! PLACEHOLDER — inspector (owner: contents/inspector panel agent).
//!
//! Bottom of the right column, fixed 164 points: selected name and path, Finder and
//! Open/Preview buttons, and the full-width coral "Add to Collection" button. Observes
//! [`Regions::INSPECTOR`]. Actions are shown disabled before a selection so the list never
//! shrinks. Still to do: "In Collection" state, help texts, selectable path text.

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, px};
use orbit_app_state::{Event, Regions};

use crate::components::{ButtonStyle, button, hairline};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::Icon;
use crate::theme;

pub const REGIONS: Regions = Regions::INSPECTOR;

pub struct InspectorPanel {
    host: Host,
    _subscriptions: Vec<Subscription>,
}

impl InspectorPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self { host: host.clone(), _subscriptions: host.observe(REGIONS, cx) }
    }
}

impl Render for InspectorPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let state = self.host.state(cx);
        let selected = state.selected().cloned();
        let can_collect = state.can_collect_selection();
        let has_selection = selected.is_some();
        let is_dir = selected.as_ref().is_some_and(|item| item.is_dir());
        let name = selected.as_ref().map_or_else(|| "Select an item".to_owned(), |item| item.display_name());
        let path = selected.as_ref().map(|item| theme::display(&item.path)).unwrap_or_default();
        let (reveal, open, collect) = (self.host.clone(), self.host.clone(), self.host.clone());
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(theme::SPACE_L)
            .pb(theme::SPACE_L)
            .child(hairline(false))
            .child(
                div().h(px(15.)).text_size(theme::TEXT_BODY).font_weight(theme::WEIGHT_SEMIBOLD).truncate().child(name),
            )
            .child(div().h(px(26.)).text_size(theme::TEXT_CAPTION).text_color(theme::color(theme::SLATE)).child(path))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .child(div().flex_1().child(button(
                        "inspector-finder",
                        "Finder",
                        Some(Icon::Folder),
                        ButtonStyle::Bordered,
                        has_selection,
                        move |_, _, cx| reveal.send(Event::RevealSelection, cx),
                    )))
                    .child(div().flex_1().child(if is_dir {
                        button(
                            "inspector-open",
                            "Open",
                            Some(Icon::ArrowUpRight),
                            ButtonStyle::Bordered,
                            true,
                            move |_, _, cx| open.send(Event::OpenSelection, cx),
                        )
                        .into_any_element()
                    } else {
                        button(
                            "inspector-preview",
                            "Preview",
                            Some(Icon::Eye),
                            ButtonStyle::Bordered,
                            has_selection,
                            move |_, _, cx| open.send(Event::PreviewSelection, cx),
                        )
                        .into_any_element()
                    })),
            )
            .child(button(
                "inspector-collect",
                "Add to Collection",
                Some(Icon::Plus),
                ButtonStyle::Prominent,
                can_collect,
                move |_, _, cx| {
                    let path = collect.state(cx).selected().map(|item| item.path.clone());
                    if let Some(path) = path {
                        collect.send(Event::Collect { paths: vec![path] }, cx);
                    }
                },
            ))
    }
}
