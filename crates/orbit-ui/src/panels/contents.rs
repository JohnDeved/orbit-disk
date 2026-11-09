//! PLACEHOLDER — Contents list (owner: contents panel agent).
//!
//! The file list in the right column: header ("Contents" / "Search results", sort menu), the
//! count/hover caption block, and a virtualized list of 40-point rows. Observes
//! [`Regions::LIST`]. Present: rows from `AppState::row`, chart swatches, selected / hovered /
//! map-related row fills, row hover → `HoverRow`/`ClearRowHover`, click → `Select`, double-click
//! → `Open` (folders). Still to do: folder arrow button (32 × 36), collection marks and the
//! secondary line, sort menu, empty/search states, hover suppression while scrolling,
//! scroll-into-view of `first_related_row`, keyboard selection, row drag to the tray
//! (`GestureBegin`/`DragBegin`), context menu.

use std::sync::Arc;

use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use orbit_app_state::{Event, Regions};

use crate::frame_stats;
use crate::host::Host;
use crate::theme;

pub const REGIONS: Regions = Regions::LIST;

pub struct ContentsPanel {
    host: Host,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl ContentsPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self { host: host.clone(), scroll: UniformListScrollHandle::new(), _subscriptions: host.observe(REGIONS, cx) }
    }

    /// Scrolls the list by `dy` (negative scrolls down), like a wheel event over it.
    pub fn scroll_by(&mut self, dy: gpui::Pixels, cx: &mut Context<Self>) {
        let base = self.scroll.0.borrow().base_handle.clone();
        let offset = base.offset();
        let max = base.max_offset().height;
        let y = (offset.y + dy).clamp(-max, px(0.));
        base.set_offset(gpui::point(offset.x, y));
        cx.notify();
    }

    fn rows(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let state = self.host.state(cx);
        let selected = state.selected().map(|item| Arc::clone(&item.path));
        let row_hover = state.row_hover().map(<[u8]>::to_vec);
        let mut rows = Vec::with_capacity(range.len());
        for display in range {
            let Some(row) = state.row(display) else { continue };
            let item = row.item();
            let name: SharedString = theme::display(row.name()).into();
            let meta = *row.meta();
            let is_selected = selected.as_deref() == Some(&item.path[..]);
            let is_hovered = row_hover.as_deref() == Some(&item.path[..]);
            let related = state.is_row_related(display);
            let swatch = state.color_ordinal(row.name()).map(|ordinal| theme::chart(ordinal, 0));
            let detail = if meta.flags.is_dir {
                format!("{} files", theme::format_count(meta.file_count))
            } else {
                "File".to_owned()
            };
            let (fill, outline) = if is_selected {
                (theme::color(theme::ROW_SELECTED), Some((theme::color(theme::CORAL), px(2.))))
            } else if is_hovered || related {
                (theme::color(theme::ROW_HOVER), Some((theme::color(theme::ROW_HOVER_OUTLINE), px(1.))))
            } else {
                (theme::white(0.0), None)
            };
            let host = self.host.clone();
            let path = Arc::clone(&item.path);
            let hover_host = self.host.clone();
            let hover_path = Arc::clone(&item.path);
            rows.push(
                div()
                    .id(display)
                    .h(theme::ROW_HEIGHT)
                    .px(px(8.))
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.))
                            .px(px(10.))
                            .rounded(theme::RADIUS_ROW)
                            .bg(fill)
                            .when_some(outline, |el, (color, width)| el.border(width).border_color(color))
                            .child(
                                div()
                                    .size(px(8.))
                                    .rounded(px(2.))
                                    .flex_none()
                                    .bg(swatch.unwrap_or_else(|| theme::color(theme::NEUTRAL).into())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().text_size(theme::TEXT_ROW).truncate().child(name))
                                    .child(
                                        div()
                                            .text_size(theme::TEXT_SMALL)
                                            .text_color(theme::color(theme::SLATE))
                                            .truncate()
                                            .child(detail),
                                    ),
                            )
                            .child(theme::monospaced_digits(
                                div()
                                    .flex_none()
                                    .text_size(theme::TEXT_BODY)
                                    .text_color(theme::color(theme::SLATE))
                                    .child(theme::format_bytes(meta.size)),
                            )),
                    )
                    .on_hover(move |hovered, _, cx| {
                        if *hovered {
                            hover_host.send(Event::HoverRow { path: Arc::clone(&hover_path) }, cx);
                        }
                    })
                    .on_click(move |event, _, cx| {
                        let event = if event.click_count() >= 2 && meta.flags.is_dir {
                            Event::Open { path: Arc::clone(&path) }
                        } else {
                            Event::Select { path: Some(Arc::clone(&path)) }
                        };
                        host.send(event, cx);
                    })
                    .into_any_element(),
            );
        }
        rows
    }
}

impl Render for ContentsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let state = self.host.state(cx);
        let count = state.row_count();
        let title = if state.filtering() { "Search results" } else { "Contents" };
        let caption = if state.filtering() {
            format!("{} matches", theme::format_count(state.search_match_count()))
        } else {
            format!("{} items · largest first", theme::format_count(count as u64))
        };
        let hover = state.hover_description().map(str::to_owned);
        let host = self.host.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(theme::SPACE_L)
                    .pt(theme::SPACE_L)
                    .pb(px(10.))
                    .text_size(theme::TEXT_HEADING)
                    .font_weight(theme::WEIGHT_SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .h(px(48.))
                    .px(theme::SPACE_L)
                    .pb(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .text_size(theme::TEXT_SMALL)
                    .child(div().text_color(theme::color(theme::SLATE)).child(caption))
                    .when_some(hover, |el, hover| {
                        el.child(div().text_color(theme::color(theme::CORAL)).truncate().child(hover))
                    }),
            )
            .child(div().mx(theme::SPACE_L).h(px(1.)).bg(theme::white(0.1)))
            .child(
                div()
                    .id("contents-list")
                    .flex_1()
                    .min_h_0()
                    .on_hover(move |hovered, _, cx| {
                        if !*hovered {
                            host.send(Event::ClearRowHover, cx);
                        }
                    })
                    .child(
                        uniform_list("contents-rows", count, cx.processor(|this, range, _, cx| this.rows(range, cx)))
                            .track_scroll(self.scroll.clone())
                            .size_full(),
                    ),
            )
    }
}
