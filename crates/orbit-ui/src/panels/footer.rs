//! Mapping footer: a fixed 51 points between the explorer and the collection tray. Observes ONLY
//! [`Regions::FOOTER`]: progress ticks, the track transition, the completion fade and the age
//! refresh re-render this view and nothing else.
//!
//! While work runs, one sentence names it and a thin track measures it against the files expected
//! in total, so a full track means a finished map:
//! ```text
//!  ◌  Rescanning Macintosh HD                                       64%    ⚠ 780 skipped  [ Stop ]
//!     ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━──────────────
//!     6,702,410 found · 5,750,063 indexed · Saved map from 11 hr ago stays available   (status)
//! ```
//! The glyph hangs in the margin so headline, track and detail share one left edge; the action
//! sits beside the task it controls. Coral is the indexed share, a quiet ivory the files found;
//! without an estimate of the total there is no track and no percentage (nothing is invented).
//! At rest the block collapses to one calm line: `✓ Up to date · Indexed 5 min ago`. A background
//! update of the saved map keeps that line and only swaps the glyph and label in place
//! (`◌ Updating   · Indexed 5 min ago`): on a whole disk, updates come every few seconds, and
//! growing into the progress block each time would make the footer jump.
//!
//! The shares ease over [`theme::TRACK_TRANSITION`]; completion cross-fades to the resting line
//! over [`theme::FOOTER_FADE`]. Reduce Motion disables both and shows static symbols.
//!
//! [`FooterView`] is the whole presentation as plain values (unit-tested copy; the
//! `footer_gallery` example renders every state from it). Without progress values (a root but
//! no footer state yet) the legacy two-line status is shown, bottom aligned in the same height.

use std::time::{Duration, Instant, SystemTime};

use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, Window, div, prelude::*, px, relative,
};
use orbit_app_state::{AppState, Event, FooterAction, FooterState, MapProgress, Phase, Regions};

use crate::components::{activity_indicator, small_button, tooltip, truncate_middle};
use crate::frame_stats;
use crate::host::Host;
use crate::icons::{Icon, icon};
use crate::theme;

pub const REGIONS: Regions = Regions::FOOTER;

/// Left inset of the headline, track and detail: the glyph hangs in the margin before it.
const HANG: f32 = 20.;
const TRACK_HEIGHT: f32 = 4.;

/// Labels of the resting line. Its label slot is as wide as the widest, so the detail after it
/// does not move when an update starts or ends.
const RESTING_LABELS: [&str; 2] = ["Up to date", "Updating"];

/// The symbol hanging before the headline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterGlyph {
    Activity,
    Done,
    Paused,
    Warning,
}

/// Everything the footer shows, as plain values.
#[derive(Clone, Debug, PartialEq)]
pub struct FooterView {
    pub glyph: FooterGlyph,
    /// What is happening, as one sentence ("Rescanning Macintosh HD").
    pub headline: String,
    /// Share of the finished map, when there is an estimate ("64%").
    pub percent: Option<String>,
    pub progress: Option<MapProgress>,
    /// Paused or interrupted work: the track is kept, but quiet.
    pub dimmed: bool,
    /// The numbers behind the headline.
    pub detail: String,
    /// A transient message (collection feedback, a failed update), right aligned.
    pub status: Option<String>,
    pub skipped: u64,
    pub action: FooterAction,
    /// The map is saved (up to date, or updating in the background): one resting line instead of
    /// the progress block.
    pub resting: bool,
    /// Help of the track and percentage.
    pub help: Option<String>,
}

impl FooterView {
    /// The presentation of `footer` for the map of `root` (display name), saved `age` ago.
    pub fn new(footer: &FooterState, root: &str, age: Option<&str>, status: Option<&str>, skipped: u64) -> Self {
        let count = theme::format_count;
        let found = footer.discovered_files;
        let counts = match footer.cached_files {
            Some(indexed) => format!("{} found · {} indexed", count(found), count(indexed)),
            None => format!("{} found", count(found)),
        };
        let progress = footer.map_progress();
        let (glyph, headline, detail) = match footer.phase {
            Phase::Saved => (
                FooterGlyph::Done,
                RESTING_LABELS[0].to_owned(),
                age.map(|age| format!("Indexed {age} ago")).unwrap_or_default(),
            ),
            Phase::Finding => {
                let verb = if footer.rescan { "Rescanning" } else { "Mapping" };
                let mut detail = if found == 0 { "Looking for files".to_owned() } else { counts };
                if let Some(age) = age.filter(|_| footer.rescan) {
                    detail.push_str(&format!(" · Saved map from {age} ago stays available"));
                }
                (FooterGlyph::Activity, format!("{verb} {root}"), detail)
            }
            Phase::Saving => (FooterGlyph::Activity, format!("Indexing {root}"), format!("All {counts}")),
            Phase::Finishing => (FooterGlyph::Activity, format!("Finishing the index of {root}"), counts),
            Phase::Updating => (
                FooterGlyph::Activity,
                RESTING_LABELS[1].to_owned(),
                age.map(|age| format!("Indexed {age} ago")).unwrap_or_else(|| "Checking changed folders".to_owned()),
            ),
            Phase::Paused if footer.is_paused => {
                (FooterGlyph::Paused, "Mapping paused".to_owned(), format!("{counts} · Progress is kept"))
            }
            Phase::Paused => (
                FooterGlyph::Paused,
                "Automatic updates paused".to_owned(),
                match age {
                    Some(age) => format!("Showing the map saved {age} ago"),
                    None => "Showing the saved map".to_owned(),
                },
            ),
            Phase::Interrupted => {
                (FooterGlyph::Warning, "Mapping interrupted".to_owned(), format!("{counts} · Resume to continue"))
            }
        };
        let percent = progress.map(|p| percent_text(p.done()));
        let help = progress.map(|p| {
            let expected = footer.expected_files().map(count);
            let mut text = match (&expected, footer.discovery_complete) {
                (_, true) | (None, _) => format!("{} files found", count(found)),
                (Some(expected), false) => {
                    format!(
                        "{} of about {expected} files found ({})",
                        count(found),
                        footer.discovery_detail.to_lowercase()
                    )
                }
            };
            match (p.indexed, footer.cached_files) {
                (Some(_), Some(indexed)) => text.push_str(&format!(", {} indexed.", count(indexed))),
                _ => text.push('.'),
            }
            text.push_str(" The map is finished when the track is full.");
            text
        });
        Self {
            glyph,
            headline,
            percent,
            progress,
            dimmed: matches!(footer.phase, Phase::Paused | Phase::Interrupted),
            detail,
            status: status.filter(|s| !s.is_empty()).map(str::to_owned),
            skipped,
            action: footer.action,
            resting: matches!(footer.phase, Phase::Saved | Phase::Updating),
            help,
        }
    }
}

/// `"64%"`, never 100% before the map is done, `"<1%"` for a start.
fn percent_text(done: f64) -> String {
    let percent = (done.clamp(0.0, 1.0) * 100.0).floor() as u32;
    match percent {
        0 if done > 0.0 => "<1%".to_owned(),
        100 if done < 1.0 => "99%".to_owned(),
        percent => format!("{percent}%"),
    }
}

/// An eased transition of one value (a track share, the block opacity).
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

/// The shown (eased) values of one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shown {
    /// Found share of the track.
    pub found: f32,
    /// Indexed share of the track.
    pub indexed: f32,
    /// Opacity of the progress block; the resting line has the rest.
    pub progress_opacity: f32,
}

impl Shown {
    /// The settled values of `view` (no transition).
    pub fn settled(view: &FooterView) -> Self {
        let (found, indexed) = targets(view);
        Self { found, indexed, progress_opacity: if view.resting { 0.0 } else { 1.0 } }
    }
}

fn targets(view: &FooterView) -> (f32, f32) {
    view.progress.map_or((0.0, 0.0), |p| (p.found as f32, p.indexed.unwrap_or(0.0) as f32))
}

pub struct FooterPanel {
    host: Host,
    /// Indexed share of the track (target: `map_progress().indexed`).
    share: Option<Transition>,
    /// Found share of the track.
    found: Option<Transition>,
    /// Opacity of the progress block (0 once the map is saved).
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
            found: None,
            progress_opacity: None,
            root_label: None,
            age_timer: None,
            _subscriptions: host.observe(REGIONS, cx).into_iter().chain([theme::observe_motion(cx)]).collect(),
        }
    }

    /// A share or fade transition is in progress (it requests animation frames for this view).
    pub fn is_animating(&self) -> bool {
        let now = Instant::now();
        [self.share, self.found, self.progress_opacity].iter().flatten().any(|t| t.running(now))
    }

    /// The indexed share the track is moving to, `None` before the first progress render.
    pub fn share_target(&self) -> Option<f32> {
        self.share.map(|t| t.to)
    }

    fn root_label(&mut self, name: &str, window: &Window) -> SharedString {
        if let Some((cached, label)) = &self.root_label
            && cached == name
        {
            return label.clone();
        }
        let label = truncate_middle(name, px(theme::FOOTER_ROOT_WIDTH), theme::TEXT_BODY, theme::WEIGHT_MEDIUM, window);
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

    /// Eases the shown values towards `view`'s.
    fn shown(&mut self, view: &FooterView, reduce_motion: bool, window: &mut Window) -> Shown {
        let now = Instant::now();
        let animate = !reduce_motion;
        let target = Shown::settled(view);
        let ease = |slot: &mut Option<Transition>, to: f32, duration: Duration| {
            let transition = slot.get_or_insert_with(|| Transition::settled(to, duration, now));
            transition.retarget(to, animate, now);
            *transition
        };
        let indexed = ease(&mut self.share, target.indexed, theme::TRACK_TRANSITION);
        let found = ease(&mut self.found, target.found, theme::TRACK_TRANSITION);
        let fade = ease(&mut self.progress_opacity, target.progress_opacity, theme::FOOTER_FADE);
        if animate && [indexed, found, fade].iter().any(|t| t.running(now)) {
            window.request_animation_frame();
        }
        if !animate {
            return target;
        }
        Shown { found: found.value(now), indexed: indexed.value(now), progress_opacity: fade.value(now) }
    }
}

impl Render for FooterPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame_stats::rendered(REGIONS);
        let reduce_motion = theme::reduce_motion(cx);
        let host = self.host.clone();
        let state = host.state(cx);
        let content = if let Some(footer) = state.footer() {
            let root = self.root_label(&footer.root_name, window);
            let age = state.indexed_at().map(age_text);
            let skipped = state.snapshot().map_or(0, |s| s.skipped_count);
            let view = FooterView::new(footer, &root, age.as_deref(), state.status(), skipped);
            let shown = self.shown(&view, reduce_motion, window);
            Some((footer_content(&view, shown, reduce_motion, Some(&self.host)), age.is_some()))
        } else if state.root_path().is_some() || state.is_scanning() || state.is_refreshing() {
            self.share = None;
            self.found = None;
            self.progress_opacity = None;
            let shows_age = !state.is_partial() && state.snapshot().is_some() && state.indexed_at().is_some();
            Some((legacy_status(state, &self.host, reduce_motion), shows_age))
        } else {
            self.share = None;
            self.found = None;
            self.progress_opacity = None;
            None
        };
        let shows_age = content.as_ref().is_some_and(|(_, age)| *age);
        self.keep_age_fresh(shows_age, cx);
        footer_frame(content.map(|(content, _)| content))
    }
}

/// The footer's fixed frame: padding, small type, tabular digits.
pub fn footer_frame(content: Option<gpui::Div>) -> gpui::Div {
    // Counts and ages change constantly: digits share one width so nothing jitters.
    theme::monospaced_digits(
        div()
            .size_full()
            .px(px(28.))
            .text_size(theme::TEXT_SMALL)
            .text_color(theme::color(theme::SLATE))
            .children(content),
    )
}

/// The footer for `view` at the eased values `shown`. Without a `host` (the gallery) the action
/// button is inert.
pub fn footer_content(view: &FooterView, shown: Shown, reduce_motion: bool, host: Option<&Host>) -> gpui::Div {
    let trailing = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(14.))
        .children((view.skipped > 0).then(|| skipped_link(view.skipped)))
        .child(action_button(view.action, host.cloned()));
    let mut layers = div().relative().flex_1().min_w_0().h_full();
    if shown.progress_opacity > 0.0 {
        layers = layers.child(div().absolute().inset_0().opacity(shown.progress_opacity).child(progress_block(
            view,
            shown,
            reduce_motion,
        )));
    }
    if shown.progress_opacity < 1.0 {
        layers = layers.child(
            div().absolute().inset_0().opacity(1.0 - shown.progress_opacity).child(resting_line(view, reduce_motion)),
        );
    }
    div().size_full().flex().flex_row().items_center().gap(px(18.)).child(layers).child(trailing)
}

/// Glyph, headline and percentage; the track; the detail and status.
fn progress_block(view: &FooterView, shown: Shown, reduce_motion: bool) -> gpui::Div {
    let headline = div()
        .h(px(16.))
        .flex()
        .flex_row()
        .items_center()
        .child(glyph_slot("footer-activity", view.glyph, reduce_motion))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(theme::TEXT_BODY)
                .font_weight(theme::WEIGHT_MEDIUM)
                .text_color(theme::color(theme::TEXT))
                .child(view.headline.clone()),
        )
        .children(view.percent.clone().map(|percent| {
            div()
                .id("footer-percent")
                .flex_none()
                .pl(px(12.))
                .text_size(theme::TEXT_BODY)
                .font_weight(theme::WEIGHT_MEDIUM)
                .text_color(theme::color(if view.dimmed { theme::SLATE } else { theme::TEXT }))
                .child(percent)
                .when_some(view.help.clone(), |el, help| el.tooltip(tooltip(help)))
        }));
    let track = view.progress.map(|_| {
        let fill = |share: f32, color: gpui::Rgba| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .w(relative(share.clamp(0.0, 1.0)))
                .rounded(px(TRACK_HEIGHT / 2.))
                .bg(color)
        };
        let alpha = if view.dimmed { 0.45 } else { 1.0 };
        div()
            .id("mapping-track")
            .ml(px(HANG))
            .relative()
            .h(px(TRACK_HEIGHT))
            .rounded(px(TRACK_HEIGHT / 2.))
            .overflow_hidden()
            .bg(theme::white(0.08))
            .child(fill(shown.found, theme::with_alpha(theme::IVORY, 0.32 * alpha)))
            .child(fill(shown.indexed, theme::with_alpha(theme::CORAL, alpha)))
            .when_some(view.help.clone(), |el, help| el.tooltip(tooltip(help)))
    });
    let detail = div()
        .ml(px(HANG))
        .h(px(14.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.))
        .child(div().flex_1().min_w_0().truncate().child(view.detail.clone()))
        .children(view.status.clone().map(status_text));
    div().size_full().flex().flex_col().justify_center().gap(px(6.)).child(headline).children(track).child(detail)
}

/// `✓ Up to date · Indexed 5 min ago` … status (`◌ Updating` while the saved map is updated).
fn resting_line(view: &FooterView, reduce_motion: bool) -> gpui::Div {
    // The other labels are laid out at zero height: they only widen the slot to the widest one.
    let sizers = RESTING_LABELS
        .iter()
        .filter(|label| **label != view.headline)
        .map(|label| div().h(px(0.)).overflow_hidden().opacity(0.).child(*label));
    div()
        .size_full()
        .flex()
        .flex_row()
        .items_center()
        .child(glyph_slot("footer-resting-activity", view.glyph, reduce_motion))
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .text_size(theme::TEXT_BODY)
                .font_weight(theme::WEIGHT_MEDIUM)
                .text_color(theme::color(theme::TEXT))
                .child(view.headline.clone())
                .children(sizers),
        )
        .when(!view.detail.is_empty(), |el| {
            el.child(div().flex_none().px(px(6.)).child("·")).child(div().flex_none().child(view.detail.clone()))
        })
        .child(div().flex_1().min_w(px(12.)))
        .children(view.status.clone().map(status_text))
}

/// The hanging glyph in a fixed slot.
fn glyph_slot(id: &'static str, glyph: FooterGlyph, reduce_motion: bool) -> gpui::Div {
    let symbol = |shape: Icon, color: u32| icon(shape).size(px(11.)).text_color(theme::color(color)).into_any_element();
    let element = match glyph {
        FooterGlyph::Activity => activity_indicator(id, px(11.), reduce_motion, Icon::Ellipsis),
        FooterGlyph::Done => symbol(Icon::CircleCheck, theme::SLATE),
        FooterGlyph::Paused => symbol(Icon::CirclePause, theme::SLATE),
        FooterGlyph::Warning => symbol(Icon::Warning, theme::CORAL),
    };
    div().w(px(HANG)).h(px(12.)).flex_none().flex().items_center().child(element)
}

/// "5 min" since the saved timestamp (a clock moving backwards reads as "less than a minute").
fn age_text(indexed_at: SystemTime) -> String {
    theme::format_age(SystemTime::now().duration_since(indexed_at).unwrap_or_default())
}

/// "Indexed 5 min ago" (the saved map being mapped or updated again).
fn index_date(age: Option<String>) -> gpui::Div {
    div().flex_none().whitespace_nowrap().children(age.map(|age| format!("Indexed {age} ago")))
}

/// "⚠ 12 skipped": items the map could not read; opens their details. Neutral, not an alarm:
/// unreadable system folders are expected without Full Disk Access.
fn skipped_link(count: u64) -> impl IntoElement {
    div()
        .id("footer-skipped")
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(5.))
        .whitespace_nowrap()
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| s.text_color(theme::color(theme::TEXT)).underline())
        .child(icon(Icon::Warning).size(px(10.)).text_color(theme::color(theme::SLATE)))
        .child(format!("{} skipped", theme::format_count(count)))
        .tooltip(tooltip("Some items could not be read. Show details"))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, window, cx| crate::sheets::show_skipped_items(window, cx))
}

/// "12 skipped" of the legacy status (before progress values exist).
fn skipped(state: &AppState) -> Option<impl IntoElement> {
    let skipped = state.snapshot()?.skipped_count;
    (skipped > 0).then(|| skipped_link(skipped))
}

fn status_text(text: String) -> gpui::Stateful<gpui::Div> {
    div()
        .id("footer-status")
        .flex_shrink()
        .min_w_0()
        .max_w(relative(0.55))
        .truncate()
        .child(text.clone())
        .tooltip(tooltip(text))
}

/// The footer action in a fixed 64-point slot (empty when there is none).
fn action_button(action: FooterAction, host: Option<Host>) -> impl IntoElement {
    let (label, event, help) = match action {
        FooterAction::Stop => ("Stop", Event::Pause, "Stop mapping this location"),
        FooterAction::Resume => ("Resume", Event::Resume, "Continue mapping this location"),
        FooterAction::Refresh => ("Refresh", Event::Refresh, "Refresh this map and restart updates"),
        FooterAction::Pause => ("Pause", Event::Pause, "Pause the current refresh"),
        FooterAction::None => return div().w(theme::FOOTER_ACTION_WIDTH).h(px(20.)).flex_none().into_any_element(),
    };
    small_button("footer-action", label, theme::FOOTER_ACTION_WIDTH, true, move |_, _, cx| {
        if let Some(host) = &host {
            host.send(event.clone(), cx);
        }
    })
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
        Some(("Restart Mapping", Event::RescanEverything))
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

    fn state(phase: orbit_app_state::Phase) -> orbit_app_state::FooterState {
        orbit_app_state::FooterState {
            root_name: "Macintosh HD".into(),
            discovered_files: 6_702_410,
            cached_files: Some(4_633_420),
            discovery_fraction: Some(1.02),
            discovery_detail: "More files than expected",
            is_paused: false,
            discovery_complete: false,
            rescan: true,
            phase,
            action: orbit_app_state::FooterAction::Stop,
        }
    }

    /// The copy answers "is the map ready, what is happening, how far": one sentence, a
    /// percentage of the finished map only with an estimate, the numbers in context.
    #[test]
    fn footer_copy_names_the_work_and_measures_it_against_the_total() {
        use super::{FooterGlyph, FooterView, percent_text};
        use orbit_app_state::Phase;
        let rescan = FooterView::new(&state(Phase::Finding), "Macintosh HD", Some("11 hr, 26 min"), None, 780);
        assert_eq!(rescan.headline, "Rescanning Macintosh HD");
        assert_eq!(rescan.percent.as_deref(), Some("68%"), "indexed share of the expected total");
        assert_eq!(
            rescan.detail,
            "6,702,410 found · 4,633,420 indexed · Saved map from 11 hr, 26 min ago stays available"
        );
        assert!(rescan.help.as_deref().is_some_and(|h| h.contains("more files than expected")), "{:?}", rescan.help);
        assert!(!rescan.resting && rescan.glyph == FooterGlyph::Activity);

        let first = orbit_app_state::FooterState { discovery_fraction: None, rescan: false, ..state(Phase::Finding) };
        let first = FooterView::new(&first, "Home", None, None, 0);
        assert_eq!((first.headline.as_str(), first.percent, first.progress), ("Mapping Home", None, None));

        let saved = orbit_app_state::FooterState { phase: Phase::Saved, ..state(Phase::Saved) };
        let saved = FooterView::new(&saved, "Macintosh HD", Some("5 min"), None, 780);
        assert_eq!(
            (saved.headline.as_str(), saved.detail.as_str(), saved.resting),
            ("Up to date", "Indexed 5 min ago", true)
        );

        // A background update keeps the resting line: only the glyph and label change.
        let updating = orbit_app_state::FooterState {
            cached_files: None,
            discovery_complete: true,
            action: orbit_app_state::FooterAction::Pause,
            ..state(Phase::Updating)
        };
        let updating = FooterView::new(&updating, "Macintosh HD", Some("5 min"), None, 780);
        assert_eq!(
            (updating.headline.as_str(), updating.detail.as_str(), updating.resting, updating.progress),
            ("Updating", "Indexed 5 min ago", true, None)
        );
        assert_eq!(updating.glyph, FooterGlyph::Activity);

        let paused = orbit_app_state::FooterState {
            phase: Phase::Paused,
            is_paused: false,
            discovery_complete: true,
            action: orbit_app_state::FooterAction::Refresh,
            ..state(Phase::Paused)
        };
        let paused = FooterView::new(&paused, "Home", Some("38 min"), Some("Map kept. Update failed: disk busy"), 0);
        assert_eq!(paused.headline, "Automatic updates paused");
        assert_eq!((paused.progress, paused.status.as_deref()), (None, Some("Map kept. Update failed: disk busy")));

        assert_eq!(
            (percent_text(0.004), percent_text(0.999), percent_text(1.0)),
            ("<1%".into(), "99%".into(), "100%".into())
        );
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
