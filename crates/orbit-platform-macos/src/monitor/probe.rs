//! Replay validation: proves that nothing changed below a root since a saved checkpoint by
//! replaying its history into a throwaway stream. Used to reuse a saved subtree map.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use orbit_platform::ChangeCheckpoint;

use super::plan::EventPlan;
use super::{create_stream, new_queue};
use crate::ffi::*;

/// At most two probes run at once; discovery gives up rather than queue behind them.
static SLOTS: AtomicUsize = AtomicUsize::new(0);
const MAX_SLOTS: usize = 2;

/// The decision state of one probe. Only an exact history-done sentinel counts as "no change";
/// any other flag (including dropped events or root changes) is a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeState {
    active: bool,
    history_finished: bool,
    changed: bool,
    accepted: bool,
    finished: bool,
}

impl Default for ProbeState {
    fn default() -> Self {
        Self { active: true, history_finished: false, changed: false, accepted: false, finished: false }
    }
}

impl ProbeState {
    pub fn receive(&mut self, flags: &[u32]) {
        if !self.active {
            return;
        }
        for &flag in flags {
            if flag == kFSEventStreamEventFlagHistoryDone {
                self.history_finished = true;
            } else {
                self.changed = true;
            }
        }
    }

    /// The stream was flushed after the sentinel; `root_valid` re-checks the root identity.
    pub fn finish(&mut self, root_valid: bool) {
        if !self.active {
            return;
        }
        self.accepted = root_valid && self.history_finished && !self.changed;
        self.active = false;
        self.finished = true;
    }

    /// The caller stopped waiting; later events and results are ignored.
    pub fn expire(&mut self) {
        self.active = false;
    }

    pub fn accepted(&self) -> bool {
        self.finished && self.accepted
    }

    pub fn history_finished(&self) -> bool {
        self.history_finished
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

struct Probe {
    state: Mutex<ProbeState>,
    changed: Condvar,
}

impl Probe {
    fn update(&self, change: impl FnOnce(&mut ProbeState)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        change(&mut state);
        self.changed.notify_all();
    }

    /// Waits until `ready` holds or `deadline` passes; returns the state at that point.
    fn wait_until(&self, deadline: Instant, ready: impl Fn(&ProbeState) -> bool) -> ProbeState {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while !ready(&state) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = self.changed.wait_timeout(state, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
        *state
    }
}

extern "C" fn probe_callback(
    _stream: ConstFSEventStreamRef,
    info: *mut c_void,
    count: usize,
    _paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    if info.is_null() || flags.is_null() {
        return;
    }
    // SAFETY: `info` is the `Probe` retained by the stream context; `flags` has `count` entries.
    let (probe, flags) = unsafe { (&*info.cast::<Probe>(), std::slice::from_raw_parts(flags, count)) };
    probe.update(|state| state.receive(flags));
}

struct Slot;

impl Slot {
    fn acquire() -> Option<Self> {
        SLOTS.try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_SLOTS).then_some(n + 1)).ok().map(|_| Slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        SLOTS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn validate(checkpoint: &ChangeCheckpoint, timeout: Duration) -> bool {
    let Ok(plan) = EventPlan::inspect(&checkpoint.root) else { return false };
    let since = checkpoint.event_id;
    if timeout.is_zero()
        || plan.relative_path.is_none()
        || !plan.supports_replay
        || checkpoint.identity != plan.checkpoint.identity
        || since == kFSEventStreamEventIdSinceNow
        || since > plan.checkpoint.event_id
    {
        return false;
    }
    let Some(slot) = Slot::acquire() else { return false };
    let probe = Arc::new(Probe { state: Mutex::new(ProbeState::default()), changed: Condvar::new() });
    let deadline = Instant::now() + timeout;
    let worker = Arc::clone(&probe);
    // Stream creation and flushing may block; the caller gives up after its small budget.
    let spawned = std::thread::Builder::new().name("orbit-history-probe".into()).spawn(move || {
        let _slot = slot;
        run_probe(&plan, since, &worker, deadline);
        worker.update(|state| state.finish(false));
    });
    if spawned.is_err() {
        return false;
    }
    let state = probe.wait_until(deadline, |state| !state.is_active());
    probe.update(ProbeState::expire);
    state.accepted()
}

fn run_probe(plan: &EventPlan, since: u64, probe: &Arc<Probe>, deadline: Instant) {
    if !probe.state.lock().unwrap_or_else(|e| e.into_inner()).is_active() {
        return;
    }
    let Ok(stream) = create_stream(plan, probe_callback, probe, since, 0.0) else { return };
    let queue = new_queue(c"local.orbitdisk.history-probe");
    // SAFETY: the stream and queue were created above and are released exactly once below.
    unsafe {
        FSEventStreamSetDispatchQueue(stream, queue);
        if FSEventStreamStart(stream) != 0 {
            let state = probe.wait_until(deadline, |state| state.history_finished() || !state.is_active());
            if state.history_finished() && state.is_active() {
                // A history sentinel alone does not prove latency-buffered live events arrived.
                FSEventStreamFlushSync(stream);
                let valid = plan.root_identity_is_valid();
                probe.update(|state| state.finish(valid));
            }
            FSEventStreamStop(stream);
        }
        FSEventStreamInvalidate(stream);
        FSEventStreamRelease(stream);
        dispatch_release(queue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_flushed_change_free_history_is_accepted() {
        let created = kFSEventStreamEventFlagItemCreated;
        let done = kFSEventStreamEventFlagHistoryDone;
        let decide = |flags: &[u32], flushed: bool, root_valid: bool, expired: bool| {
            let mut state = ProbeState::default();
            if expired {
                state.expire();
            }
            state.receive(flags);
            if flushed {
                state.finish(root_valid);
            }
            state.accepted()
        };
        assert!(decide(&[done], true, true, false));
        assert!(!decide(&[created, done], true, true, false), "a replayed change was ignored");
        assert!(
            !decide(&[done | kFSEventStreamEventFlagRootChanged], true, true, false),
            "combined flags hid a change"
        );
        assert!(!decide(&[], true, true, false), "no history sentinel");
        assert!(!decide(&[done], false, true, false), "not flushed");
        assert!(!decide(&[done], true, false, false), "root replaced");
        assert!(!decide(&[done], true, true, true), "expired probe accepted");
    }
}
