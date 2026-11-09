//! Collection review (720 × 470) and the permanent-delete confirmation (640 × 430).

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, list, prelude::*, px, uniform_list,
};
use orbit_app_state::model::ScanState;
use orbit_app_state::{Event, Item, ReviewToken};

use super::{ConfirmKind, Layer, SheetButton, SheetsLayer, card, noun, send, sheet_button, title};
use crate::components::tooltip;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::theme;

fn incomplete(item: &Item) -> bool {
    item.is_dir() && item.meta.state != ScanState::Complete
}

/// One review row: kind, name, full path, size and Remove (disabled with "Removing from
/// collection…" while the item is being removed in the background).
fn review_row(
    index: usize,
    item: &Item,
    updating: bool,
    removing: bool,
    remove: Option<(Host, ReviewToken)>,
) -> AnyElement {
    let size = if updating {
        "Updating…".to_owned()
    } else if incomplete(item) {
        format!("{} so far", theme::format_bytes(item.meta.size))
    } else {
        theme::format_bytes(item.meta.size)
    };
    let enabled = remove.is_some() && !removing;
    let path = Arc::clone(&item.path);
    div()
        .h(theme::REVIEW_ROW_HEIGHT)
        .w_full()
        .px(px(12.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .border_b_1()
        .border_color(theme::white(theme::DIVIDER_ALPHA))
        .child(icon(if item.is_dir() { Icon::Folder } else { Icon::File }).text_color(theme::color(theme::CORAL)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().truncate().font_weight(theme::WEIGHT_MEDIUM).child(item.display_name()))
                .child(
                    div()
                        .truncate()
                        .text_size(theme::TEXT_CAPTION)
                        .text_color(theme::color(theme::SLATE))
                        .child(theme::display(&item.path)),
                ),
        )
        .child(theme::monospaced_digits(
            div().w(theme::REVIEW_SIZE_WIDTH).flex_none().flex().justify_end().whitespace_nowrap().child(size),
        ))
        .child(
            sheet_button(
                format!("review-remove-{index}"),
                "Remove",
                Some(if removing { Icon::Loader } else { Icon::CircleMinus }),
                SheetButton::Bordered,
                enabled,
                move |_, _, cx| {
                    if let Some((host, review)) = &remove {
                        let event = Event::Uncollect { path: Arc::clone(&path), review: Some(review.clone()) };
                        send(host, event, cx);
                    }
                },
            )
            .tooltip(tooltip(if removing {
                "Removing from collection…"
            } else {
                "Remove from collection"
            })),
        )
        .into_any_element()
}

impl SheetsLayer {
    pub(super) fn render_review(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.host.state(cx);
        // Content actions carry the token the user is looking at; without an owner the sheet
        // shows nothing and only Done works.
        let token = state.review_token();
        let done_token = self.review.clone();
        let items: Arc<[Item]> =
            token.as_ref().and_then(|_| state.collection()).map_or_else(|| Arc::from([]), |c| Arc::clone(c.items()));
        let size = token.as_ref().and_then(|_| state.collection()).map_or(0, |c| c.size());
        let removing = state.is_removing();
        let validating = state.is_validating_collection();
        let updating = state.updating_removal_sizes();
        let editable = token.is_some() && !removing;
        let edit_pending = state.is_updating_collection();
        let removable = editable && !validating && !edit_pending && !items.is_empty();

        let error =
            done_token.as_ref().filter(|t| state.review_has_owner(t)).and_then(|_| state.error()).map(str::to_owned);
        let status = match error {
            Some(message) => div()
                .id("review-error")
                .debug_selector(|| "review-error".into())
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .child(icon(Icon::Warning).size(px(12.)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(theme::TEXT_BODY)
                                .font_weight(theme::WEIGHT_SEMIBOLD)
                                .child("Some items could not be removed"),
                        )
                        .child(
                            sheet_button(
                                "review-error-dismiss",
                                "Dismiss",
                                None,
                                SheetButton::Bordered,
                                done_token.is_some(),
                                cx.listener(|this, _, _, cx| {
                                    if let Some(review) = this.review.clone() {
                                        this.send(Event::DismissCollectionError { review }, cx);
                                    }
                                }),
                            )
                            .h(px(22.))
                            .px(px(8.))
                            .text_size(theme::TEXT_SMALL),
                        ),
                )
                .child(
                    div()
                        .id("review-error-text")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .text_size(theme::TEXT_SMALL)
                        .child(message),
                )
                .into_any_element(),
            None => div()
                .text_size(theme::TEXT_BODY)
                .text_color(theme::color(theme::SLATE))
                .line_clamp(2)
                .child(if updating {
                    "Updating sizes…".to_owned()
                } else if items.iter().any(incomplete) {
                    "Some folder sizes are incomplete.".to_owned()
                } else {
                    theme::format_bytes(size)
                })
                .into_any_element(),
        };

        let header = div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(12.))
            .flex_none()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(title("Collection").h(px(28.)))
                    // Reserved: errors replace the size text without moving the list.
                    .child(div().h(theme::REVIEW_STATUS_HEIGHT).w_full().overflow_hidden().child(status)),
            )
            .child(sheet_button(
                "review-done",
                "Done",
                None,
                SheetButton::Bordered,
                true,
                cx.listener(|this, _, _, cx| this.cancel(Layer::Review, cx)),
            ));

        let rows = {
            let items = Arc::clone(&items);
            let remove = token.clone().filter(|_| editable).map(|token| (self.host.clone(), token));
            let host = self.host.clone();
            uniform_list("review-items", items.len(), move |range, _: &mut Window, cx: &mut App| {
                let state = host.state(cx);
                range
                    .map(|index| {
                        let item = &items[index];
                        let updating = updating && state.updating_removal_size(&item.path, item.is_dir());
                        let removing = state.is_removing_from_collection(&item.path);
                        review_row(index, item, updating, removing, remove.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .size_full()
        };
        let list_panel = div()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(theme::color(theme::PANEL))
            .rounded(theme::RADIUS_CARD)
            .overflow_hidden()
            .child(rows);

        let begin = |kind: ConfirmKind| {
            let token = token.clone();
            cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                if let Some(review) = token.clone() {
                    this.begin_confirmation(kind, review, cx);
                }
            })
        };
        let clear_token = token.clone();
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .flex_none()
            .child(sheet_button(
                "review-clear",
                "Clear Collection",
                None,
                SheetButton::Bordered,
                editable && !validating && !edit_pending,
                cx.listener(move |this, _, _, cx| {
                    if let Some(review) = clear_token.clone() {
                        this.send(Event::ClearCollection { review }, cx);
                    }
                }),
            ))
            .child(div().flex_1())
            .when(removing, |el| el.child(icon(Icon::Loader).text_color(theme::color(theme::SLATE))))
            .child(sheet_button(
                "review-delete",
                "Delete Permanently…",
                None,
                SheetButton::Destructive,
                removable,
                begin(ConfirmKind::Permanent),
            ))
            .child(sheet_button(
                "review-trash",
                "Move to Trash…",
                None,
                SheetButton::Prominent,
                removable,
                begin(ConfirmKind::Trash),
            ));

        let (w, h) = theme::REVIEW_SHEET;
        card("review-sheet")
            .w(px(w))
            .h(px(h))
            .p(theme::SHEET_PADDING)
            .gap(px(18.))
            .child(header)
            .child(list_panel)
            .child(footer)
            .into_any_element()
    }

    /// "Delete N items permanently?" with every full path, the folder-content scope and the
    /// irreversible warning. Its destructive button has no keyboard shortcut.
    pub(super) fn render_permanent_confirmation(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(confirmation) = &self.confirmation else { return div().into_any_element() };
        let state = self.host.state(cx);
        let owned = state.review_has_owner(&confirmation.review);
        let items: Arc<[Item]> = if owned { Arc::clone(&confirmation.items) } else { Arc::from([]) };
        let has_incomplete = items.iter().any(incomplete);
        let enabled = super::confirmation_allowed(state, &confirmation.review);
        let sizes = if state.updating_removal_sizes() {
            "Updating sizes…".to_owned()
        } else {
            format!(
                "{} in file sizes{}",
                theme::format_bytes(if owned { confirmation.size } else { 0 }),
                if has_incomplete { " so far" } else { "" }
            )
        };
        let warning = if has_incomplete {
            "Selected folders may not be fully mapped. Deleting them includes everything inside, including files not \
             shown yet. This cannot be undone."
        } else {
            "These items and all contents of the listed folders will be deleted immediately. They will not go to \
             Trash. This cannot be undone."
        };
        if confirmation.list.item_count() != items.len() {
            confirmation.list.reset(items.len());
        }
        let paths = {
            let items = Arc::clone(&items);
            list(confirmation.list.clone(), move |index, _, _| {
                let item = &items[index];
                div()
                    .w_full()
                    .pb(px(14.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .font_weight(theme::WEIGHT_MEDIUM)
                            .child(icon(if item.is_dir() { Icon::Folder } else { Icon::File }))
                            .child(div().min_w_0().truncate().child(item.display_name())),
                    )
                    // Full path, wrapped, never truncated.
                    .child(
                        div()
                            .w_full()
                            .text_size(theme::TEXT_SMALL)
                            .text_color(theme::color(theme::SLATE))
                            .child(theme::display(&item.path)),
                    )
                    .into_any_element()
            })
            .size_full()
        };
        let count = items.len();
        let (w, h) = theme::PERMANENT_SHEET;
        card("permanent-sheet")
            .w(px(w))
            .h(px(h))
            .p(theme::SHEET_PADDING)
            .gap(px(18.))
            .child(title(format!("Delete {count} {} permanently?", noun(count))))
            .child(div().flex_none().text_size(theme::TEXT_ROW).child(warning))
            .child(
                theme::monospaced_digits(div())
                    .flex_none()
                    .text_size(theme::TEXT_BODY)
                    .text_color(theme::color(theme::SLATE))
                    .child(sizes),
            )
            .child(
                div()
                    .id("permanent-paths")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p(px(14.))
                    .bg(theme::color(theme::PANEL))
                    .rounded(px(8.))
                    .child(paths),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(10.))
                    .flex_none()
                    .child(sheet_button(
                        "permanent-cancel",
                        "Cancel",
                        None,
                        SheetButton::Bordered,
                        true,
                        cx.listener(|this, _, _, cx| this.cancel(Layer::Confirmation(ConfirmKind::Permanent), cx)),
                    ))
                    .child(sheet_button(
                        "permanent-confirm",
                        "Delete Permanently",
                        None,
                        SheetButton::Destructive,
                        enabled,
                        cx.listener(|this, _, _, cx| this.confirm(ConfirmKind::Permanent, cx)),
                    )),
            )
            .into_any_element()
    }
}
