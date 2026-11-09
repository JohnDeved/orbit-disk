//! Search-accelerator preparation for refreshed saved maps.
//!
//! A refresh replaces the saved map's file, so its prepared search index no longer applies and a
//! new one would be built over every row. Rebuilding after each refresh of a map that keeps
//! changing (a mapped `/Library` refreshes every few seconds while it is shown) costs far more
//! than the refresh itself. Instead one thread builds the index of the newest refreshed map once
//! the map has stayed unchanged for [`REFRESH_WARM_DELAY`], or right away when a search needs it
//! (until then searches run in SQL). The thread sleeps on a condvar: no polling.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use orbit_core::saved::SavedMaps;
use orbit_core::schema::SnapshotInfo as CoreSnapshot;
use orbit_platform::{Cancellation, Platform};
use parking_lot::{Condvar, Mutex};

/// How long a refreshed map must stay unchanged before its search index is rebuilt.
pub(crate) const REFRESH_WARM_DELAY: Duration = Duration::from_secs(60);

struct Job {
    snapshot: CoreSnapshot,
    due: Instant,
}

#[derive(Default)]
struct State {
    pending: Option<Job>,
    /// The build in progress: its saved map and cancellation.
    running: Option<(CoreSnapshot, Cancellation)>,
    stopped: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// The warming thread (started on first use) and its queue of one job.
pub(crate) struct SearchWarmer<P: Platform> {
    maps: Arc<SavedMaps<P>>,
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl<P: Platform> SearchWarmer<P> {
    pub fn new(maps: Arc<SavedMaps<P>>) -> Self {
        Self { maps, shared: Arc::default(), thread: Mutex::new(None) }
    }

    /// Prepares `snapshot`'s search index after `delay`. A newer map of the same file replaces a
    /// pending job and cancels a build of the older one (the file is gone); asking again for the
    /// same map only moves its deadline earlier.
    pub fn schedule(&self, snapshot: CoreSnapshot, delay: Duration) {
        let due = Instant::now() + delay;
        {
            let mut state = self.shared.state.lock();
            if state.stopped {
                return;
            }
            if let Some((running, cancel)) = &state.running {
                if running.id == snapshot.id && running.database == snapshot.database {
                    return;
                }
                if running.database == snapshot.database {
                    cancel.cancel();
                }
            }
            let due = match &state.pending {
                Some(job) if job.snapshot.id == snapshot.id && job.snapshot.database == snapshot.database => {
                    job.due.min(due)
                }
                _ => due,
            };
            state.pending = Some(Job { snapshot, due });
        }
        self.shared.wake.notify_all();
        self.start_thread();
    }

    fn start_thread(&self) {
        let mut thread = self.thread.lock();
        if thread.is_some() {
            return;
        }
        let (maps, shared) = (Arc::clone(&self.maps), Arc::clone(&self.shared));
        *thread =
            std::thread::Builder::new().name("orbit-search-warmup".into()).spawn(move || run(&maps, &shared)).ok();
    }

    /// Cancels pending and running work and waits for the thread.
    pub fn stop(&self) {
        {
            let mut state = self.shared.state.lock();
            state.stopped = true;
            state.pending = None;
            if let Some((_, cancel)) = &state.running {
                cancel.cancel();
            }
        }
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.lock().take() {
            let _ = thread.join();
        }
    }
}

impl<P: Platform> Drop for SearchWarmer<P> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run<P: Platform>(maps: &SavedMaps<P>, shared: &Shared) {
    let mut state = shared.state.lock();
    loop {
        if state.stopped {
            return;
        }
        let Some(due) = state.pending.as_ref().map(|job| job.due) else {
            shared.wake.wait(&mut state);
            continue;
        };
        if Instant::now() < due {
            shared.wake.wait_until(&mut state, due);
            continue;
        }
        let Some(job) = state.pending.take() else { continue };
        let cancel = Cancellation::new();
        state.running = Some((job.snapshot.clone(), cancel.clone()));
        drop(state);
        // A replaced or cancelled map fails harmlessly; the next refresh schedules again.
        let _ = maps.prepare_search(&job.snapshot, &cancel);
        state = shared.state.lock();
        state.running = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_core::store::IndexPaths;

    fn snapshot(database: &std::path::Path, id: &str) -> CoreSnapshot {
        CoreSnapshot {
            id: id.into(),
            root_path: b"/r".to_vec(),
            database: database.to_path_buf(),
            scanned_at: 0.0,
            elapsed: 0.0,
            skipped: 0,
            issues: Vec::new(),
            event_id: 0,
            event_identity: String::new(),
        }
    }

    #[allow(clippy::default_constructed_unit_structs)] // `Native` is a unit struct only on macOS.
    fn warmer(dir: &std::path::Path) -> SearchWarmer<orbit_native::Native> {
        let maps = SavedMaps::new(Arc::new(orbit_native::Native::default()), IndexPaths::under(dir));
        SearchWarmer::new(Arc::new(maps))
    }

    /// Refreshes coalesce into one pending job whose deadline only moves earlier for the same
    /// map; a newer map replaces it. Nothing runs before the deadline.
    #[test]
    fn jobs_coalesce_and_wait_for_their_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let warmer = warmer(dir.path());
        let database = dir.path().join("map.sqlite");
        warmer.schedule(snapshot(&database, "A"), Duration::from_secs(3600));
        warmer.schedule(snapshot(&database, "A"), Duration::from_secs(7200));
        let first = warmer.shared.state.lock().pending.as_ref().map(|job| job.due).unwrap();
        assert!(first <= Instant::now() + Duration::from_secs(3600), "a later request keeps the deadline");
        warmer.schedule(snapshot(&database, "B"), Duration::from_secs(7200));
        {
            let state = warmer.shared.state.lock();
            let job = state.pending.as_ref().unwrap();
            assert_eq!(job.snapshot.id, "B", "the newer map replaces the older one");
            assert!(state.running.is_none(), "nothing runs before the deadline");
        }
        warmer.stop();
        assert!(warmer.shared.state.lock().pending.is_none());
    }
}
