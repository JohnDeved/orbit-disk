//! PLACEHOLDER — collection tray (owner: tray/sheets panel agent).
//!
//! Bottom strip: 80 points of content inside a 20 × 12 margin, separated from the footer by a
//! fixed 12-point gap. Observes [`Regions::TRAY`]. Present: count/size text, drag labels and the
//! "Review Collection" button (`BeginReview`). Still to do: the drop target (dashed coral outline
//! while dragging, solid "Release to collect" when over it, `DragOverTray`/`DragDrop`, Escape →
//! `DragEnd`), validation spinner, removal locks.

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, px};
use orbit_app_state::{Event, Regions};

use crate::components::{ButtonStyle, button};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::theme;

pub const REGIONS: Regions = Regions::TRAY;

pub struct TrayPanel {
    host: Host,
    _subscriptions: Vec<Subscription>,
}

impl TrayPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self { host: host.clone(), _subscriptions: host.observe(REGIONS, cx) }
    }
}

impl Render for TrayPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let state = self.host.state(cx);
        let collection = state.collection();
        let count = collection.map_or(0, |c| c.len());
        let dragging = state.drag_label().map(str::to_owned);
        let ready = state.drop_target_active() && state.can_accept_drop();
        let title = if ready {
            "Release to collect".to_owned()
        } else if dragging.is_some() {
            "Drop here to collect".to_owned()
        } else if state.is_validating_collection() {
            "Checking collection…".to_owned()
        } else if count == 0 {
            "Drag files and folders here".to_owned()
        } else {
            format!("{count} {} in collection", if count == 1 { "item" } else { "items" })
        };
        let detail = dragging.unwrap_or_else(|| {
            collection
                .filter(|c| !c.is_empty())
                .map_or_else(|| "From the map or file list.".to_owned(), |c| theme::format_bytes(c.size()))
        });
        let can_review = count > 0 && !state.is_removing();
        let host = self.host.clone();
        div().size_full().pt(theme::TRAY_GAP).px(px(theme::TRAY_MARGIN.0)).pb(px(theme::TRAY_MARGIN.1)).child(
            div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(16.))
                .px(px(20.))
                .rounded(theme::RADIUS_CARD)
                .child(icon(Icon::TrayDown).size(px(28.)).text_color(theme::color(theme::CORAL)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .child(
                            div()
                                .text_size(theme::TEXT_TRAY)
                                .font_weight(theme::WEIGHT_SEMIBOLD)
                                .truncate()
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(theme::TEXT_SMALL)
                                .text_color(theme::color(theme::SLATE))
                                .truncate()
                                .child(detail),
                        ),
                )
                .child(button(
                    "tray-review",
                    "Review Collection",
                    Some(Icon::TrayFull),
                    ButtonStyle::Bordered,
                    can_review,
                    move |_, _, cx| host.send(Event::BeginReview, cx),
                )),
        )
    }
}
