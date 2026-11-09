//! Per-session state and the snapshot id table shared by the backend's threads.
//!
//! Locks here are held only to copy a handle in or out; no engine call runs under them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use orbit_core::schema::SnapshotInfo as CoreSnapshot;
use orbit_core::session::MappingSession;
use orbit_platform::{Cancellation, Platform};
use parking_lot::Mutex;

use crate::monitor::{Ledger, MonitorRun};

/// One app-state mapping session: its running [`MappingSession`] (while mapping), the saved map
/// it published or reused, and its change monitor.
pub(crate) struct Entry<P: Platform> {
    pub root: Vec<u8>,
    /// Stops the mapping worker (recovery, mapping, search preparation).
    pub cancel: Cancellation,
    state: Mutex<EntryState<P>>,
    monitor: Mutex<Option<MonitorRun<P::Monitor>>>,
    /// Monitor batches handed to the app and not yet persisted by a refresh.
    pub ledger: Arc<Mutex<Ledger>>,
}

struct EntryState<P: Platform> {
    live: Option<Arc<MappingSession<P>>>,
    saved: Option<CoreSnapshot>,
    worker: Option<JoinHandle<()>>,
}

impl<P: Platform> Entry<P> {
    pub fn new(root: Vec<u8>) -> Self {
        Self {
            root,
            cancel: Cancellation::new(),
            state: Mutex::new(EntryState { live: None, saved: None, worker: None }),
            monitor: Mutex::new(None),
            ledger: Arc::default(),
        }
    }

    pub fn live(&self) -> Option<Arc<MappingSession<P>>> {
        self.state.lock().live.clone()
    }

    pub fn saved(&self) -> Option<CoreSnapshot> {
        self.state.lock().saved.clone()
    }

    /// Installs the running session. Returns false (and cancels it) when the entry was stopped
    /// meanwhile.
    pub fn set_live(&self, session: Arc<MappingSession<P>>) -> bool {
        self.state.lock().live = Some(Arc::clone(&session));
        if self.cancel.is_cancelled() {
            session.cancel();
            return false;
        }
        true
    }

    /// The session published: views switch to the saved map and the live inventory is released
    /// once the last reader drops its handle.
    pub fn finish(&self, saved: CoreSnapshot) {
        let mut state = self.state.lock();
        state.saved = Some(saved);
        state.live = None;
    }

    pub fn set_saved(&self, saved: CoreSnapshot) {
        self.state.lock().saved = Some(saved);
    }

    pub fn set_worker(&self, worker: JoinHandle<()>) {
        self.state.lock().worker = Some(worker);
    }

    /// Stops mapping (the stage stays resumable) and monitoring. Does not block.
    pub fn stop(&self) {
        self.cancel.cancel();
        if let Some(live) = self.live() {
            live.cancel();
        }
    }

    /// Stops everything and waits for the worker and the mapping threads, so the stage lock is
    /// free for the next session of the same root.
    pub fn shut_down(&self) {
        self.stop();
        self.stop_monitor();
        let worker = self.state.lock().worker.take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        let live = self.state.lock().live.take();
        drop(live);
    }

    pub fn replace_monitor(&self, run: Option<MonitorRun<P::Monitor>>) {
        let previous = std::mem::replace(&mut *self.monitor.lock(), run);
        if let Some(previous) = previous {
            previous.stop();
        }
    }

    pub fn stop_monitor(&self) {
        self.replace_monitor(None);
    }
}

/// App-state snapshot ids (`u64`) for orbit-core snapshots. Every publication gets a new id.
#[derive(Default)]
pub(crate) struct Snapshots {
    next: u64,
    by_id: HashMap<u64, CoreSnapshot>,
    ids: HashMap<(PathBuf, String), u64>,
}

impl Snapshots {
    pub fn register(&mut self, snapshot: &CoreSnapshot) -> u64 {
        let key = (snapshot.database.clone(), snapshot.id.clone());
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        self.next += 1;
        let id = self.next;
        self.ids.insert(key, id);
        self.by_id.insert(id, snapshot.clone());
        id
    }

    pub fn get(&self, id: u64) -> Option<CoreSnapshot> {
        self.by_id.get(&id).cloned()
    }
}
