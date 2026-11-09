//! PLACEHOLDER — sheets (owner: tray/sheets panel agent).
//!
//! Window-modal overlays drawn above everything: collection review (`show_review`), the
//! permanent-delete confirmation, skipped-items details, error alerts. Observes
//! [`Regions::SHEETS`]. When nothing is open the view renders an empty, non-interactive layer.
//!
//! Present: a review placeholder with item count and Done (`DismissReview`); a click on the
//! dimmed background dismisses the top sheet and is consumed. Still to do: the review list with
//! remove buttons, Move to Trash / Delete Permanently, the confirmation sheet
//! (`BeginConfirmation`/`EndConfirmation`, no Return default, Escape cancels), removal locks,
//! error presentation order (errors wait for an open preview or details sheet).

use gpui::{
    Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, Styled, Subscription, Window, div, px,
};
use orbit_app_state::{Event, Regions};

use crate::components::{ButtonStyle, button};
use crate::frame_stats;
use crate::host::Host;
use crate::theme;

pub const REGIONS: Regions = Regions::SHEETS;

pub struct SheetsLayer {
    host: Host,
    _subscriptions: Vec<Subscription>,
}

impl SheetsLayer {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self { host: host.clone(), _subscriptions: host.observe(REGIONS, cx) }
    }

    /// Whether a sheet is open (the root uses it to block window shortcuts).
    pub fn is_open(&self, cx: &gpui::App) -> bool {
        self.host.state(cx).show_review()
    }
}

impl Render for SheetsLayer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let state = self.host.state(cx);
        let Some(review) = state.review_token().filter(|_| state.show_review()) else {
            return div().into_any_element();
        };
        let count = state.collection().map_or(0, |c| c.len());
        let (dismiss_bg, dismiss_done) = (self.host.clone(), self.host.clone());
        div()
            .id("sheet-backdrop")
            .size_full()
            .flex()
            .justify_center()
            .bg(theme::with_alpha(theme::INK, 0.45))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                dismiss_bg.send(Event::DismissReview { review: review.clone() }, cx);
            })
            .child(
                div()
                    .id("review-sheet")
                    .mt(px(52.))
                    .w(px(560.))
                    .h(px(420.))
                    .flex()
                    .flex_col()
                    .gap(theme::SPACE_M)
                    .p(theme::SPACE_XL)
                    .rounded(theme::RADIUS_PANEL)
                    .bg(theme::color(theme::PANEL))
                    .border_1()
                    .border_color(theme::white(0.1))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(div().text_size(theme::TEXT_HEADING).font_weight(theme::WEIGHT_SEMIBOLD).child("Collection"))
                    .child(
                        div()
                            .text_color(theme::color(theme::SLATE))
                            .child(format!("{count} items · review placeholder")),
                    )
                    .child(div().flex_1())
                    .child(div().flex().justify_end().child(button(
                        "review-done",
                        "Done",
                        None,
                        ButtonStyle::Bordered,
                        true,
                        move |_, _, cx| {
                            let review = dismiss_done.state(cx).review_token();
                            if let Some(review) = review {
                                dismiss_done.send(Event::DismissReview { review }, cx);
                            }
                        },
                    ))),
            )
            .into_any_element()
    }
}
