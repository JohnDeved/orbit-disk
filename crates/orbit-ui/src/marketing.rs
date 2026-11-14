//! `--marketing-state`: drives the app into one of the README screenshot states on the sample
//! fixture ([`crate::fixture::FixtureLayout::Sample`]) and reports when it is on screen
//! (`scripts/capture-marketing.sh`).
//!
//! The states are reached through ordinary [`Event`]s sent after the saved Home map is
//! published, one step at a time, each once its predecessor shows in the state (panel behavior
//! is unchanged). The state counts as shown once nothing is pending (no requests or compute jobs
//! in flight, no validation, opening or sort, no footer transition), the window has its requested
//! size, the expected sheet is (or no sheet is) open, and two polls in a row handled no event.
//! In the app, the ready callback then waits until a frame drawn after that point was presented.
//! Motion is reduced for the whole run, so no transition is ever pending.

use std::fmt;

use gpui::{App, Pixels, Size, Task, WindowHandle};
use orbit_app_state::{AppState, Event};

use crate::fixture::{self, sample};
use crate::host::AppHost;
use crate::main_window::MainWindow;
use crate::theme;

/// A README screenshot state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketingState {
    /// The Home map with "Projects" selected (map and list), nothing collected.
    Overview,
    /// The Home map with "Downloads" selected, nothing collected: the start of a collect drag.
    Downloads,
    /// "Downloads" selected and collected: marked on map and list, shown in the tray.
    Collection,
    /// "Downloads" collected, then "Projects" selected: the README hero, with the largest folder
    /// selected and the collection in use.
    Hero,
    /// "Projects" opened with its largest folder "Studio" selected.
    Projects,
    /// "Projects" opened with nothing selected, so the map center describes Projects.
    ProjectsOpen,
    /// The removal review of the collected "Downloads" (Move to Trash / Delete Permanently).
    Review,
    /// The review with its Move to Trash confirmation open.
    TrashConfirm,
    /// The review with its permanent-deletion confirmation sheet open.
    PermanentDelete,
}

impl MarketingState {
    pub const ALL: [Self; 9] = [
        Self::Overview,
        Self::Downloads,
        Self::Collection,
        Self::Hero,
        Self::Projects,
        Self::ProjectsOpen,
        Self::Review,
        Self::TrashConfirm,
        Self::PermanentDelete,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Downloads => "downloads",
            Self::Collection => "collection",
            Self::Hero => "hero",
            Self::Projects => "projects",
            Self::ProjectsOpen => "projects-open",
            Self::Review => "review",
            Self::TrashConfirm => "trash-confirm",
            Self::PermanentDelete => "permanent-delete",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.name() == name)
    }

    fn steps(self) -> &'static [Step] {
        match self {
            Self::Overview => &[Step::Select(sample::PROJECTS)],
            Self::Downloads => &[Step::Select(sample::DOWNLOADS)],
            Self::Collection => &[Step::Select(sample::DOWNLOADS), Step::Collect(sample::DOWNLOADS)],
            // Collecting is by path, so it needs no selection (every step must still hold at the end).
            Self::Hero => &[Step::Collect(sample::DOWNLOADS), Step::Select(sample::PROJECTS)],
            Self::Projects => &[Step::Open(sample::PROJECTS), Step::Select(sample::STUDIO)],
            Self::ProjectsOpen => &[Step::Open(sample::PROJECTS)],
            Self::Review => &[Step::Select(sample::DOWNLOADS), Step::Collect(sample::DOWNLOADS), Step::Review],
            Self::TrashConfirm => &[
                Step::Select(sample::DOWNLOADS),
                Step::Collect(sample::DOWNLOADS),
                Step::Review,
                Step::Confirm { permanent: false },
            ],
            Self::PermanentDelete => &[
                Step::Select(sample::DOWNLOADS),
                Step::Collect(sample::DOWNLOADS),
                Step::Review,
                Step::Confirm { permanent: true },
            ],
        }
    }

    /// Home is mapped and every step of this state shows in `state`.
    /// Whether this state shows a sheet.
    fn shows_sheet(self) -> bool {
        matches!(self, Self::Review | Self::TrashConfirm | Self::PermanentDelete)
    }

    pub fn reached(self, state: &AppState) -> bool {
        home_mapped(state) && self.steps().iter().all(|step| step.done(state))
    }
}

impl fmt::Display for MarketingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The saved Home map is published and listed.
fn home_mapped(state: &AppState) -> bool {
    state.root_path() == Some(fixture::HOME)
        && state.snapshot().is_some()
        && !state.is_scanning()
        && state.row_count() > 0
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Select(&'static str),
    Open(&'static str),
    Collect(&'static str),
    Review,
    /// Opens a confirmation from the review (a sheets-layer action, not an app event).
    Confirm {
        permanent: bool,
    },
}

impl Step {
    fn done(self, state: &AppState) -> bool {
        match self {
            Self::Select(relative) => state.selected().is_some_and(|item| *item.path == *sample::path(relative)),
            Self::Open(relative) => !state.is_opening() && state.current_path() == Some(&sample::path(relative)[..]),
            Self::Collect(relative) => state
                .collection()
                .is_some_and(|collection| collection.items().iter().any(|item| *item.path == *sample::path(relative))),
            Self::Review => state.show_review(),
            Self::Confirm { .. } => state.confirmation_lease().is_some(),
        }
    }

    fn action(self) -> Poll {
        match self {
            Self::Confirm { permanent } => Poll::Confirm { permanent },
            _ => Poll::Send(self.event()),
        }
    }

    fn event(self) -> Event {
        let path = |relative| sample::path(relative).into();
        match self {
            Self::Select(relative) => Event::Select { path: Some(path(relative)) },
            Self::Open(relative) => Event::Open { path: path(relative) },
            Self::Collect(relative) => Event::Collect { paths: vec![path(relative)] },
            Self::Review | Self::Confirm { .. } => Event::BeginReview,
        }
    }
}

/// What the window shows besides the state (read by the runner).
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowFacts {
    /// A sheet (review, alert, error) is open.
    pub sheet_open: bool,
    /// The footer is running a transition.
    pub animating: bool,
    /// The content has the requested size (always true without one).
    pub sized: bool,
    /// The content size the window has.
    pub viewport: Size<Pixels>,
}

/// The next thing the runner does.
#[derive(Debug)]
pub enum Poll {
    Send(Event),
    /// Open the review's trash (`false`) or permanent-delete (`true`) confirmation.
    Confirm {
        permanent: bool,
    },
    Wait,
    /// The state is reached and nothing is pending.
    Settled,
}

/// Steps towards a [`MarketingState`]; [`Director::poll`] it until [`Poll::Settled`].
#[derive(Debug)]
pub struct Director {
    target: MarketingState,
    next: usize,
    sent: bool,
    /// Events handled at the previous idle poll.
    quiet_since: Option<u64>,
}

impl Director {
    pub fn new(target: MarketingState) -> Self {
        Self { target, next: 0, sent: false, quiet_since: None }
    }

    pub fn poll(&mut self, host: &AppHost, window: WindowFacts) -> Poll {
        let state = host.state();
        if !home_mapped(state) {
            self.quiet_since = None;
            return Poll::Wait;
        }
        let steps = self.target.steps();
        while let Some(step) = steps.get(self.next) {
            if step.done(state) {
                self.next += 1;
                self.sent = false;
                continue;
            }
            self.quiet_since = None;
            if self.sent {
                return Poll::Wait;
            }
            self.sent = true;
            return step.action();
        }
        let idle = host.in_flight() == 0
            && !state.is_opening()
            && !state.is_validating_collection()
            && !state.is_updating_collection()
            && !state.is_sorting_rows()
            && state.error().is_none()
            && self.target.reached(state)
            && window.sheet_open == self.target.shows_sheet()
            && !window.animating
            && window.sized;
        if !idle {
            self.quiet_since = None;
            return Poll::Wait;
        }
        if self.quiet_since == Some(host.handled()) {
            return Poll::Settled;
        }
        self.quiet_since = Some(host.handled());
        Poll::Wait
    }

    /// Why the state is not settled yet (for the give-up message).
    pub fn describe(&self, host: &AppHost, window: WindowFacts) -> String {
        let state = host.state();
        let waiting: Vec<&str> = [
            (state.is_opening(), "opening"),
            (state.is_validating_collection(), "validating"),
            (state.is_updating_collection(), "updating the collection"),
            (state.is_sorting_rows(), "sorting"),
            (state.error().is_some(), "an error"),
            (window.sheet_open != self.target.shows_sheet(), "the sheet"),
            (window.animating, "the footer animation"),
            (!window.sized, "the window size"),
        ]
        .into_iter()
        .filter_map(|(pending, name)| pending.then_some(name))
        .collect();
        format!(
            "step {}/{}, root {:?}, folder {:?}, selected {:?}, collected {}, review {}, in flight {}, window {}x{}, \
             waiting for [{}]",
            self.next,
            self.target.steps().len(),
            state.root_path().map(theme::display),
            state.current_path().map(theme::display),
            state.selected().map(|item| theme::display(&item.path)),
            state.collection().map_or(0, |c| c.len()),
            state.show_review(),
            host.in_flight(),
            f32::from(window.viewport.width),
            f32::from(window.viewport.height),
            waiting.join(", "),
        )
    }
}

/// How [`drive`] runs.
#[derive(Clone, Copy, Debug)]
pub struct MarketingRun {
    pub state: MarketingState,
    /// The content size the window must have before the state counts as shown.
    pub window_size: Option<Size<Pixels>>,
    /// Report readiness only after a frame drawn in the settled state was presented (the app;
    /// test windows never present frames).
    pub await_presented_frame: bool,
}

/// Interval between polls.
pub const POLL: std::time::Duration = std::time::Duration::from_millis(50);
/// Polls before giving up (30 s).
pub const MAX_POLLS: u32 = 600;
/// How long the settled state may wait for a presented frame.
pub const FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Reduces motion, then drives `window`'s host to `run.state` and calls `ready` once it is shown
/// (or `failed` with a diagnostic when it was not reached within [`MAX_POLLS`], or no frame was
/// presented within [`FRAME_TIMEOUT`] after that).
pub fn drive(
    window: WindowHandle<MainWindow>,
    run: MarketingRun,
    ready: impl FnOnce(&mut App) + 'static,
    failed: impl FnOnce(String, &mut App) + 'static,
    cx: &mut App,
) -> Task<()> {
    theme::set_reduce_motion(true, cx);
    cx.spawn(async move |cx| {
        let mut director = Director::new(run.state);
        let mut settled = false;
        for _ in 0..MAX_POLLS {
            cx.background_executor().timer(POLL).await;
            let poll = window.update(cx, |main, window, cx| {
                let host = main.host().clone();
                let panels = main.panels();
                let facts = facts(main, window, run, cx);
                match director.poll(host.read(cx), facts) {
                    Poll::Send(event) => {
                        host.send(event, cx);
                        false
                    }
                    Poll::Confirm { permanent } => {
                        let sheets = panels.sheets.clone();
                        sheets.update(cx, |sheets, cx| sheets.open_confirmation(permanent, cx));
                        false
                    }
                    Poll::Wait => false,
                    Poll::Settled => true,
                }
            });
            match poll {
                Ok(true) => {
                    settled = true;
                    break;
                }
                Ok(false) => {}
                // The window closed: nothing to show.
                Err(_) => return,
            }
        }
        if !settled {
            let detail = window
                .update(cx, |main, window, cx| director.describe(main.host().read(cx), facts(main, window, run, cx)))
                .unwrap_or_else(|_| "the window closed".into());
            cx.update(|cx| failed(format!("{} was not reached: {detail}", run.state), cx)).ok();
            return;
        }
        if run.await_presented_frame {
            // No frames tick while the screen is locked or asleep: say so instead of hanging.
            // Dropped (cancelled) once a frame was presented.
            let _watchdog = cx.spawn(async move |cx| {
                cx.background_executor().timer(FRAME_TIMEOUT).await;
                let message = format!(
                    "{} is reached but no frame was presented within {FRAME_TIMEOUT:?} (is the screen locked or \
                     asleep?)",
                    run.state
                );
                cx.update(|cx| failed(message, cx)).ok();
            });
            // Frame callbacks run before that frame is drawn: the first one's frame shows the
            // settled state, the second one runs after it was presented.
            let (sender, receiver) = futures::channel::oneshot::channel();
            let armed = window.update(cx, |_, window, _| {
                window.refresh();
                window.on_next_frame(move |window, _| {
                    window.refresh();
                    window.on_next_frame(move |_, _| {
                        let _ = sender.send(());
                    });
                });
            });
            if armed.is_err() || receiver.await.is_err() {
                return;
            }
        }
        cx.update(ready).ok();
    })
}

fn facts(main: &MainWindow, window: &gpui::Window, run: MarketingRun, cx: &App) -> WindowFacts {
    let panels = main.panels();
    let viewport = window.viewport_size();
    WindowFacts {
        sheet_open: panels.sheets.read(cx).is_open(cx),
        animating: panels.footer.read(cx).is_animating(),
        sized: run.window_size.is_none_or(|size| {
            (viewport.width - size.width).abs() < gpui::px(0.5) && (viewport.height - size.height).abs() < gpui::px(0.5)
        }),
        viewport,
    }
}

#[cfg(test)]
mod tests;
