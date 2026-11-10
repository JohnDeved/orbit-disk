//! No change history. A map from an earlier launch gets one full refresh when monitoring starts
//! (changes made while the app was not running are unknown). A map made in this process is
//! current, and without live events nothing could keep it current, so monitoring reports itself
//! unavailable instead of asking for another full refresh: that answer used to make every
//! publication start the next rescan, forever.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_platform::{ChangeBatch, ChangeCheckpoint, ChangeMonitor, ChangeSink, MonitorStart, PlatformError, Result};

/// Identity of checkpoints from a platform without history; never replayable. It names this
/// process, so a checkpoint captured in it (a map made in this run) is recognised.
fn identity() -> &'static str {
    static IDENTITY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    IDENTITY.get_or_init(|| {
        let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        format!("generic:no-history:{}:{started}", std::process::id())
    })
}

type SharedSink = Arc<Mutex<Option<ChangeSink>>>;

#[derive(Default)]
pub struct GenericMonitor {
    sink: Option<SharedSink>,
}

impl std::fmt::Debug for GenericMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenericMonitor").field("running", &self.sink.is_some()).finish()
    }
}

fn checkpoint(root: &Path) -> ChangeCheckpoint {
    ChangeCheckpoint { root: root.to_path_buf(), identity: identity().into(), event_id: 0 }
}

impl ChangeMonitor for GenericMonitor {
    fn has_history(&self) -> bool {
        false
    }

    fn capture(&self, root: &Path) -> Result<ChangeCheckpoint> {
        Ok(checkpoint(root))
    }

    fn start(
        &mut self,
        root: &Path,
        _exclusions: &[PathBuf],
        since: Option<&ChangeCheckpoint>,
        sink: ChangeSink,
    ) -> Result<MonitorStart> {
        self.stop();
        orbit_platform::posix::open_root(root)?;
        if since.is_some_and(|since| since.identity == identity()) {
            return Err(PlatformError::Unsupported(
                "this platform reports no folder changes; use Refresh to update the map",
            ));
        }
        let shared: SharedSink = Arc::new(Mutex::new(Some(sink)));
        let worker = Arc::clone(&shared);
        let batch = ChangeBatch {
            directories: Vec::new(),
            subtrees: vec![root.to_path_buf()],
            full_refresh: true,
            history_finished: true,
            checkpoint: checkpoint(root),
        };
        std::thread::Builder::new()
            .name("orbit-generic-monitor".into())
            .spawn(move || {
                // Delivery happens under the lock that `stop` takes, so it never follows `stop`.
                if let Some(sink) = worker.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    sink(batch);
                }
            })
            .map_err(|e| PlatformError::os(root, e.raw_os_error().unwrap_or(libc::EAGAIN)))?;
        self.sink = Some(shared);
        Ok(MonitorStart::FullRefresh)
    }

    fn stop(&mut self) {
        if let Some(shared) = self.sink.take() {
            shared.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
    }
}

impl Drop for GenericMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map made in this process is not rescanned again when it is monitored; one from an
    /// earlier launch (or without a checkpoint) gets the single full refresh.
    #[test]
    fn only_maps_from_an_earlier_launch_are_refreshed_fully() {
        let dir = std::env::temp_dir();
        let mut monitor = GenericMonitor::default();
        let current = monitor.capture(&dir).unwrap();
        let started = monitor.start(&dir, &[], Some(&current), Box::new(|_| {}));
        assert!(matches!(started, Err(PlatformError::Unsupported(_))), "{started:?}");
        let earlier = ChangeCheckpoint { identity: "generic:no-history:1:2".into(), ..current };
        let (tx, rx) = std::sync::mpsc::channel();
        let started = monitor.start(&dir, &[], Some(&earlier), Box::new(move |batch| tx.send(batch).unwrap()));
        assert!(matches!(started, Ok(MonitorStart::FullRefresh)));
        assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().full_refresh);
        monitor.stop();
    }
}
