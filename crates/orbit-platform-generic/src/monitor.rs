//! No change history: every start asks for one full refresh and nothing follows.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_platform::{ChangeBatch, ChangeCheckpoint, ChangeMonitor, ChangeSink, MonitorStart, PlatformError, Result};

/// Identity of checkpoints from a platform without history; never replayable.
const IDENTITY: &str = "generic:no-history";

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
    ChangeCheckpoint { root: root.to_path_buf(), identity: IDENTITY.into(), event_id: 0 }
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
        _since: Option<&ChangeCheckpoint>,
        sink: ChangeSink,
    ) -> Result<MonitorStart> {
        self.stop();
        orbit_platform::posix::open_root(root)?;
        let shared: SharedSink = Arc::new(Mutex::new(Some(sink)));
        let worker = Arc::clone(&shared);
        let batch = ChangeBatch {
            directories: vec![root.to_path_buf()],
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
