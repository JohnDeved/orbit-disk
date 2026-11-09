//! [`AppHost`]: the GPUI entity that owns the [`AppState`] and performs its [`Effect`]s, and
//! [`Host`], the cheap handle panels hold.
//!
//! Effects are hosted exactly as `orbit-app-state` documents them:
//! - `Backend` runs [`orbit_app_state::perform`] on the background executor with a fresh
//!   [`Cancellation`]; the reply event is handled on the UI thread with `Instant::now()`.
//!   `Cancel(id)` cancels that token and drops the reply.
//! - `Compute` runs the job on the background executor (cancellable the same way).
//! - `Timer` sleeps on the executor's timer and sends `Event::Timer`.
//! - Mapping/monitor notifications arrive through the [`Notifier`] channel, drained by one
//!   foreground task (events already queued are handled in one entity update).
//! - `Platform` reveals or opens with the system (Quick Look is out of v1 scope: Preview opens
//!   with the default application).
//! - `Persist` updates the settings file (atomic write, off the UI thread).
//! - `Notify(regions)` notifies only those regions' [`RegionSignal`]s.
//!
//! The host itself never calls `cx.notify()`: views re-render through region signals only.
//!
//! [`RegionSignal`]: crate::regions::RegionSignal

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use gpui::{App, AppContext, Context, Entity, EventEmitter, SharedString, Subscription, Task};
use orbit_app_state::event::{Effect, PlatformAction};
use orbit_app_state::{AppState, Backend, Config, Event, Notifier, Regions, RequestId, perform};
use orbit_core::view::NodeIx;
use orbit_platform::Cancellation;
use orbit_sunburst::{Presentation, SegmentIndex};

use crate::regions::{RegionSignal, RegionSignals};
use crate::settings::SettingsStore;

/// What the host is built from.
pub struct HostOptions {
    pub config: Config,
    pub backend: Arc<dyn Backend>,
    pub settings: SettingsStore,
}

/// Events the host emits for app-level wiring (updater, quit guard).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// `AppState::removal_active` changed: block or unblock the updater.
    RemovalActivityChanged(bool),
}

pub struct AppHost {
    state: AppState,
    backend: Arc<dyn Backend>,
    notifier: Notifier,
    /// Backend requests and compute jobs whose reply is still wanted.
    in_flight: HashMap<RequestId, Cancellation>,
    regions: RegionSignals,
    settings: SettingsStore,
    removal_active: bool,
    /// Events handled so far (diagnostics, tests).
    handled: u64,
    /// Host-level message shown in the header (e.g. the quit veto); cleared by the next event.
    notice: Option<SharedString>,
    _inbox: Task<()>,
}

impl EventEmitter<HostEvent> for AppHost {}

impl std::fmt::Debug for AppHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppHost").field("state", &self.state).field("in_flight", &self.in_flight.len()).finish()
    }
}

impl AppHost {
    pub fn new(options: HostOptions, cx: &mut Context<Self>) -> Self {
        let state = AppState::new(options.config, Instant::now());
        Self::with_state(state, options.backend, options.settings, cx)
    }

    /// Hosts an existing state (tests drive it to a situation first).
    pub fn with_state(
        state: AppState,
        backend: Arc<dyn Backend>,
        settings: SettingsStore,
        cx: &mut Context<Self>,
    ) -> Self {
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<Event>();
        let notifier: Notifier = Arc::new(move |event| {
            let _ = sender.unbounded_send(event);
        });
        let inbox = cx.spawn(async move |this, cx| {
            while let Some(first) = receiver.next().await {
                let mut batch = vec![first];
                while let Ok(event) = receiver.try_recv() {
                    batch.push(event);
                }
                let delivered = this.update(cx, |host, cx| {
                    for event in batch {
                        host.dispatch(event, cx);
                    }
                });
                if delivered.is_err() {
                    break;
                }
            }
        });
        let removal_active = state.removal_active();
        Self {
            state,
            backend,
            notifier,
            in_flight: HashMap::new(),
            regions: RegionSignals::new(cx),
            settings,
            removal_active,
            handled: 0,
            notice: None,
            _inbox: inbox,
        }
    }

    // -----------------------------------------------------------------------------------------
    // Read API.

    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The map to paint (layout + collection marks); rebuilt only when its inputs changed.
    pub fn presentation(&mut self) -> Option<Presentation<NodeIx>> {
        self.state.presentation()
    }

    /// The map segment to outline for the hovered row.
    pub fn row_hover_segment(&mut self) -> Option<SegmentIndex> {
        self.state.row_hover_segment()
    }

    pub fn regions(&self) -> &RegionSignals {
        &self.regions
    }

    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    pub fn settings(&self) -> &SettingsStore {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut SettingsStore {
        &mut self.settings
    }

    /// A host-level message for the header (shown instead of / next to `state.status()`).
    pub fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    pub fn set_notice(&mut self, notice: Option<SharedString>, cx: &mut Context<Self>) {
        if self.notice != notice {
            self.notice = notice;
            self.regions.notify(Regions::HEADER, cx);
        }
    }

    /// Events handled so far (diagnostics, tests).
    pub fn handled(&self) -> u64 {
        self.handled
    }

    /// Requests whose replies are still wanted (diagnostics, tests).
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    // -----------------------------------------------------------------------------------------
    // Input.

    /// Handles one event now and performs its effects.
    pub fn dispatch(&mut self, event: Event, cx: &mut Context<Self>) {
        if self.notice.is_some() && is_user_intent(&event) {
            self.set_notice(None, cx);
        }
        self.handled += 1;
        let effects = self.state.handle(event, Instant::now());
        self.perform(effects, cx);
    }

    /// App launch: volume discovery and reopening `last_location`.
    pub fn launch(&mut self, last_location: Option<Vec<u8>>, cx: &mut Context<Self>) {
        let effects = self.state.launch(last_location, Instant::now());
        self.perform(effects, cx);
    }

    /// Runs an automation command against the state and performs what it produced
    /// (`AppState::take_automation_effects`).
    pub fn automation<R>(&mut self, command: impl FnOnce(&mut AppState) -> R, cx: &mut Context<Self>) -> R {
        let result = command(&mut self.state);
        let effects = self.state.take_automation_effects();
        self.perform(effects, cx);
        result
    }

    // -----------------------------------------------------------------------------------------
    // Effects.

    pub(crate) fn perform(&mut self, effects: Vec<Effect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                Effect::Backend { id, request } => {
                    let cancel = Cancellation::new();
                    self.in_flight.insert(id, cancel.clone());
                    let backend = Arc::clone(&self.backend);
                    let notifier = Arc::clone(&self.notifier);
                    let job = cx.background_spawn(async move { perform(&*backend, id, request, &cancel, &notifier) });
                    cx.spawn(async move |this, cx| {
                        let reply = job.await;
                        this.update(cx, |host, cx| host.complete(id, reply, cx)).ok();
                    })
                    .detach();
                }
                Effect::Compute { id, job } => {
                    self.in_flight.insert(id, Cancellation::new());
                    let job = cx.background_spawn(async move { job.run() });
                    cx.spawn(async move |this, cx| {
                        let result = job.await;
                        this.update(cx, |host, cx| host.complete(id, Some(Event::Computed { id, result }), cx)).ok();
                    })
                    .detach();
                }
                Effect::Cancel(id) => {
                    if let Some(cancel) = self.in_flight.remove(&id) {
                        cancel.cancel();
                    }
                }
                Effect::Timer { after, timer } => {
                    let sleep = cx.background_executor().timer(after);
                    cx.spawn(async move |this, cx| {
                        sleep.await;
                        this.update(cx, |host, cx| host.dispatch(Event::Timer(timer), cx)).ok();
                    })
                    .detach();
                }
                Effect::Platform(action) => match action {
                    PlatformAction::Reveal(path) => cx.reveal_path(&os_path(&path)),
                    PlatformAction::Open(path) | PlatformAction::Preview(path) => cx.open_with_system(&os_path(&path)),
                },
                Effect::Persist(persist) => {
                    if self.settings.update(|settings| settings.apply(persist)) {
                        self.settings.save(cx.background_executor());
                    }
                }
                Effect::Notify(regions) => self.regions.notify(regions, cx),
            }
        }
        let active = self.state.removal_active();
        if active != self.removal_active {
            self.removal_active = active;
            cx.emit(HostEvent::RemovalActivityChanged(active));
        }
    }

    fn complete(&mut self, id: RequestId, reply: Option<Event>, cx: &mut Context<Self>) {
        // A cancelled request was removed from `in_flight`: its reply is dropped.
        if self.in_flight.remove(&id).is_none() {
            return;
        }
        if let Some(event) = reply {
            self.dispatch(event, cx);
        }
    }
}

/// Clears host notices: anything the user did, not asynchronous results.
fn is_user_intent(event: &Event) -> bool {
    !matches!(event, Event::Backend { .. } | Event::Computed { .. } | Event::Mapping { .. } | Event::Timer(_))
        && !matches!(
            event,
            Event::HoverSegment { .. } | Event::ClearMapHover | Event::HoverRow { .. } | Event::ClearRowHover
        )
}

/// A filesystem path from raw bytes (lossless on Unix).
pub fn os_path(bytes: &[u8]) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Path::new(std::ffi::OsStr::from_bytes(bytes)).to_path_buf()
    }
    #[cfg(not(unix))]
    {
        std::path::PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Raw bytes of a filesystem path (lossless on Unix).
pub fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

// ---------------------------------------------------------------------------------------------
// Handle for panels.

/// What panels hold: the host entity plus the region signals.
///
/// - Read: `host.state(cx)` (any `&AppState` accessor); `host.update(cx, |h, _| h.presentation())`
///   for the two `&mut` reads (presentation, row hover segment).
/// - Emit: `host.send(Event::…, cx)` from event handlers (never from `render`).
/// - Subscribe: `host.observe(Regions::LIST, cx)` in the view constructor; keep the returned
///   subscriptions in the view.
#[derive(Clone)]
pub struct Host {
    entity: Entity<AppHost>,
    regions: RegionSignals,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").field("entity", &self.entity.entity_id()).finish()
    }
}

impl Host {
    pub fn new(options: HostOptions, cx: &mut App) -> Self {
        Self::from_entity(cx.new(|cx| AppHost::new(options, cx)), cx)
    }

    pub fn from_entity(entity: Entity<AppHost>, cx: &App) -> Self {
        let regions = entity.read(cx).regions().clone();
        Self { entity, regions }
    }

    pub fn entity(&self) -> &Entity<AppHost> {
        &self.entity
    }

    pub fn read<'a>(&self, cx: &'a App) -> &'a AppHost {
        self.entity.read(cx)
    }

    pub fn state<'a>(&self, cx: &'a App) -> &'a AppState {
        self.entity.read(cx).state()
    }

    pub fn update<R>(&self, cx: &mut App, f: impl FnOnce(&mut AppHost, &mut Context<AppHost>) -> R) -> R {
        self.entity.update(cx, f)
    }

    /// Handles `event` now (from an event handler or action, never during render).
    pub fn send(&self, event: Event, cx: &mut App) {
        self.entity.update(cx, |host, cx| host.dispatch(event, cx));
    }

    pub fn signal(&self, region: Regions) -> &Entity<RegionSignal> {
        self.regions.get(region)
    }

    /// Re-renders the view `V` whenever any of `regions` is notified.
    pub fn observe<V: 'static>(&self, regions: Regions, cx: &mut Context<V>) -> Vec<Subscription> {
        crate::regions::ALL
            .iter()
            .filter(|(region, _)| regions.contains(*region))
            .map(|(region, _)| cx.observe(self.regions.get(*region), |_, _, cx| cx.notify()))
            .collect()
    }
}
