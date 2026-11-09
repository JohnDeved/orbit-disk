//! PLACEHOLDER — mapping footer (owner: footer/toolbar panel agent).
//!
//! Fixed 51 points above the collection tray. Observes ONLY [`Regions::FOOTER`]: progress ticks
//! notify nothing else, so this is the only view that re-renders while mapping progresses.
//! Present: root name, found/indexed counts, the 10-point ivory/coral track, phase text and the
//! action button. Still to do: swatches and fixed count groups, striped track when the indexed
//! share is withheld, fade-out to "Indexed · updated … ago", activity indicator (static under
//! reduce motion), transitions.

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, px, relative};
use orbit_app_state::{Event, FooterAction, Regions};

use crate::components::{ButtonStyle, button};
use crate::frame_stats;
use crate::host::Host;
use crate::theme;

pub const REGIONS: Regions = Regions::FOOTER;

pub struct FooterPanel {
    host: Host,
    _subscriptions: Vec<Subscription>,
}

impl FooterPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self { host: host.clone(), _subscriptions: host.observe(REGIONS, cx) }
    }
}

impl Render for FooterPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let Some(footer) = self.host.state(cx).footer().cloned() else {
            return div().size_full();
        };
        let share = footer.saved_share().unwrap_or(0.0) as f32;
        let (label, event) = match footer.action {
            FooterAction::Stop => (Some("Stop Mapping"), Some(Event::Pause)),
            FooterAction::Resume => (Some("Restart Mapping"), Some(Event::Resume)),
            FooterAction::Refresh => (Some("Refresh"), Some(Event::Refresh)),
            FooterAction::Pause => (Some("Pause"), Some(Event::Pause)),
            FooterAction::None => (None, None),
        };
        let host = self.host.clone();
        let counts = format!(
            "{} found · {} indexed",
            theme::format_count(footer.discovered_files),
            footer.cached_files.map_or_else(|| "–".to_owned(), theme::format_count)
        );
        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::SPACE_M)
            .px(px(28.))
            .text_size(theme::TEXT_SMALL)
            .text_color(theme::color(theme::SLATE))
            .child(div().w(px(140.)).truncate().child(footer.root_name.clone()))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(theme::monospaced_digits(div().child(counts)))
                    .child(
                        div()
                            .h(theme::MAPPING_TRACK_HEIGHT)
                            .w_full()
                            .rounded(px(5.))
                            .bg(theme::color(theme::TRACK))
                            .child(
                                div().h_full().w(relative(1.0)).rounded(px(5.)).bg(theme::color(theme::IVORY)).child(
                                    div().h_full().w(relative(share)).rounded(px(5.)).bg(theme::color(theme::CORAL)),
                                ),
                            ),
                    ),
            )
            .child(div().w(px(120.)).child(footer.phase_text()))
            .child(div().w(px(130.)).flex().justify_end().children(label.zip(event).map(|(label, event)| {
                button("footer-action", label, None, ButtonStyle::Bordered, true, move |_, _, cx| {
                    host.send(event.clone(), cx)
                })
            })))
    }
}
