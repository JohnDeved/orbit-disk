//! Alerts: app dialogs ([`Alert`]), the Move to Trash confirmation and the error alert.

use gpui::{
    AnyElement, AnyWindowHandle, App, Context, Div, IntoElement, ParentElement, PromptLevel, SharedString, Styled,
};
use gpui::{InteractiveElement, StatefulInteractiveElement, div, prelude::*, px};
use orbit_app_state::Event;
use orbit_app_state::model::ScanState;

use super::{ConfirmKind, SheetButton, SheetsLayer, card, confirmation_allowed, noun, sheet_button};
use crate::theme;

/// What a button of an [`Alert`] does on the keyboard and outside clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertRole {
    /// Return. Drawn prominent.
    Default,
    /// Escape and outside clicks.
    Cancel,
    Normal,
}

/// Runs with the chosen button's index once the alert closed.
pub(super) type OnChoice = Box<dyn FnOnce(usize, &mut App)>;

/// An alert with a title, a message and buttons (an `NSAlert`): the first button is the
/// rightmost (or topmost when there are more than two).
pub struct Alert {
    pub title: SharedString,
    pub message: SharedString,
    pub buttons: Vec<(SharedString, AlertRole)>,
    pub(super) on_choice: Option<OnChoice>,
}

impl std::fmt::Debug for Alert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Alert").field("title", &self.title).field("buttons", &self.buttons).finish_non_exhaustive()
    }
}

impl Alert {
    pub fn new(title: impl Into<SharedString>, message: impl Into<SharedString>) -> Self {
        Self { title: title.into(), message: message.into(), buttons: Vec::new(), on_choice: None }
    }

    /// A single "OK" button (Return and Escape).
    pub fn ok(title: impl Into<SharedString>, message: impl Into<SharedString>) -> Self {
        Self::new(title, message).button("OK", AlertRole::Default)
    }

    pub fn button(mut self, label: impl Into<SharedString>, role: AlertRole) -> Self {
        self.buttons.push((label.into(), role));
        self
    }

    /// Called with the index of the chosen button, after the alert closed.
    pub fn on_choice(mut self, on_choice: impl FnOnce(usize, &mut App) + 'static) -> Self {
        self.on_choice = Some(Box::new(on_choice));
        self
    }

    pub(super) fn default_index(&self) -> Option<usize> {
        self.buttons.iter().position(|(_, role)| *role == AlertRole::Default)
    }

    /// Escape / outside click: the Cancel button, or the only button.
    pub(super) fn cancel_index(&self) -> Option<usize> {
        self.buttons
            .iter()
            .position(|(_, role)| *role == AlertRole::Cancel)
            .or_else(|| (self.buttons.len() == 1).then_some(0))
    }

    /// Fallback for windows without a sheets layer: a platform prompt on `window`.
    pub fn prompt(self, window: AnyWindowHandle, level: PromptLevel, cx: &mut App) {
        let labels: Vec<SharedString> = self.buttons.iter().map(|(label, _)| label.clone()).collect();
        let answer = window
            .update(cx, |_, window, cx| {
                let labels: Vec<&str> = labels.iter().map(|label| label.as_ref()).collect();
                window.prompt(level, &self.title, Some(&self.message), &labels, cx)
            })
            .ok();
        let (Some(answer), Some(on_choice)) = (answer, self.on_choice) else { return };
        cx.spawn(async move |cx| {
            if let Ok(choice) = answer.await {
                cx.update(|cx| on_choice(choice, cx)).ok();
            }
        })
        .detach();
    }
}

/// An alert card: title, message (scrolls when long) and the buttons.
fn alert_card(id: &'static str, title: SharedString, message: SharedString, buttons: Vec<AnyElement>) -> AnyElement {
    let vertical = buttons.len() > 2;
    let row: Div = if vertical {
        div().flex().flex_col().gap(px(8.)).children(buttons)
    } else {
        // NSAlert order: the first button is rightmost.
        div().flex().flex_row().justify_end().gap(px(10.)).children(buttons.into_iter().rev())
    };
    card(id)
        .w(theme::ALERT_WIDTH)
        .p(px(20.))
        .gap(px(10.))
        .child(div().text_size(theme::TEXT_ROW).font_weight(theme::WEIGHT_SEMIBOLD).child(title))
        .child(
            div()
                .id("alert-message")
                .max_h(px(260.))
                .overflow_y_scroll()
                .text_size(theme::TEXT_BODY)
                .text_color(theme::color(theme::SLATE))
                .child(message),
        )
        .child(row.mt(px(8.)))
        .into_any_element()
}

impl SheetsLayer {
    pub(super) fn render_app_alert(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(alert) = self.alerts.front() else { return div().into_any_element() };
        let vertical = alert.buttons.len() > 2;
        let buttons = alert
            .buttons
            .iter()
            .enumerate()
            .map(|(index, (label, role))| {
                let style = if *role == AlertRole::Default { SheetButton::Prominent } else { SheetButton::Bordered };
                sheet_button(
                    format!("alert-button-{index}"),
                    label.clone(),
                    None,
                    style,
                    true,
                    cx.listener(move |this, _, _, cx| this.answer_alert(Some(index), cx)),
                )
                .when(vertical, |el| el.w_full())
                .into_any_element()
            })
            .collect();
        alert_card("app-alert", alert.title.clone(), alert.message.clone(), buttons)
    }

    /// The error alert (Swift: "Orbit Disk" alert with OK), shown when no sheet is open.
    pub(super) fn render_error_alert(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let message = self.host.state(cx).error().unwrap_or_default().to_owned();
        let ok = sheet_button(
            "error-ok",
            "OK",
            None,
            SheetButton::Prominent,
            true,
            cx.listener(|this, _, _, cx| this.send(Event::DismissError, cx)),
        );
        alert_card("error-alert", "Orbit Disk".into(), message.into(), vec![ok.into_any_element()])
    }

    /// "Move N items to Trash?" — Return moves them; Escape and outside clicks cancel.
    pub(super) fn render_trash_confirmation(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(confirmation) = &self.confirmation else { return div().into_any_element() };
        let state = self.host.state(cx);
        let owned = state.review_has_owner(&confirmation.review);
        let count = if owned { confirmation.items.len() } else { 0 };
        let incomplete =
            owned && confirmation.items.iter().any(|item| item.is_dir() && item.meta.state != ScanState::Complete);
        let message = if incomplete {
            "Selected folders may not be fully mapped. Moving them to Trash includes everything inside, including \
             files not shown yet. You can restore them from Trash in Finder."
        } else {
            "You can restore these items from Trash in Finder. Orbit Disk never empties the Trash."
        };
        let enabled = confirmation_allowed(state, &confirmation.review);
        let cancel = sheet_button(
            "trash-cancel",
            "Cancel",
            None,
            SheetButton::Bordered,
            true,
            cx.listener(|this, _, _, cx| this.cancel(super::Layer::Confirmation(ConfirmKind::Trash), cx)),
        );
        let trash = sheet_button(
            "trash-confirm",
            "Move to Trash",
            None,
            SheetButton::Prominent,
            enabled,
            cx.listener(|this, _, _, cx| this.confirm(ConfirmKind::Trash, cx)),
        );
        alert_card(
            "trash-confirmation",
            format!("Move {count} {} to Trash?", noun(count)).into(),
            message.into(),
            vec![trash.into_any_element(), cancel.into_any_element()],
        )
    }
}
