//! The mapping worker of one session (Swift `scanResolved` worker): journal recovery of the
//! saved map, saved-map reuse, the [`MappingSession`], progress forwarding and publication.
//!
//! ```text
//! saved map of the root?  -> SavedMaps::recover (removal journal; live removal -> RemovalReplay)
//! !force && saved && no stage           -> Published { reused }            (no traversal)
//! !force && saved && stage && !keep_map -> SavedAvailable, then resume the stage
//! MappingSession::start                 -> DiscoveryStarted
//!   every change (at most every `progress_interval`): Progress, PreviewChanged
//!   outcome: Published | Failed (a stop is silent)
//! published map: prepare the search accelerator -> SearchReady
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_app_state::backend::MappingRequest;
use orbit_app_state::{Event, MappingEvent, Notifier};
use orbit_core::recovery::SavedRecovery;
use orbit_core::saved::SEARCH_WARMUP_DELAY;
use orbit_core::schema::SnapshotInfo as CoreSnapshot;
use orbit_core::session::{MappingSession, SessionOptions, SessionOutcome};
use orbit_platform::{Cancellation, FileIdentity, Platform};

use crate::Inner;
use crate::convert::{self, message};
use crate::registry::Entry;

/// Longest wait for a change before the outcome is checked again.
const CHANGE_WAIT: Duration = Duration::from_millis(250);

struct Emitter {
    session: orbit_app_state::SessionId,
    notify: Notifier,
}

impl Emitter {
    fn emit(&self, event: MappingEvent) {
        (self.notify)(Event::Mapping { session: self.session, event });
    }
}

pub(crate) fn run<P: Platform>(
    inner: &Arc<Inner<P>>,
    entry: &Arc<Entry<P>>,
    request: &MappingRequest,
    notify: Notifier,
    replaced: Vec<Arc<Entry<P>>>,
) {
    // The previous session of this root must release its stage before this one opens it.
    for previous in replaced {
        previous.shut_down();
    }
    let out = Emitter { session: request.session, notify };
    let cancel = entry.cancel.clone();
    let root = request.root.to_vec();
    let identity = match inner.root_identity(&root) {
        Ok(identity) => identity,
        Err(error) => return out.emit(MappingEvent::Failed(error)),
    };
    if request.identity.is_some_and(|expected| expected != identity) {
        return out.emit(MappingEvent::Failed("The mapped folder changed.".into()));
    }
    let existing = if request.force { None } else { recovered_saved_map(inner, &root, identity, &cancel, &out) };
    if cancel.is_cancelled() {
        return;
    }
    let has_stage = inner.has_stage(&root, identity);
    let upgrade = existing.as_ref().is_some_and(convert::needs_boot_upgrade);
    if let Some(saved) = existing.as_ref().filter(|_| !upgrade) {
        if !has_stage {
            publish(inner, entry, saved, true, &out, &cancel);
            return;
        }
        if !request.keep_map
            && let Ok((info, _)) = inner.describe(saved)
        {
            entry.set_saved(saved.clone());
            out.emit(MappingEvent::SavedAvailable(info));
        }
    }
    let preferred = Some(request.route.clone()).filter(|route| orbit_core::paths::is_within(route, &root));
    let options = SessionOptions {
        workers: inner.options.workers,
        preferred,
        home: Some(inner.options.home.clone()),
        reuse_saved_maps: !request.force || upgrade,
        prepare_search: false,
        hooks: inner.options.hooks.clone(),
        busy_retry: inner.options.busy_retry,
        current_path_interval: inner.options.current_path_interval,
        ..SessionOptions::default()
    };
    let session = match MappingSession::start(Arc::clone(&inner.platform), inner.maps.paths().clone(), &root, options) {
        Ok(session) => Arc::new(session),
        Err(error) => {
            if !cancel.is_cancelled() && !error.is_cancelled() {
                out.emit(MappingEvent::Failed(message(&error)));
            }
            return;
        }
    };
    if !entry.set_live(Arc::clone(&session)) {
        return;
    }
    out.emit(MappingEvent::DiscoveryStarted);
    forward(inner, entry, &session, &out, &cancel);
}

/// The saved map of `root`, brought up to date with the removal journal, if it still describes
/// the folder now at `root`.
fn recovered_saved_map<P: Platform>(
    inner: &Inner<P>,
    root: &[u8],
    identity: FileIdentity,
    cancel: &Cancellation,
    out: &Emitter,
) -> Option<CoreSnapshot> {
    let saved = inner.maps.snapshot(root)?;
    let current = match inner.maps.recover(&saved, cancel) {
        Ok((SavedRecovery::UpToDate(_), _)) => Some(saved),
        Ok((SavedRecovery::Published(_), published)) => published.or_else(|| inner.maps.snapshot(root)),
        Ok((SavedRecovery::Live(plan), _)) => {
            // A removal is still running in this folder: hide its items until it is reconciled.
            out.emit(MappingEvent::RemovalReplay {
                removals: plan.removals,
                affected_parents: plan.affected_parents.into_iter().collect(),
                full_refresh: plan.overlay_overflow,
            });
            Some(saved)
        }
        // The saved map cannot be repaired: the new mapping is the full refresh.
        Ok((SavedRecovery::FullRefresh(_), _)) => None,
        Err(error) if error.is_cancelled() => None,
        // Kept as is; the next refresh folds the journal history in.
        Err(_) => Some(saved),
    }?;
    let (info, _) = inner.describe(&current).ok()?;
    (info.root_identity == Some(identity)).then_some(current)
}

fn forward<P: Platform>(
    inner: &Arc<Inner<P>>,
    entry: &Arc<Entry<P>>,
    session: &Arc<MappingSession<P>>,
    out: &Emitter,
    cancel: &Cancellation,
) {
    let interval = inner.options.progress_interval;
    let mut preview_revision = None;
    let mut last_progress = None;
    loop {
        match session.outcome() {
            SessionOutcome::Running => {}
            SessionOutcome::Published(saved) => {
                publish(inner, entry, &saved, false, out, cancel);
                return;
            }
            SessionOutcome::Failed(error) => {
                if !cancel.is_cancelled() {
                    out.emit(MappingEvent::Failed(message(&error)));
                }
                return;
            }
            SessionOutcome::Cancelled => return,
        }
        if cancel.is_cancelled() {
            // A stop: the stage stays resumable and the app keeps what it shows.
            session.cancel();
            return;
        }
        let emitted = Instant::now();
        let revision = session.revision();
        if preview_revision != Some(revision) {
            preview_revision = Some(revision);
            out.emit(MappingEvent::PreviewChanged { revision, search_ready: true });
        }
        let snapshot = convert::progress(
            &session.progress(),
            &session.discovery_progress(),
            session.current_path().as_deref().unwrap_or(session.root()),
        );
        if last_progress.as_ref() != Some(&snapshot) {
            out.emit(MappingEvent::Progress(snapshot.clone()));
            last_progress = Some(snapshot);
        }
        session.changes().wait(revision, CHANGE_WAIT);
        // Throttle: however fast changes arrive, at most one round of events per interval.
        let next = emitted + interval;
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        }
    }
}

/// Reports a saved map (new or reused) and prepares its search accelerator.
fn publish<P: Platform>(
    inner: &Arc<Inner<P>>,
    entry: &Entry<P>,
    saved: &CoreSnapshot,
    reused: bool,
    out: &Emitter,
    cancel: &Cancellation,
) {
    let (snapshot, root_file_count) = match inner.describe(saved) {
        Ok(described) => described,
        Err(error) => {
            entry.finish(saved.clone());
            return out.emit(MappingEvent::Failed(error));
        }
    };
    entry.finish(saved.clone());
    out.emit(MappingEvent::Published { snapshot, reused, root_file_count });
    // Give the first folder read a head start (Swift `warmSearch`).
    let deadline = Instant::now() + SEARCH_WARMUP_DELAY;
    while Instant::now() < deadline {
        if cancel.is_cancelled() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Ok(true) = inner.maps.prepare_search(saved, cancel) {
        out.emit(MappingEvent::SearchReady);
    }
}
