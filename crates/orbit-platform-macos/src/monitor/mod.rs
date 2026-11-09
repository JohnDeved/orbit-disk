//! FSEvents change monitor.
//!
//! Each started stream runs on its own serial dispatch queue. Callbacks normalize events there and
//! deliver batches to the sink under the stream's delivery lock; `stop` clears the sink under the
//! same lock before tearing the stream down, so once `stop` returns the sink is never called again
//! (a callback already inside the sink finishes first). The sink must therefore not call `stop`.

pub mod plan;
mod probe;
#[cfg(test)]
mod tests;

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_platform::{ChangeBatch, ChangeCheckpoint, ChangeMonitor, ChangeSink, MonitorStart, PlatformError, Result};

use crate::ffi::*;
use crate::paths;
use plan::{EventPlan, RawEvent, native_exclusion_path, prepare_batch};

pub use probe::ProbeState;

const LIVE_LATENCY: f64 = 0.6;
const STREAM_FLAGS: u32 =
    kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagWatchRoot | kFSEventStreamCreateFlagNoDefer;

/// macOS change history through FSEvents.
#[derive(Default)]
pub struct FsEventsMonitor {
    active: Option<ActiveStream>,
}

impl std::fmt::Debug for FsEventsMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsEventsMonitor").field("running", &self.active.is_some()).finish()
    }
}

struct Delivery {
    sink: ChangeSink,
    latest_event_id: u64,
    identity: String,
}

/// State shared with the stream callback (retained by FSEvents through the context).
struct Shared {
    plan: EventPlan,
    excluded: Vec<Vec<u8>>,
    raw_excluded: Vec<Vec<u8>>,
    live: AtomicBool,
    delivery: Mutex<Option<Delivery>>,
}

impl Shared {
    fn deliver(&self, prepared: plan::PreparedBatch) {
        let mut guard = self.delivery.lock().unwrap_or_else(|e| e.into_inner());
        let Some(delivery) = guard.as_mut() else { return };
        delivery.latest_event_id = delivery.latest_event_id.max(prepared.latest_event_id);
        if prepared.identity_invalidated {
            delivery.identity.clear();
            delivery.latest_event_id = 0;
        }
        let directories: Vec<PathBuf> = if prepared.full_refresh {
            vec![paths::to_path(&self.plan.root_path)]
        } else {
            prepared.directories.iter().map(|d| paths::to_path(d)).collect()
        };
        if directories.is_empty() && !prepared.full_refresh && !prepared.history_finished {
            return;
        }
        let checkpoint = ChangeCheckpoint {
            root: paths::to_path(&self.plan.root_path),
            identity: delivery.identity.clone(),
            event_id: delivery.latest_event_id,
        };
        (delivery.sink)(ChangeBatch {
            directories,
            full_refresh: prepared.full_refresh,
            history_finished: prepared.history_finished,
            checkpoint,
        });
    }
}

/// A started stream and its queue. Raw pointers are only touched from `stop`/drop.
struct ActiveStream {
    stream: FSEventStreamRef,
    queue: dispatch_queue_t,
    shared: Arc<Shared>,
}

// SAFETY: FSEvents streams scheduled on a dispatch queue may be stopped and released from any
// thread; the pointers are owned exclusively by this value.
unsafe impl Send for ActiveStream {}

impl ActiveStream {
    fn shutdown(self) {
        self.shared.live.store(false, Ordering::Release);
        // Waits for an in-flight delivery, then guarantees no further sink calls.
        let sink = self.shared.delivery.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(sink);
        // SAFETY: the stream was created, scheduled and started by us and is released once.
        unsafe {
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            dispatch_release(self.queue);
        }
    }
}

pub(crate) extern "C" fn retain_arc<T>(info: *const c_void) -> *const c_void {
    // SAFETY: `info` came from `Arc::<T>::as_ptr` and FSEvents holds it only between retain and release.
    unsafe { Arc::increment_strong_count(info.cast::<T>()) };
    info
}

pub(crate) extern "C" fn release_arc<T>(info: *const c_void) {
    // SAFETY: balances a previous `retain_arc::<T>`.
    unsafe { Arc::decrement_strong_count(info.cast::<T>()) };
}

/// Creates a (not yet started) stream for `plan` that calls `callback` with `info`.
pub(crate) fn create_stream<T>(
    plan: &EventPlan,
    callback: FSEventStreamCallback,
    info: &Arc<T>,
    since: u64,
    latency: f64,
) -> Result<FSEventStreamRef> {
    let context = FSEventStreamContext {
        version: 0,
        info: Arc::as_ptr(info).cast_mut().cast(),
        retain: Some(retain_arc::<T>),
        release: Some(release_arc::<T>),
        copy_description: None,
    };
    let unavailable = || PlatformError::os(paths::to_path(&plan.root_path), libc::EILSEQ);
    // SAFETY: the context is copied by FSEvents (retaining `info`); the path array is retained by
    // the stream and released by our CfOwned afterwards.
    let stream = unsafe {
        if let Some(relative) = &plan.relative_path {
            let watched = cf_string_array(&[relative.as_slice()]).ok_or_else(unavailable)?;
            FSEventStreamCreateRelativeToDevice(
                std::ptr::null(),
                callback,
                &context,
                plan.device,
                watched.0,
                since,
                latency,
                STREAM_FLAGS,
            )
        } else {
            let watched = cf_string_array(&[plan.canonical_root.as_slice()]).ok_or_else(unavailable)?;
            FSEventStreamCreate(std::ptr::null(), callback, &context, watched.0, since, latency, STREAM_FLAGS)
        }
    };
    if stream.is_null() {
        return Err(PlatformError::Unsupported("file change monitoring is unavailable for this folder"));
    }
    Ok(stream)
}

pub(crate) fn new_queue(label: &CStr) -> dispatch_queue_t {
    // SAFETY: a valid label; a null attribute creates a serial queue.
    unsafe { dispatch_queue_create(label.as_ptr(), std::ptr::null()) }
}

/// Copies the callback's C arrays. Paths are raw bytes (the stream does not use CF types).
///
/// # Safety
/// `paths`, `flags` and `ids` must be the arrays FSEvents passed for `count` events.
pub(crate) unsafe fn raw_events<'a>(
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) -> Vec<RawEvent<'a>> {
    if count == 0 || paths.is_null() || flags.is_null() || ids.is_null() {
        return Vec::new();
    }
    // SAFETY: guaranteed by the caller.
    let (paths, flags, ids) = unsafe {
        (
            std::slice::from_raw_parts(paths.cast::<*const c_char>(), count),
            std::slice::from_raw_parts(flags, count),
            std::slice::from_raw_parts(ids, count),
        )
    };
    (0..count)
        .map(|index| RawEvent {
            // SAFETY: each path is a NUL-terminated C string owned by FSEvents for the callback.
            path: if paths[index].is_null() { b"" } else { unsafe { CStr::from_ptr(paths[index]) }.to_bytes() },
            flags: flags[index],
            id: ids[index],
        })
        .collect()
}

extern "C" fn stream_callback(
    _stream: ConstFSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    if info.is_null() {
        return;
    }
    // SAFETY: `info` is the `Shared` retained by the stream context.
    let shared = unsafe { &*info.cast::<Shared>() };
    if !shared.live.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: arrays as passed by FSEvents for this callback.
    let events = unsafe { raw_events(count, paths, flags, ids) };
    let Some(prepared) = prepare_batch(&shared.plan, &shared.excluded, &shared.raw_excluded, &events, || {
        shared.live.load(Ordering::Acquire)
    }) else {
        return;
    };
    shared.deliver(prepared);
}

extern "C" fn deliver_initial_refresh(context: *mut c_void) {
    // SAFETY: `context` is an `Arc<Shared>` leaked by `start` for exactly this call.
    let shared = unsafe { Arc::from_raw(context.cast_const().cast::<Shared>()) };
    let prepared = plan::PreparedBatch { full_refresh: true, ..Default::default() };
    shared.deliver(prepared);
}

fn exclusion_sets(plan: &EventPlan, exclusions: &[PathBuf]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut excluded: Vec<Vec<u8>> = Vec::new();
    for exclusion in exclusions {
        let raw = paths::bytes(exclusion);
        if raw.first() != Some(&b'/') {
            continue;
        }
        let standardized = paths::standardize(raw);
        let resolved = paths::realpath(&standardized).ok();
        for path in std::iter::once(standardized).chain(resolved) {
            if !excluded.contains(&path) {
                excluded.push(path);
            }
        }
    }
    let mut native: Vec<Vec<u8>> = Vec::new();
    for path in &excluded {
        for candidate in std::iter::once(path.clone()).chain(native_exclusion_path(path)) {
            if !native.contains(&candidate) {
                native.push(candidate);
            }
        }
    }
    let mut raw: Vec<Vec<u8>> = Vec::new();
    for path in &native {
        for candidate in plan.raw_exclusion_paths(path) {
            if !raw.contains(&candidate) {
                raw.push(candidate);
            }
        }
    }
    (excluded, raw)
}

impl ChangeMonitor for FsEventsMonitor {
    fn has_history(&self) -> bool {
        true
    }

    fn capture(&self, root: &Path) -> Result<ChangeCheckpoint> {
        Ok(EventPlan::inspect(root)?.checkpoint)
    }

    fn start(
        &mut self,
        root: &Path,
        exclusions: &[PathBuf],
        since: Option<&ChangeCheckpoint>,
        sink: ChangeSink,
    ) -> Result<MonitorStart> {
        self.stop();
        let plan = EventPlan::inspect(root)?;
        // A live-only stream cannot close the baseline-scan gap; retrying full scans would loop.
        if !plan.supports_replay {
            return Err(PlatformError::Unsupported(
                "this volume does not provide reusable change history; use Rescan to update the map",
            ));
        }
        // SAFETY: plain FSEvents query.
        let current = unsafe { FSEventsGetCurrentEventId() };
        let can_replay = since.is_some_and(|since| {
            since.identity == plan.checkpoint.identity
                && since.event_id <= current
                && since.event_id != kFSEventStreamEventIdSinceNow
        });
        let since_id = match since {
            Some(since) if can_replay => since.event_id,
            _ => plan.checkpoint.event_id,
        };
        let (excluded, raw_excluded) = exclusion_sets(&plan, exclusions);
        let identity = plan.checkpoint.identity.clone();
        let shared = Arc::new(Shared {
            plan,
            excluded,
            raw_excluded,
            live: AtomicBool::new(true),
            delivery: Mutex::new(Some(Delivery { sink, latest_event_id: since_id, identity })),
        });
        let stream = match create_stream(&shared.plan, stream_callback, &shared, since_id, LIVE_LATENCY) {
            Ok(stream) => stream,
            Err(error) => {
                shared.delivery.lock().unwrap_or_else(|e| e.into_inner()).take();
                return Err(error);
            }
        };
        let queue = new_queue(c"local.orbitdisk.fsevents");
        if !can_replay {
            // Queued ahead of the stream's first callback on the same serial queue.
            let context = Arc::into_raw(Arc::clone(&shared)).cast_mut().cast::<c_void>();
            // SAFETY: valid queue; `context` is reclaimed by `deliver_initial_refresh`.
            unsafe { dispatch_async_f(queue, context, deliver_initial_refresh) };
        }
        // SAFETY: the stream was just created; the queue outlives it (released in shutdown).
        let started = unsafe {
            FSEventStreamSetDispatchQueue(stream, queue);
            FSEventStreamStart(stream) != 0
        };
        let active = ActiveStream { stream, queue, shared };
        if !started {
            active.shutdown();
            return Err(PlatformError::Unsupported("file change monitoring is unavailable for this folder"));
        }
        self.active = Some(active);
        Ok(if can_replay { MonitorStart::Replaying } else { MonitorStart::FullRefresh })
    }

    fn stop(&mut self) {
        if let Some(active) = self.active.take() {
            active.shutdown();
        }
    }

    fn history_unchanged_since(&self, checkpoint: &ChangeCheckpoint, timeout: Duration) -> bool {
        probe::validate(checkpoint, timeout)
    }
}

impl Drop for FsEventsMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}
