//! Skipped-items details (Swift `issuesView`, 650 × 420): why parts of the map are missing,
//! the affected paths, and where to grant full-disk access.

use gpui::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled};
use gpui::{div, prelude::*, px};

use super::{Layer, PrivacySettingsUrl, SheetButton, SheetsLayer, card, sheet_button};
use crate::theme;

/// At most this many affected paths are listed; the rest are counted.
pub const MAX_LISTED_ISSUES: usize = 1_000;

/// The issue lines to list and how many skipped items they leave out.
fn listed(issues: &[String], skipped: u64) -> (&[String], u64) {
    let shown = &issues[..issues.len().min(MAX_LISTED_ISSUES)];
    (shown, skipped.saturating_sub(shown.len() as u64))
}

impl SheetsLayer {
    pub(super) fn render_skipped(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.host.state(cx);
        let snapshot = state.snapshot();
        let skipped = snapshot.map_or(0, |snapshot| snapshot.skipped_count);
        let root = state.root_path().map(theme::display);
        let detail = match (skipped, root) {
            (0, _) => "No items were skipped in the current map.".to_owned(),
            (count, Some(root)) => format!(
                "{} {} could not be read while mapping {root}. Their sizes are not included.",
                theme::format_count(count),
                if count == 1 { "item" } else { "items" }
            ),
            (count, None) => format!("{} items could not be read.", theme::format_count(count)),
        };
        let (issues, more) = snapshot.map_or((&[][..], 0), |snapshot| listed(&snapshot.issues, skipped));
        let more = (more > 0 && !issues.is_empty()).then(|| format!("and {} more", theme::format_count(more)));
        let privacy = cx.try_global::<PrivacySettingsUrl>().and_then(|url| url.0);
        let (w, h) = theme::SKIPPED_SHEET;
        card("skipped-sheet")
            .w(px(w))
            .h(px(h))
            .p(px(28.))
            .gap(px(18.))
            .child(div().flex_none().text_size(theme::TEXT_TITLE).child("Some items could not be scanned"))
            .child(div().flex_none().text_color(theme::color(theme::SLATE)).child(
                "macOS may restrict protected folders. The map includes accessible items only. You can grant Orbit \
                 Disk Full Disk Access in System Settings and rescan.",
            ))
            .child(
                div()
                    .id("skipped-detail")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(detail)
                    .children(issues.iter().enumerate().map(|(index, issue)| {
                        div()
                            .id(("skipped-issue", index))
                            .text_size(theme::TEXT_SMALL)
                            .text_color(theme::color(theme::SLATE))
                            .child(issue.clone())
                    }))
                    .children(more.map(|more| {
                        div()
                            .id("skipped-more")
                            .debug_selector(|| "skipped-more".into())
                            .text_size(theme::TEXT_SMALL)
                            .text_color(theme::color(theme::SLATE))
                            .child(more)
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_none()
                    .when_some(privacy, |el, url| {
                        el.child(sheet_button(
                            "skipped-privacy",
                            "Privacy Settings",
                            None,
                            SheetButton::Bordered,
                            true,
                            move |_, _, cx| cx.open_url(url),
                        ))
                    })
                    .child(div().flex_1())
                    .child(sheet_button(
                        "skipped-done",
                        "Done",
                        None,
                        SheetButton::Bordered,
                        true,
                        cx.listener(|this, _, _, cx| this.cancel(Layer::Skipped, cx)),
                    )),
            )
            .into_any_element()
    }
}
