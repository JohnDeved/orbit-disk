//! Mapping footer (Swift `MappingProgressFooter` / `MappingProgressView`): a fixed 51 points
//! between the explorer and the collection tray. Observes ONLY [`Regions::FOOTER`]: progress ticks,
//! the track transition, the completion fade and the age refresh re-render this view and nothing
//! else.
//!
//! With progress values ([`FooterState`]):
//! ```text
//! ▪ 12,345 found     ▪ 9,876 indexed                         ◌ Finding more files   (14)
//! ████████████████████████████████▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░   (10)
//! Macintosh HD   Indexed 5 min ago   12 skipped            status message   [Stop]  (20)
//! ```
//! One 10-point track for the files found so far: ivory is in memory, coral is the indexed share
//! (never past ivory), diagonal stripes when the share is withheld. The share eases over 0.35 s;
//! at completion counts and track fade out over 0.18 s, leaving "Indexed · updated … ago" (its
//! actual saved timestamp, refreshed every [`theme::AGE_REFRESH`] while visible). Reduce Motion
//! disables both transitions and shows a static activity symbol.
//!
//! Without progress values (a root but no footer state yet) the legacy two-line status is shown,
//! bottom aligned in the same height.

use std::time::{Duration, Instant, SystemTime};

use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, PathBuilder, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas, div, point, prelude::*, px, relative,
};
use orbit_app_state::{AppState, Event, FooterAction, FooterState, Phase, Regions};

use crate::components::{activity_indicator, small_button, tooltip, truncate_middle};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::theme;

pub const REGIONS: Regions = Regions::FOOTER;

/// An eased transition of one value (the coral share, the progress opacity).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Transition {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
}

impl Transition {
    fn settled(value: f32, duration: Duration, now: Instant) -> Self {
        Self { from: value, to: value, start: now, duration }
    }

    fn value(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.start).as_secs_f32();
        let t = if self.duration.is_zero() { 1.0 } else { elapsed / self.duration.as_secs_f32() };
        self.from + (self.to - self.from) * theme::ease_out(t)
    }

    fn running(&self, now: Instant) -> bool {
        self.from != self.to && now.saturating_duration_since(self.start) < self.duration
    }

    /// Moves towards `to` from the value shown now (jumps when `animate` is false).
    fn retarget(&mut self, to: f32, animate: bool, now: Instant) {
        if to == self.to {
            return;
        }
        self.from = if animate { self.value(now) } else { to };
        self.to = to;
        self.start = now;
    }
}

pub struct FooterPanel {
    host: Host,
    /// Coral share of the track (target: `saved_share().unwrap_or(0)`).
    share: Option<Transition>,
    /// Opacity of the counts and track (0 once the map is saved).
    progress_opacity: Option<Transition>,
    /// Middle-truncated root name, cached per name.
    root_label: Option<(String, SharedString)>,
    /// Re-renders "… ago" while an age is visible.
    age_timer: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl FooterPanel {
    pub fn new(host: &Host, cx: &mut Context<Self>) -> Self {
        Self {
            host: host.clone(),
            share: None,
            progress_opacity: None,
            root_label: None,
            age_timer: None,
            _subscriptions: host.observe(REGIONS, cx).into_iter().chain([theme::observe_motion(cx)]).collect(),
        }
    }

    /// A share or fade transition is in progress (it requests animation frames for this view).
    pub fn is_animating(&self) -> bool {
        let now = Instant::now();
        self.share.is_some_and(|t| t.running(now)) || self.progress_opacity.is_some_and(|t| t.running(now))
    }

    /// The coral share the track is moving to, `None` before the first progress render.
    pub fn share_target(&self) -> Option<f32> {
        self.share.map(|t| t.to)
    }

    fn root_label(&mut self, name: &str, window: &Window) -> SharedString {
        if let Some((cached, label)) = &self.root_label
            && cached == name
        {
            return label.clone();
        }
        let label =
            truncate_middle(name, px(theme::FOOTER_ROOT_WIDTH), theme::TEXT_SMALL, theme::WEIGHT_MEDIUM, window);
        self.root_label = Some((name.to_owned(), label.clone()));
        label
    }

    fn keep_age_fresh(&mut self, visible: bool, cx: &mut Context<Self>) {
        if !visible {
            self.age_timer = None;
            return;
        }
        if self.age_timer.is_none() {
            self.age_timer = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(theme::AGE_REFRESH).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
    }

    fn progress(
        &mut self,
        footer: &FooterState,
        state: &AppState,
        reduce_motion: bool,
        window: &mut Window,
    ) -> gpui::Div {
        let now = Instant::now();
        let saved = footer.phase == Phase::Saved;
        let share = footer.saved_share();
        let animate = !reduce_motion;
        let share_target = share.unwrap_or(0.0) as f32;
        let fade_target = if saved { 0.0 } else { 1.0 };
        let share_t = self.share.get_or_insert_with(|| Transition::settled(share_target, theme::TRACK_TRANSITION, now));
        share_t.retarget(share_target, animate, now);
        let share_t = *share_t;
        let fade_t =
            self.progress_opacity.get_or_insert_with(|| Transition::settled(fade_target, theme::FOOTER_FADE, now));
        fade_t.retarget(fade_target, animate, now);
        let fade_t = *fade_t;
        if animate && (share_t.running(now) || fade_t.running(now)) {
            window.request_animation_frame();
        }
        let shown_share = if animate { share_t.value(now) } else { share_target };
        let opacity = if animate { fade_t.value(now) } else { fade_target };

        let has_files = footer.discovered_files > 0 || saved;
        let mut track = div()
            .id("mapping-track")
            .relative()
            .h(theme::MAPPING_TRACK_HEIGHT)
            .w_full()
            .rounded(theme::MAPPING_TRACK_HEIGHT / 2.)
            .overflow_hidden()
            .bg(theme::color(theme::TRACK));
        if has_files {
            track = track.child(div().absolute().top_0().left_0().size_full().bg(theme::color(theme::IVORY)));
            track = match share {
                None => track.child(stripes()),
                Some(_) => track
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(relative(shown_share.clamp(0.0, 1.0)))
                            .bg(theme::color(theme::CORAL)),
                    )
                    .when(shown_share > 0.0 && shown_share < 1.0, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .left(relative(shown_share))
                                .w(px(1.))
                                .h_full()
                                .bg(theme::color(theme::CANVAS)),
                        )
                    }),
            };
        }
        if !saved {
            track = track.tooltip(tooltip(footer.track_help()));
        }

        let phase = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .text_color(theme::color(theme::SLATE))
            .child(
                div()
                    .size(px(12.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(phase_indicator(footer.phase, reduce_motion)),
            )
            .child(div().whitespace_nowrap().child(footer.phase_text()));
        let summary = theme::monospaced_digits(
            div()
                .h(px(14.))
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(16.))
                .child(count_group(
                    "found-count",
                    Some(footer.discovered_files),
                    "found",
                    theme::IVORY,
                    "Files discovered in memory and available to browse. The total grows while finding files.",
                    !saved,
                ))
                .child(count_group(
                    "indexed-count",
                    footer.cached_files,
                    "indexed",
                    theme::CORAL,
                    "Discovered files stored in the local index. Orange shows their share of the files found so far.",
                    !saved,
                ))
                .child(div().flex_1().min_w(px(8.)))
                .child(phase),
        );
        let progress = div()
            .h(theme::FOOTER_PROGRESS_HEIGHT)
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.))
            .opacity(opacity)
            .child(summary)
            .child(track);

        let root = self.root_label(&footer.root_name, window);
        let age = state.indexed_at().map(age_text);
        let status = state.status().map(str::to_owned);
        let host = self.host.clone();
        let row = div()
            .h(theme::FOOTER_ROW_HEIGHT)
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .id("footer-root")
                    .flex_none()
                    .max_w(px(theme::FOOTER_ROOT_WIDTH))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .font_weight(theme::WEIGHT_MEDIUM)
                    .child(root)
                    .tooltip(tooltip(format!("Map of {}", footer.root_name))),
            )
            .child(if saved { completed_status(age) } else { index_date(age) })
            .children(skipped(state))
            .child(status_text(status))
            .child(action_button(footer.action, host));
        div().size_full().flex().flex_col().gap(px(3.)).child(progress).child(row)
    }
}

impl Render for FooterPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let reduce_motion = theme::reduce_motion(cx);
        let host = self.host.clone();
        let state = host.state(cx);
        let content = if let Some(footer) = state.footer() {
            let content = self.progress(footer, state, reduce_motion, window);
            Some((content, state.indexed_at().is_some()))
        } else if state.root_path().is_some() || state.is_scanning() || state.is_refreshing() {
            self.share = None;
            self.progress_opacity = None;
            let shows_age = !state.is_partial() && state.snapshot().is_some() && state.indexed_at().is_some();
            Some((legacy_status(state, &self.host, reduce_motion), shows_age))
        } else {
            self.share = None;
            self.progress_opacity = None;
            None
        };
        let shows_age = content.as_ref().is_some_and(|(_, age)| *age);
        self.keep_age_fresh(shows_age, cx);
        // Counts and ages change constantly: digits share one width so nothing jitters.
        theme::monospaced_digits(
            div()
                .size_full()
                .px(px(28.))
                .text_size(theme::TEXT_SMALL)
                .text_color(theme::color(theme::SLATE))
                .children(content.map(|(content, _)| content)),
        )
    }
}

/// "5 min" since the saved timestamp (a clock moving backwards reads as "less than a minute").
fn age_text(indexed_at: SystemTime) -> String {
    theme::format_age(SystemTime::now().duration_since(indexed_at).unwrap_or_default())
}

/// Swatch, count and label in a fixed 164-point group; "Updating indexed count" when withheld.
fn count_group(
    id: &'static str,
    count: Option<u64>,
    label: &'static str,
    swatch: u32,
    help: &'static str,
    interactive: bool,
) -> impl IntoElement {
    let group = div()
        .id(id)
        .w(theme::FOOTER_COUNT_WIDTH)
        .h(px(14.))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .whitespace_nowrap()
        .overflow_hidden()
        .child(div().size(px(7.)).flex_none().rounded(px(2.)).bg(theme::color(swatch)));
    let group = match count {
        Some(count) => group
            .child(
                div()
                    .font_weight(theme::WEIGHT_MEDIUM)
                    .text_color(theme::color(theme::TEXT))
                    .child(theme::format_count(count)),
            )
            .child(div().child(label)),
        None => group.child(div().child("Updating indexed count")),
    };
    group.when(interactive, |el| el.tooltip(tooltip(help)))
}

/// The diagonal stripes of a withheld indexed share (they do not represent a percentage).
fn stripes() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |bounds, _, window, _| {
            let height = bounds.size.height;
            let mut builder = PathBuilder::stroke(px(2.));
            let mut x = -height;
            while x < bounds.size.width {
                builder.move_to(point(bounds.origin.x + x, bounds.origin.y + height));
                builder.line_to(point(bounds.origin.x + x + height, bounds.origin.y));
                x += px(10.);
            }
            if let Ok(path) = builder.build() {
                window.paint_path(path, theme::color(theme::CANVAS));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn phase_indicator(phase: Phase, reduce_motion: bool) -> gpui::AnyElement {
    let symbol = |glyph: Icon| icon(glyph).size(px(11.)).text_color(theme::color(theme::SLATE)).into_any_element();
    match phase {
        Phase::Saved => symbol(Icon::Check),
        Phase::Paused => symbol(Icon::Pause),
        Phase::Interrupted => symbol(Icon::Warning),
        _ => activity_indicator("footer-activity", px(11.), reduce_motion, Icon::Ellipsis),
    }
}

/// "Indexed · updated 5 min ago" (completion).
fn completed_status(age: Option<String>) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .gap(px(4.))
        .whitespace_nowrap()
        .child(div().font_weight(theme::WEIGHT_MEDIUM).text_color(theme::color(theme::TEXT)).child("Indexed"))
        .children(age.map(|age| div().child(format!("· updated {age} ago"))))
}

/// "Indexed 5 min ago" (the saved map being mapped or updated again).
fn index_date(age: Option<String>) -> gpui::Div {
    div().flex_none().whitespace_nowrap().children(age.map(|age| format!("Indexed {age} ago")))
}

/// "12 skipped": items the map could not read.
fn skipped(state: &AppState) -> Option<impl IntoElement> {
    let skipped = state.snapshot()?.skipped_count;
    (skipped > 0).then(|| {
        div()
            .id("footer-skipped")
            .flex_none()
            .whitespace_nowrap()
            .cursor(gpui::CursorStyle::PointingHand)
            .text_color(theme::color(theme::CORAL))
            .hover(|s| s.underline())
            .child(format!("{} skipped", theme::format_count(skipped)))
            .tooltip(tooltip("Some items could not be scanned. Show details"))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, window, cx| crate::sheets::show_skipped_items(window, cx))
    })
}

fn status_text(status: Option<String>) -> impl IntoElement {
    let text = status.unwrap_or_default();
    div()
        .id("footer-status")
        .flex_1()
        .min_w_0()
        .flex()
        .justify_end()
        .child(div().min_w_0().truncate().child(text.clone()))
        .when(!text.is_empty(), |el| el.tooltip(tooltip(text)))
}

/// The footer action in a fixed 64-point slot (empty when there is none).
fn action_button(action: FooterAction, host: Host) -> impl IntoElement {
    let (label, event, help) = match action {
        FooterAction::Stop => ("Stop", Event::Pause, "Stop mapping this location"),
        FooterAction::Resume => ("Resume", Event::Resume, "Continue mapping this location"),
        FooterAction::Refresh => ("Refresh", Event::Refresh, "Refresh this map and restart updates"),
        FooterAction::Pause => ("Pause", Event::Pause, "Pause the current refresh"),
        FooterAction::None => return div().w(theme::FOOTER_ACTION_WIDTH).h(px(20.)).flex_none().into_any_element(),
    };
    small_button("footer-action", label, theme::FOOTER_ACTION_WIDTH, true, move |_, _, cx| host.send(event.clone(), cx))
        .tooltip(tooltip(help))
        .into_any_element()
}

/// The two-line status shown before progress values exist (Swift `legacyStatus`).
fn legacy_status(state: &AppState, host: &Host, reduce_motion: bool) -> gpui::Div {
    let busy = state.is_scanning() || state.is_refreshing();
    let partial = state.is_partial();
    let glyph = if busy {
        activity_indicator("footer-legacy-activity", px(12.), reduce_motion, Icon::Ellipsis)
    } else {
        let glyph = if partial || state.updates_paused() { Icon::CirclePause } else { Icon::CircleCheck };
        icon(glyph).size(px(12.)).text_color(theme::color(theme::SLATE)).into_any_element()
    };
    let mut row = div()
        .h(px(26.))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(7.))
        .child(div().size(px(14.)).flex_none().flex().items_center().justify_center().child(glyph));
    if partial {
        let title = if state.is_scanning() { state.title() } else { "Partial map · Not saved" };
        let found = root_file_count(state);
        let count = if state.updating_removal_sizes() {
            "Updating sizes…".to_owned()
        } else {
            format!("{} {} found", theme::format_count(found), if found == 1 { "file" } else { "files" })
        };
        row = row
            .child(
                div()
                    .w(theme::FOOTER_LEGACY_TITLE_WIDTH)
                    .flex_none()
                    .truncate()
                    .font_weight(theme::WEIGHT_MEDIUM)
                    .text_color(theme::color(theme::TEXT))
                    .child(title),
            )
            .child(theme::monospaced_digits(
                div().w(theme::FOOTER_LEGACY_COUNT_WIDTH).flex_none().truncate().child(count),
            ));
    } else if state.snapshot().is_some() {
        row = row
            .child(index_date(state.indexed_at().map(age_text)).w(theme::FOOTER_LEGACY_TITLE_WIDTH))
            .children(skipped(state));
    }
    let action = if state.is_scanning() {
        Some(("Stop Mapping", Event::Pause))
    } else if partial {
        Some(("Restart Mapping", Event::Refresh))
    } else if state.is_refreshing() {
        Some(("Pause", Event::Pause))
    } else {
        None
    };
    let host = host.clone();
    row = row.child(div().flex_1().min_w(px(8.))).child(
        div().w(theme::FOOTER_LEGACY_ACTION_WIDTH).flex_none().flex().justify_end().children(action.map(
            |(label, event)| {
                small_button("footer-legacy-action", label, px(110.), true, move |_, _, cx| {
                    host.send(event.clone(), cx)
                })
            },
        )),
    );
    let detail = match (partial, state.status()) {
        (_, Some(status)) => status.to_owned(),
        (true, None) if state.is_scanning() => theme::display(state.scan_path()),
        (true, None) => "Restart mapping to finish.".to_owned(),
        (false, None) if state.is_scanning() => state.title().to_owned(),
        (false, None) => String::new(),
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .justify_end()
        .gap(px(4.))
        .child(row)
        .child(div().h(px(14.)).w_full().truncate().child(detail))
}

/// Files found under the mapped root (the displayed folder may be a descendant).
fn root_file_count(state: &AppState) -> u64 {
    let Some(folder) = state.folder() else { return state.progress_count() };
    let tree = folder.tree();
    match tree.ancestors.first() {
        Some(&root) => tree.node(root).meta.file_count,
        None => folder.folder_meta().file_count,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{AppContext, TestAppContext};
    use orbit_app_state::model::ProgressSnapshot;
    use orbit_app_state::testing::Harness;
    use orbit_app_state::{AppState, Backend, Config, Event, MappingEvent, Regions};

    use crate::frame_stats;
    use crate::host::{AppHost, Host};
    use crate::main_window::MainWindow;
    use crate::settings::{Settings, SettingsStore};

    fn host_with(state: AppState, backend: Arc<dyn Backend>, cx: &mut TestAppContext) -> Host {
        cx.update(|cx| {
            let entity =
                cx.new(|cx| AppHost::with_state(state, backend, SettingsStore::in_memory(Settings::default()), cx));
            Host::from_entity(entity, cx)
        })
    }

    /// A progress tick that moves the coral share, then the frames of its transition: only the
    /// footer re-renders; map, list, sidebar, header, inspector and tray reuse their frames.
    #[gpui::test]
    fn progress_and_track_transition_rerender_only_the_footer(cx: &mut TestAppContext) {
        let mut h = Harness::new(Config::new(b"/home".to_vec()));
        for i in 0..40 {
            h.backend.add_file(format!("/r/d{}/f{i}", i % 4).as_bytes(), 10 + i);
        }
        h.open(b"/r");
        h.preview(b"/r");
        h.run();
        let session = h.state.active_session_id().expect("a mapping session");
        let host = host_with(h.state, h.backend, cx);
        let (root, cx) = cx.add_window_view({
            let host = host.clone();
            move |window, cx| MainWindow::new(host, None, window, cx)
        });
        let tick = |cached: u64, revision: u64, cx: &mut gpui::VisualTestContext| {
            let progress = ProgressSnapshot {
                revision,
                item_count: 44,
                cached_file_count: Some(cached),
                discovery_fraction: Some(0.5),
                ..ProgressSnapshot::default()
            };
            cx.update(|_, cx| host.send(Event::Mapping { session, event: MappingEvent::Progress(progress) }, cx));
            cx.run_until_parked();
        };
        tick(4, 1, cx);
        let footer = cx.read(|cx| root.read(cx).panels().footer.clone());
        let first = cx.read(|cx| footer.read(cx).share_target()).expect("the footer shows progress");

        let others =
            [Regions::MAP, Regions::LIST, Regions::SIDEBAR, Regions::HEADER, Regions::INSPECTOR, Regions::TRAY];
        let before = others.map(frame_stats::render_count);
        let footer_before = frame_stats::render_count(Regions::FOOTER);

        tick(30, 2, cx);
        let target = cx.read(|cx| footer.read(cx).share_target()).expect("progress");
        assert!(target > first, "the indexed share moved: {first} → {target}");
        // The transition's animation frames notify the footer view (`request_animation_frame`).
        for _ in 0..8 {
            cx.update(|_, cx| footer.update(cx, |_, cx| cx.notify()));
            cx.run_until_parked();
        }

        assert_eq!(others.map(frame_stats::render_count), before, "only the footer re-rendered");
        assert_eq!(frame_stats::render_count(Regions::FOOTER), footer_before + 9);
    }
}
