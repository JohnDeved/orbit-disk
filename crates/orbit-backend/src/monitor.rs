//! Change monitoring of a saved map (Swift `beginMonitoring` + `DiskChangeMonitor`).
//!
//! The platform monitor delivers batches on its own thread; the sink only merges them into a
//! pending set. A forwarder thread waits for the debounce window, then hands the merged
//! directories to the app as one [`MappingEvent::Changes`]. However many batches arrive, at most
//! one event is pending per debounce window.
//!
//! Every forwarded batch is kept in a [`Ledger`] with its checkpoint. A refresh advances the saved
//! map's change-history position only past the batches whose folders it re-listed, so a batch
//! that reached the app after the refresh took its folders is replayed after a crash instead of
//! being skipped.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use orbit_app_state::{Event, MappingEvent, Notifier, SessionId};
use orbit_platform::{ChangeBatch, ChangeCheckpoint, ChangeMonitor};
use parking_lot::{Condvar, Mutex};

/// Forwarded batches beyond this many are merged (oldest first).
const LEDGER_LIMIT: usize = 1024;

/// One forwarded batch.
struct Delivered {
    directories: Vec<Vec<u8>>,
    subtrees: Vec<Vec<u8>>,
    full_refresh: bool,
    checkpoint: ChangeCheckpoint,
}

/// Batches handed to the app and not yet persisted by a refresh, oldest first.
#[derive(Default)]
pub(crate) struct Ledger {
    batches: VecDeque<Delivered>,
}

impl Ledger {
    fn push(&mut self, batch: Delivered) {
        self.batches.push_back(batch);
        if self.batches.len() > LEDGER_LIMIT
            && let (Some(oldest), Some(next)) = (self.batches.pop_front(), self.batches.front_mut())
        {
            next.directories.extend(oldest.directories);
            next.subtrees.extend(oldest.subtrees);
            next.full_refresh |= oldest.full_refresh;
        }
    }

    pub fn clear(&mut self) {
        self.batches.clear();
    }

    /// The checkpoint a refresh may persist that re-lists `listings` and reconciles `subtrees`
    /// recursively: that of the newest batch such that it and every older batch are covered.
    /// A batch's listing is covered by the same listing or by a subtree containing it; its
    /// subtree only by a subtree containing it. Covered batches are dropped.
    pub fn settle(&mut self, listings: &[Vec<u8>], subtrees: &[Vec<u8>]) -> Option<ChangeCheckpoint> {
        let listings: HashSet<&[u8]> = listings.iter().map(Vec::as_slice).collect();
        let subtrees: HashSet<&[u8]> = subtrees.iter().map(Vec::as_slice).collect();
        let in_subtree = |dir: &[u8]| {
            let mut path = Some(dir);
            while let Some(current) = path {
                if subtrees.contains(current) {
                    return true;
                }
                path = orbit_core::paths::parent(current);
            }
            false
        };
        let mut checkpoint = None;
        while let Some(batch) = self.batches.front() {
            let covered = !batch.full_refresh
                && batch.directories.iter().all(|d| listings.contains(d.as_slice()) || in_subtree(d))
                && batch.subtrees.iter().all(|d| in_subtree(d));
            if !covered {
                break;
            }
            checkpoint = self.batches.pop_front().map(|b| b.checkpoint);
        }
        checkpoint
    }
}

#[derive(Default)]
struct Pending {
    directories: BTreeSet<Vec<u8>>,
    subtrees: BTreeSet<Vec<u8>>,
    full_refresh: bool,
    checkpoint: Option<ChangeCheckpoint>,
    dirty: bool,
    stopped: bool,
}

#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    wake: Condvar,
}

impl Shared {
    fn add(&self, batch: ChangeBatch) {
        let mut pending = self.pending.lock();
        let bytes = |p: &PathBuf| p.as_os_str().as_encoded_bytes().to_vec();
        pending.directories.extend(batch.directories.iter().map(bytes));
        pending.subtrees.extend(batch.subtrees.iter().map(bytes));
        pending.full_refresh |= batch.full_refresh;
        pending.checkpoint = Some(batch.checkpoint);
        pending.dirty = true;
        self.wake.notify_all();
    }

    fn stop(&self) {
        self.pending.lock().stopped = true;
        self.wake.notify_all();
    }
}

/// A started monitor and its forwarder. [`MonitorRun::stop`] stops both.
pub(crate) struct MonitorRun<M: ChangeMonitor> {
    monitor: M,
    shared: Arc<Shared>,
    forwarder: Option<JoinHandle<()>>,
}

impl<M: ChangeMonitor> MonitorRun<M> {
    /// Starts `monitor` on `root`. Fails when the platform cannot watch it (the app then reports
    /// that automatic updates are unavailable).
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        mut monitor: M,
        root: PathBuf,
        exclusions: &[PathBuf],
        since: Option<&ChangeCheckpoint>,
        session: SessionId,
        notify: Notifier,
        ledger: Arc<Mutex<Ledger>>,
        debounce: Duration,
    ) -> orbit_platform::Result<Self> {
        let shared = Arc::new(Shared::default());
        let sink = Arc::clone(&shared);
        monitor.start(&root, exclusions, since, Box::new(move |batch| sink.add(batch)))?;
        let forward = Arc::clone(&shared);
        let forwarder = std::thread::Builder::new()
            .name("orbit-change-forwarder".into())
            .spawn(move || forward_changes(&forward, session, &notify, &ledger, debounce));
        match forwarder {
            Ok(forwarder) => Ok(Self { monitor, shared, forwarder: Some(forwarder) }),
            Err(error) => {
                monitor.stop();
                Err(orbit_platform::PlatformError::os(root, error.raw_os_error().unwrap_or(0)))
            }
        }
    }

    pub fn stop(mut self) {
        self.monitor.stop();
        self.shared.stop();
        if let Some(forwarder) = self.forwarder.take() {
            let _ = forwarder.join();
        }
    }
}

fn forward_changes(shared: &Shared, session: SessionId, notify: &Notifier, ledger: &Mutex<Ledger>, debounce: Duration) {
    loop {
        let mut pending = shared.pending.lock();
        while !pending.dirty && !pending.stopped {
            shared.wake.wait(&mut pending);
        }
        // Later batches inside the window merge into this one.
        let deadline = Instant::now() + debounce;
        while !pending.stopped && !shared.wake.wait_until(&mut pending, deadline).timed_out() {}
        if pending.stopped {
            return;
        }
        let directories: Vec<Vec<u8>> = std::mem::take(&mut pending.directories).into_iter().collect();
        let subtrees: Vec<Vec<u8>> = std::mem::take(&mut pending.subtrees).into_iter().collect();
        let full_refresh = std::mem::take(&mut pending.full_refresh);
        let batch_checkpoint = pending.checkpoint.take();
        pending.dirty = false;
        drop(pending);
        if let Some(checkpoint) = batch_checkpoint {
            // Recorded before the app can see the batch, so no refresh can skip past it.
            ledger.lock().push(Delivered {
                directories: directories.clone(),
                subtrees: subtrees.clone(),
                full_refresh,
                checkpoint,
            });
        }
        if full_refresh || !directories.is_empty() || !subtrees.is_empty() {
            notify(Event::Mapping { session, event: MappingEvent::Changes { directories, subtrees, full_refresh } });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(directories: &[&str], full_refresh: bool, event_id: u64) -> Delivered {
        Delivered {
            directories: paths(directories),
            subtrees: Vec::new(),
            full_refresh,
            checkpoint: ChangeCheckpoint { root: "/r".into(), identity: "h".into(), event_id },
        }
    }

    fn paths(list: &[&str]) -> Vec<Vec<u8>> {
        list.iter().map(|d| d.as_bytes().to_vec()).collect()
    }

    /// A refresh persists the history position only through batches whose folders it re-listed;
    /// a batch delivered after the refresh took its folders keeps the position before it.
    #[test]
    fn refresh_checkpoint_never_skips_an_unrefreshed_batch() {
        let mut ledger = Ledger::default();
        ledger.push(batch(&["/r/a"], false, 1));
        ledger.push(batch(&[], false, 2));
        ledger.push(batch(&["/r/b/c"], false, 3));
        ledger.push(batch(&["/r/d"], false, 4));
        let settled = ledger.settle(&[], &paths(&["/r/a", "/r/b"]));
        assert_eq!(settled.map(|c| c.event_id), Some(3), "subfolders of refreshed folders are covered");
        assert_eq!(ledger.settle(&[], &paths(&["/r/a"])), None, "the /r/d batch was not refreshed");
        assert_eq!(ledger.settle(&[], &paths(&["/r/d"])).map(|c| c.event_id), Some(4));
        ledger.push(batch(&["/r/e"], true, 5));
        assert_eq!(ledger.settle(&[], &paths(&["/r"])), None, "a full refresh re-maps instead");
    }

    /// Deferred (off-screen) changes hold the position back while visible ones are refreshed;
    /// the deferred refresh, which also takes everything delivered meanwhile, releases it.
    #[test]
    fn deferred_batches_hold_the_checkpoint_until_their_refresh() {
        let mut ledger = Ledger::default();
        ledger.push(batch(&["/r/elsewhere"], false, 1));
        ledger.push(batch(&["/r/shown/a"], false, 2));
        assert_eq!(ledger.settle(&[], &paths(&["/r/shown"])), None, "the deferred batch is older");
        ledger.push(batch(&["/r/shown/b"], false, 3));
        ledger.push(batch(&["/r/elsewhere/deep"], false, 4));
        let settled = ledger.settle(&[], &paths(&["/r/elsewhere", "/r/shown"]));
        assert_eq!(settled.map(|c| c.event_id), Some(4));
        assert_eq!(ledger.settle(&[], &paths(&["/r"])), None, "nothing is left to persist");
    }

    /// A refreshed listing covers only that listing: a change below it, or one whose history was
    /// coalesced (a subtree), still needs its own refresh before the position may pass it.
    #[test]
    fn a_listing_covers_only_itself() {
        let mut ledger = Ledger::default();
        ledger.push(batch(&["/r"], false, 1));
        ledger.push(batch(&["/r/a"], false, 2));
        assert_eq!(ledger.settle(&paths(&["/r"]), &[]).map(|c| c.event_id), Some(1));
        assert_eq!(ledger.settle(&paths(&["/r"]), &[]), None, "/r/a was not re-listed");
        assert_eq!(ledger.settle(&paths(&["/r/a"]), &[]).map(|c| c.event_id), Some(2));
        ledger.push(Delivered { subtrees: paths(&["/r/b"]), ..batch(&[], false, 3) });
        assert_eq!(ledger.settle(&paths(&["/r/b"]), &[]), None, "a listing does not reconcile a subtree");
        assert_eq!(ledger.settle(&[], &paths(&["/r"])).map(|c| c.event_id), Some(3));
    }
}
