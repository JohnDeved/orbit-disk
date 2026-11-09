//! Collection tray (Swift `collectionBar`): an 80-point drop area inside a 20 × 12 margin, below
//! the fixed 12-point gap under the index status. Observes [`Regions::TRAY`].
//!
//! The drop area is a GPUI drop target for [`CollectionDrag`] (the typed payload map sectors, file
//! rows and search results drag): while the pointer crosses it the tray sends `DragOverTray`, a
//! release over it sends `DragDrop`. At rest it is unboxed; during a drag the whole area gains a
//! dashed coral outline, and over it a solid coral target with "Release to collect". The outline is
//! an overlay, so the states never move the contents.

use gpui::{
    Context, DragMoveEvent, InteractiveElement, IntoElement, ParentElement, Render, Styled, Subscription, Window, div,
    px,
};
use orbit_app_state::{DragId, Event, Regions};

use crate::components::{ButtonStyle, CollectionDrag, activity_indicator, button};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::theme;

pub const REGIONS: Regions = Regions::TRAY;

pub struct TrayPanel {
    host: Host,
    /// The drag and over-state last reported with `DragOverTray` (pointer moves repeat it).
    reported: Option<(DragId, bool)>,
    _subscriptions: Vec<Subscription>,
}

impl TrayPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        let subscriptions = host.observe(REGIONS, cx).into_iter().chain([theme::observe_motion(cx)]).collect();
        Self { host: host.clone(), reported: None, _subscriptions: subscriptions }
    }

    fn drag_moved(&mut self, event: &DragMoveEvent<CollectionDrag>, _: &mut Window, cx: &mut Context<Self>) {
        let drag = event.drag(cx).drag;
        let over = event.bounds.contains(&event.event.position);
        let was_over = self.reported.is_some_and(|(id, over)| id == drag && over);
        if self.reported == Some((drag, over)) {
            return;
        }
        self.reported = Some((drag, over));
        if over || was_over {
            self.host.send(Event::DragOverTray { drag, over }, cx);
        }
    }

    fn dropped(&mut self, payload: &CollectionDrag, _: &mut Window, cx: &mut Context<Self>) {
        self.reported = None;
        self.host.send(Event::DragDrop { drag: payload.drag }, cx);
    }
}

impl Render for TrayPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let reduce_motion = theme::reduce_motion(cx);
        let state = self.host.state(cx);
        let collection = state.collection();
        let count = collection.map_or(0, |c| c.len());
        let dragging = state.drag_label().map(str::to_owned);
        let is_dragging = dragging.is_some();
        let ready = state.drop_target_active();
        let validating = state.is_validating_collection();
        let updating = state.is_updating_collection();
        let title = if ready {
            "Release to collect".to_owned()
        } else if is_dragging {
            "Drop here to collect".to_owned()
        } else if validating {
            "Checking collection…".to_owned()
        } else if updating {
            "Removing from collection…".to_owned()
        } else if count == 0 {
            "Drag files and folders here".to_owned()
        } else {
            format!("{count} {} in collection", if count == 1 { "item" } else { "items" })
        };
        let detail = match (dragging, collection.filter(|c| !c.is_empty())) {
            (Some(label), _) => label,
            (None, None) => "From the map or file list.".to_owned(),
            (None, Some(_)) if state.updating_removal_sizes() => "Updating sizes…".to_owned(),
            (None, Some(collection)) => {
                format!("{}{}", theme::format_bytes(collection.size()), if state.is_partial() { " so far" } else { "" })
            }
        };
        let can_review = count > 0 && !state.is_removing() && !updating && !is_dragging;
        let glyph = if !is_dragging && (validating || updating) {
            activity_indicator("tray-activity", px(18.), reduce_motion, Icon::Loader)
        } else {
            icon(Icon::TrayDown).size(px(28.)).text_color(theme::color(theme::CORAL)).into_any_element()
        };
        let host = self.host.clone();
        let outline = div().absolute().top_0().left_0().size_full().rounded(theme::RADIUS_CARD);
        let outline = if ready {
            Some(outline.border_2().border_color(theme::color(theme::CORAL)))
        } else if is_dragging {
            Some(outline.border_1().border_dashed().border_color(theme::color(theme::CORAL)))
        } else {
            None
        };
        let fill = if ready {
            theme::with_alpha(theme::CORAL, 0.16)
        } else if is_dragging {
            theme::with_alpha(theme::CORAL, 0.06)
        } else {
            theme::with_alpha(theme::CORAL, 0.0)
        };
        div().size_full().pt(theme::TRAY_GAP).px(px(theme::TRAY_MARGIN.0)).pb(px(theme::TRAY_MARGIN.1)).child(
            div()
                .id("tray-drop-area")
                .debug_selector(|| "tray-drop-area".into())
                .relative()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(16.))
                .px(px(20.))
                .rounded(theme::RADIUS_CARD)
                .bg(fill)
                .on_drag_move(cx.listener(Self::drag_moved))
                .on_drop(cx.listener(Self::dropped))
                .child(div().size(px(36.)).flex_none().flex().items_center().justify_center().child(glyph))
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
                        .child(theme::monospaced_digits(
                            div()
                                .text_size(theme::TEXT_SMALL)
                                .text_color(theme::color(theme::SLATE))
                                .truncate()
                                .child(detail),
                        )),
                )
                .child(div().flex_none().child(button(
                    "tray-review",
                    "Review Collection",
                    Some(Icon::TrayFull),
                    ButtonStyle::Bordered,
                    can_review,
                    move |_, _, cx| host.send(Event::BeginReview, cx),
                )))
                .children(outline),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{
        AppContext, Context, Entity, InteractiveElement, IntoElement, Modifiers, MouseButton, ParentElement, Render,
        StatefulInteractiveElement, Styled, TestAppContext, Window, div, point, px,
    };
    use orbit_app_state::testing::Harness;
    use orbit_app_state::{Config, DragId, DragSource, Event};

    use super::TrayPanel;
    use crate::components::{CollectionDrag, DragPreview};
    use crate::host::{AppHost, Host};
    use crate::icons::Icon;
    use crate::settings::{Settings, SettingsStore};

    /// A drag source above the tray, standing in for a map sector or file row.
    struct DragBench {
        payload: CollectionDrag,
        tray: Entity<TrayPanel>,
    }

    impl Render for DragBench {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id("drag-source")
                        .debug_selector(|| "drag-source".into())
                        .h(px(120.))
                        .w_full()
                        .on_drag(self.payload.clone(), |payload, offset, _, cx| {
                            cx.new(|_| DragPreview::new(payload, offset))
                        }),
                )
                .child(div().h(px(104.)).w_full().child(self.tray.clone()))
        }
    }

    #[gpui::test]
    fn dropping_a_typed_payload_on_the_tray_collects_it(cx: &mut TestAppContext) {
        let mut h = Harness::new(Config::new(b"/home".to_vec()));
        h.backend.add_file(b"/r/big/a.bin", 100);
        h.backend.add_file(b"/r/file1", 10);
        h.open_mapped(b"/r");
        h.settle();
        let host = cx.update(|cx| {
            let entity =
                cx.new(|cx| AppHost::with_state(h.state, h.backend, SettingsStore::in_memory(Settings::default()), cx));
            Host::from_entity(entity, cx)
        });
        let drag = DragId(7);
        let (_bench, cx) = cx.add_window_view({
            let host = host.clone();
            move |_, cx| DragBench {
                payload: CollectionDrag { drag, label: "file1".into(), detail: "10 bytes".into(), icon: Icon::File },
                tray: cx.new(|cx| TrayPanel::new(&host, cx)),
            }
        });
        cx.run_until_parked();
        let path = Harness::path("/r/file1");
        cx.update(|_, cx| {
            host.send(
                Event::DragBegin { drag, source: DragSource::Items(vec![Arc::clone(&path)]), label: "file1".into() },
                cx,
            )
        });
        let source = cx.debug_bounds("drag-source").expect("source bounds").center();
        let tray = cx.debug_bounds("tray-drop-area").expect("tray bounds").center();
        let over = |cx: &mut gpui::VisualTestContext| cx.read(|cx| host.state(cx).drop_target_active());

        cx.simulate_mouse_down(source, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(source + point(px(0.), px(12.)), MouseButton::Left, Modifiers::none());
        assert!(!over(cx), "dragging, not yet over the tray");
        cx.simulate_mouse_move(tray, MouseButton::Left, Modifiers::none());
        assert!(over(cx), "over the tray: Release to collect");
        cx.simulate_mouse_move(source, MouseButton::Left, Modifiers::none());
        assert!(!over(cx), "left the tray again");
        cx.simulate_mouse_move(tray + point(px(5.), px(0.)), MouseButton::Left, Modifiers::none());
        assert!(over(cx));

        cx.simulate_mouse_up(tray, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        cx.read(|cx| {
            let state = host.state(cx);
            assert_eq!(state.drag_id(), None, "the drop consumed the drag");
            assert!(state.collection().is_some_and(|c| c.contains(&path)), "the dropped file was collected");
        });
    }
}
