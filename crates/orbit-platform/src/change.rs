use crate::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A saved position in the OS change history for one root.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChangeCheckpoint {
    pub root: PathBuf,
    /// Opaque: which history the `event_id` belongs to (machine, volume, device). A checkpoint is
    /// only replayable when the current identity is identical.
    pub identity: String,
    pub event_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeBatch {
    /// Directories to reconcile recursively. Paths may already be removed or renamed.
    pub directories: Vec<PathBuf>,
    /// History was lost (dropped events, wrapped IDs, root changed): rescan everything.
    pub full_refresh: bool,
    /// Replay of saved history has caught up with the present.
    pub history_finished: bool,
    /// Persist only after every change through this batch is applied to the saved index.
    pub checkpoint: ChangeCheckpoint,
}

pub type ChangeSink = Box<dyn FnMut(ChangeBatch) + Send + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorStart {
    /// History since the checkpoint will be replayed, then live changes follow.
    Replaying,
    /// The checkpoint cannot be replayed; a `full_refresh` batch has been (or will be) delivered.
    FullRefresh,
}

/// Persistent filesystem change history (macOS FSEvents; Windows USN journal). Platforms without
/// history (Linux inotify) return `false` from [`ChangeMonitor::has_history`]; the engine then
/// rescans at launch and uses live events only.
pub trait ChangeMonitor: Send {
    fn has_history(&self) -> bool;

    /// The current history position for `root`, captured *before* a scan starts.
    fn capture(&self, root: &Path) -> Result<ChangeCheckpoint>;

    /// Starts delivering batches for `root` (excluding `exclusions`), replaying from `since` when
    /// possible. Batches are delivered on a platform thread; the sink must not block.
    fn start(
        &mut self,
        root: &Path,
        exclusions: &[PathBuf],
        since: Option<&ChangeCheckpoint>,
        sink: ChangeSink,
    ) -> Result<MonitorStart>;

    /// Stops delivery. After return, the sink is never called again.
    fn stop(&mut self);

    /// Replays the saved history of `checkpoint.root` from `checkpoint` and returns `true` only
    /// when the checkpoint still belongs to the current history, the replay records no change at
    /// all, and it catches up with the present within `timeout`. Lets the engine reuse a saved map
    /// of a subfolder without rescanning it. Platforms without history return `false`.
    fn history_unchanged_since(&self, checkpoint: &ChangeCheckpoint, timeout: Duration) -> bool {
        let _ = (checkpoint, timeout);
        false
    }
}
