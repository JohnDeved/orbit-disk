//! Window-modal sheets and alerts drawn above the whole main window (Swift `CollectionView`,
//! `issuesView`, the error alert and `OutsideSheetDismissal`).
//!
//! The layer is a stack, bottom to top:
//! 1. collection review (`AppState::show_review`), 720 × 470;
//! 2. its confirmation — the Move to Trash alert or the permanent-delete sheet (640 × 430) —
//!    while this layer holds the app state's confirmation lease;
//! 3. skipped-items details (650 × 420), opened with [`show_skipped_items`];
//! 4. app alerts ([`present_alert`]: AI Integration…, Clear Cache…), one at a time in order;
//! 5. the error alert (`AppState::error`), only when nothing else is open: errors wait until
//!    open sheets close. While the review is open its errors replace the size text in a reserved
//!    header region instead.
//!
//! Input rules:
//! - A mouse-down outside the top card dismisses only that card, exactly like its Cancel / Done,
//!   and is consumed: every layer occludes what is below it.
//! - Escape is Cancel / Done of the top card. Return is its default action: Move to Trash in the
//!   trash confirmation, OK in alerts. The permanent-delete sheet has no default action.
//! - While a card is open the layer holds keyboard focus and the main window does not register
//!   its actions (Quick Look, Refresh, Go Up, ⌘F, …): their menu items are disabled and nothing
//!   behind the sheet reacts.
//! - Actions carry the review token as rendered, so the app state rejects them when the
//!   collection changed after the user saw it. Removal locks every collection edit.

mod alert;
mod review;
mod skipped;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::sync::Arc;

use gpui::{
    AnyElement, AnyWindowHandle, App, ClickEvent, Context, CursorStyle, Div, ElementId, Entity, FocusHandle, Global,
    InteractiveElement, IntoElement, KeyDownEvent, ListState, MouseButton, MouseDownEvent, ParentElement, Render,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px,
};
use orbit_app_state::{AppState, Event, Item, LeaseId, Regions, ReviewToken};

pub use alert::{Alert, AlertRole};

use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::main_window::MainWindow;
use crate::theme;

pub const REGIONS: Regions = Regions::SHEETS;
/// Key context of an open sheet.
pub const SHEET_CONTEXT: &str = "OrbitSheet";

/// One open card of the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Review,
    Confirmation(ConfirmKind),
    Skipped,
    Alert,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfirmKind {
    Trash,
    Permanent,
}

/// An open trash / permanent-delete confirmation: what the user is confirming, captured when
/// it opened (the lease holds collection publication meanwhile).
struct Confirmation {
    kind: ConfirmKind,
    review: ReviewToken,
    lease: LeaseId,
    items: Arc<[Item]>,
    size: u64,
    /// Full-path list of the permanent-delete sheet.
    list: ListState,
}

pub struct SheetsLayer {
    host: Host,
    focus: FocusHandle,
    /// Focus to give back when the last card closes.
    restore_focus: Option<FocusHandle>,
    /// The review token as last rendered (Swift `actionToken ?? reviewOwner`): Done, Escape and
    /// Dismiss carry it.
    review: Option<ReviewToken>,
    confirmation: Option<Confirmation>,
    skipped: bool,
    alerts: VecDeque<Alert>,
    _subscriptions: Vec<Subscription>,
}

impl SheetsLayer {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self {
            host: host.clone(),
            focus: cx.focus_handle(),
            restore_focus: None,
            review: None,
            confirmation: None,
            skipped: false,
            alerts: VecDeque::new(),
            _subscriptions: host.observe(REGIONS, cx),
        }
    }

    /// Whether any card is open (the window's shortcuts are inactive then).
    pub fn is_open(&self, cx: &App) -> bool {
        !self.layers(self.host.state(cx)).is_empty()
    }

    /// Opens the skipped-items details.
    pub fn open_skipped_items(&mut self, cx: &mut Context<Self>) {
        if !self.skipped {
            self.skipped = true;
            cx.notify();
        }
    }

    /// Queues an alert; it shows once the alerts before it were answered.
    pub fn push_alert(&mut self, alert: Alert, cx: &mut Context<Self>) {
        self.alerts.push_back(alert);
        cx.notify();
    }

    fn layers(&self, state: &AppState) -> Vec<Layer> {
        let mut layers = Vec::new();
        if state.show_review() {
            layers.push(Layer::Review);
            if let Some(confirmation) = &self.confirmation
                && state.confirmation_lease() == Some(confirmation.lease)
            {
                layers.push(Layer::Confirmation(confirmation.kind));
            }
        }
        if self.skipped {
            layers.push(Layer::Skipped);
        }
        if !self.alerts.is_empty() {
            layers.push(Layer::Alert);
        }
        if layers.is_empty() && state.error().is_some() {
            layers.push(Layer::Error);
        }
        layers
    }

    fn top(&self, cx: &App) -> Option<Layer> {
        self.layers(self.host.state(cx)).last().copied()
    }

    fn send(&self, event: Event, cx: &mut App) {
        send(&self.host, event, cx);
    }

    // -----------------------------------------------------------------------------------------
    // Cancel / default actions.

    /// Cancel / Done of `layer` (Escape, outside click, the Cancel button).
    fn cancel(&mut self, layer: Layer, cx: &mut Context<Self>) {
        match layer {
            Layer::Review => {
                if let Some(review) = self.review.clone() {
                    self.send(Event::DismissReview { review }, cx);
                }
            }
            Layer::Confirmation(_) => {
                if let Some(confirmation) = self.confirmation.take() {
                    self.send(Event::EndConfirmation { lease: confirmation.lease }, cx);
                    cx.notify();
                }
            }
            Layer::Skipped => {
                self.skipped = false;
                cx.notify();
            }
            Layer::Alert => self.answer_alert(None, cx),
            Layer::Error => self.send(Event::DismissError, cx),
        }
    }

    /// Return: the top card's default action, if it has one.
    fn default_action(&mut self, layer: Layer, cx: &mut Context<Self>) {
        match layer {
            Layer::Confirmation(ConfirmKind::Trash) => self.confirm(ConfirmKind::Trash, cx),
            // Never a keyboard default: permanent deletion needs a click on its button.
            Layer::Confirmation(ConfirmKind::Permanent) => {}
            Layer::Alert => {
                if let Some(index) = self.alerts.front().and_then(Alert::default_index) {
                    self.answer_alert(Some(index), cx);
                }
            }
            Layer::Error => self.send(Event::DismissError, cx),
            Layer::Review | Layer::Skipped => {}
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        let Some(top) = self.top(cx) else { return };
        match keystroke.key.as_str() {
            "escape" => self.cancel(top, cx),
            "enter" => self.default_action(top, cx),
            // Focus stays inside the sheet.
            "tab" => {}
            _ => return,
        }
        cx.stop_propagation();
    }

    fn outside_mouse_down(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        if let Some(top) = self.top(cx) {
            self.cancel(top, cx);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Confirmations.

    /// "Move to Trash…" / "Delete Permanently…" in the review: takes the confirmation lease and
    /// captures the reviewed items.
    fn begin_confirmation(&mut self, kind: ConfirmKind, review: ReviewToken, cx: &mut Context<Self>) {
        let state = self.host.state(cx);
        if self.confirmation.is_some() || state.confirmation_lease().is_some() || !confirmation_allowed(state, &review)
        {
            return;
        }
        let Some(collection) = state.collection().filter(|c| !c.is_empty()) else { return };
        let (items, size) = (Arc::clone(collection.items()), collection.size());
        self.send(Event::BeginConfirmation { review: review.clone() }, cx);
        let Some(lease) = self.host.state(cx).confirmation_lease() else { return };
        let list = ListState::new(items.len(), gpui::ListAlignment::Top, px(200.));
        self.confirmation = Some(Confirmation { kind, review, lease, items, size, list });
        cx.notify();
    }

    /// The confirmation's destructive button (or Return for Trash).
    fn confirm(&mut self, kind: ConfirmKind, cx: &mut Context<Self>) {
        let Some(confirmation) = self.confirmation.as_ref().filter(|c| c.kind == kind) else { return };
        if !confirmation_allowed(self.host.state(cx), &confirmation.review) {
            return;
        }
        let Some(confirmation) = self.confirmation.take() else { return };
        let review = confirmation.review;
        self.send(
            match kind {
                ConfirmKind::Trash => Event::ConfirmTrash { review },
                ConfirmKind::Permanent => Event::ConfirmPermanentDelete { review },
            },
            cx,
        );
        self.send(Event::EndConfirmation { lease: confirmation.lease }, cx);
        cx.notify();
    }

    // -----------------------------------------------------------------------------------------
    // Alerts.

    /// Answers the front alert with `choice` (`None`: its cancel button). The callback runs
    /// after this update, so it may present the next alert.
    fn answer_alert(&mut self, choice: Option<usize>, cx: &mut Context<Self>) {
        let Some(front) = self.alerts.front() else { return };
        let Some(index) = choice.or_else(|| front.cancel_index()) else { return };
        let Some(alert) = self.alerts.pop_front() else { return };
        cx.notify();
        if let Some(on_choice) = alert.on_choice {
            cx.defer(move |cx| on_choice(index, cx));
        }
    }

    // -----------------------------------------------------------------------------------------
    // Rendering.

    /// Brings the app state's view of the review and its confirmation into the local state.
    fn reconcile(&mut self, cx: &App) {
        let state = self.host.state(cx);
        if !state.show_review() {
            self.review = None;
        } else if let Some(token) = state.review_token() {
            self.review = Some(token);
        }
        // The app state ends the lease itself when the review closes or the map changes.
        if self.confirmation.as_ref().is_some_and(|c| state.confirmation_lease() != Some(c.lease)) {
            self.confirmation = None;
        }
    }

    fn render_layer(&mut self, layer: Layer, cx: &mut Context<Self>) -> AnyElement {
        match layer {
            Layer::Review => self.render_review(cx),
            Layer::Confirmation(ConfirmKind::Trash) => self.render_trash_confirmation(cx),
            Layer::Confirmation(ConfirmKind::Permanent) => self.render_permanent_confirmation(cx),
            Layer::Skipped => self.render_skipped(cx),
            Layer::Alert => self.render_app_alert(cx),
            Layer::Error => self.render_error_alert(cx),
        }
    }
}

impl Render for SheetsLayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        self.reconcile(cx);
        let layers = self.layers(self.host.state(cx));
        if layers.is_empty() {
            if self.focus.is_focused(window) || self.restore_focus.is_some() {
                match self.restore_focus.take() {
                    Some(previous) => window.focus(&previous),
                    None => window.blur(),
                }
            }
            return div().into_any_element();
        }
        if !self.focus.is_focused(window) {
            if self.restore_focus.is_none() {
                self.restore_focus = window.focused(cx);
            }
            window.focus(&self.focus);
        }
        let count = layers.len();
        let mut children = Vec::with_capacity(count);
        for (index, layer) in layers.into_iter().enumerate() {
            let card = self.render_layer(layer, cx);
            let top = index + 1 == count;
            children.push(
                div()
                    .id(("sheet-layer", index))
                    .debug_selector(move || format!("sheet-layer-{index}"))
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(index == 0, |el| el.bg(theme::with_alpha(theme::INK, theme::SHEET_BACKDROP_ALPHA)))
                    // Nothing below the backdrop sees the pointer: no hover, cursor, click or scroll.
                    .occlude()
                    .when(top, |el| el.on_mouse_down(MouseButton::Left, cx.listener(Self::outside_mouse_down)))
                    .child(card)
                    .into_any_element(),
            );
        }
        div()
            .id("sheets")
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .key_context(SHEET_CONTEXT)
            .on_key_down(cx.listener(Self::key_down))
            .children(children)
            .into_any_element()
    }
}

/// The confirmation may proceed: same review and collection, nothing removing or validating.
fn confirmation_allowed(state: &AppState, review: &ReviewToken) -> bool {
    state.review_matches(review) && !state.is_removing() && !state.is_validating_collection()
}

/// Sends an event from a sheet (tests record what the sheets sent).
fn send(host: &Host, event: Event, cx: &mut App) {
    #[cfg(test)]
    tests::record(&event);
    host.send(event, cx);
}

fn noun(count: usize) -> &'static str {
    if count == 1 { "item" } else { "items" }
}

// ---------------------------------------------------------------------------------------------
// Shared card pieces.

/// Visual weight of a [`sheet_button`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SheetButton {
    Bordered,
    /// Coral fill: the prominent (Trash) action and alert defaults.
    Prominent,
    /// Bordered with red text: permanent deletion.
    Destructive,
}

/// A large sheet button. Not focusable: Return and Escape are handled by the layer, so a
/// destructive button can never become a keyboard default. Disabled buttons ignore clicks.
pub(crate) fn sheet_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    leading: Option<Icon>,
    style: SheetButton,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let (bg, hover_bg, fg) = match style {
        SheetButton::Bordered => (theme::white(0.08), theme::white(0.14), theme::color(theme::TEXT)),
        SheetButton::Destructive => (theme::white(0.08), theme::white(0.14), theme::color(theme::DESTRUCTIVE)),
        SheetButton::Prominent => {
            (theme::color(theme::CORAL), theme::with_alpha(theme::CORAL, 0.85), theme::color(theme::CANVAS))
        }
    };
    let fg = if enabled { fg } else { theme::with_alpha(theme::SLATE, 0.6) };
    let bg = if enabled || style != SheetButton::Prominent { bg } else { theme::white(0.10) };
    let selector = id.to_string();
    div()
        .id(ElementId::Name(id))
        .debug_selector(move || selector)
        .flex()
        .flex_none()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h(theme::SHEET_BUTTON_HEIGHT)
        .px(px(14.))
        .rounded(px(6.))
        .bg(bg)
        .text_color(fg)
        .text_size(theme::TEXT_ROW)
        .whitespace_nowrap()
        .when_some(leading, |el, leading| el.child(icon(leading).text_color(fg)))
        .child(label.into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(enabled, |el| el.cursor(CursorStyle::PointingHand).hover(move |s| s.bg(hover_bg)).on_click(on_click))
}

/// A card: occludes the backdrop, so clicks inside never count as outside clicks.
fn card(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .flex()
        .flex_col()
        .flex_none()
        .bg(theme::color(theme::CANVAS))
        .text_color(theme::color(theme::TEXT))
        .font_family(theme::FONT_FAMILY)
        .text_size(theme::TEXT_ROW)
        .rounded(theme::RADIUS_PANEL)
        .border_1()
        .border_color(theme::white(0.12))
        .shadow_lg()
        .overflow_hidden()
}

fn title(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(theme::TEXT_SHEET_TITLE)
        .font_weight(theme::WEIGHT_SEMIBOLD)
        .line_height(px(28.))
        .flex_none()
        .truncate()
        .child(text.into())
}

// ---------------------------------------------------------------------------------------------
// Opening sheets from elsewhere.

/// The sheets layer of `window`, if it is the main window.
fn layer_of(window: &Window, cx: &App) -> Option<Entity<SheetsLayer>> {
    let root = window.root::<MainWindow>().flatten()?;
    Some(root.read(cx).panels().sheets.clone())
}

/// Opens the skipped-items details in `window` (the footer's "N skipped" link). Call it from an
/// event handler, not while the main window's root view is being updated.
pub fn show_skipped_items(window: &mut Window, cx: &mut App) {
    if let Some(layer) = layer_of(window, cx) {
        layer.update(cx, |layer, cx| layer.open_skipped_items(cx));
    }
}

/// Shows `alert` as an in-window alert of `window`. Returns the alert when `window` is not a
/// main window, so the caller can fall back to a platform prompt ([`Alert::prompt`]).
pub fn present_alert(window: AnyWindowHandle, alert: Alert, cx: &mut App) -> Result<(), Alert> {
    let layer = window.update(cx, |_, window, cx| layer_of(window, cx)).ok().flatten();
    match layer {
        Some(layer) => {
            layer.update(cx, |layer, cx| layer.push_alert(alert, cx));
            Ok(())
        }
        None => Err(alert),
    }
}

/// The OS privacy pane for full-disk access (`Shell::privacy_settings_url`), set at launch.
#[derive(Clone, Copy, Debug, Default)]
pub struct PrivacySettingsUrl(pub Option<&'static str>);

impl Global for PrivacySettingsUrl {}

/// Makes the skipped-items sheet offer "Privacy Settings" (call once at launch).
pub fn set_privacy_settings_url(url: Option<&'static str>, cx: &mut App) {
    cx.set_global(PrivacySettingsUrl(url));
}
