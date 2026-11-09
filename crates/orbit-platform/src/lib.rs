//! The platform boundary for Orbit Disk.
//!
//! Engine and UI crates depend only on this crate. Each OS gets its own layer crate
//! (`orbit-platform-macos`, later `-windows` / `-linux`) that implements [`Platform`] and
//! [`Shell`]; `orbit-native` picks one at compile time so hot paths never go through `dyn`.
//!
//! Rules for layer authors:
//! - Names are raw platform bytes (`&[u8]`). Unix layers pass bytes through unchanged; a Windows
//!   layer encodes UTF-16 names as WTF-8. Nothing here assumes UTF-8.
//! - `dev` is `u64` everywhere. Layers widen narrower native device numbers without sign tricks.
//! - Directory traversal never follows symbolic links, and every open is checked against the
//!   identity the caller expects.
//! - Sizes are clamped to `>= 0` before they cross the boundary; directories report `0` for both.

mod cancel;
mod change;
mod error;
mod listing;
mod shell;
mod system;

#[cfg(unix)]
pub mod posix;

pub use cancel::Cancellation;
pub use change::{ChangeBatch, ChangeCheckpoint, ChangeMonitor, ChangeSink, MonitorStart};
pub use error::{PlatformError, Result};
pub use listing::{
    DirectoryLister, EntryKind, EntryRecord, FileIdentity, Issue, LISTING_BATCH, ListingSink, ListingTotals, Stop,
};
pub use shell::{LegacySettings, RelaunchHandle, Shell, Updater, UpdaterState};
pub use system::{AppDirs, BootLayout, CloudStatus, ProtectedPaths, VolumeInfo, VolumeLayout};

use std::path::{Path, PathBuf};

/// Engine-side services of one operating system.
///
/// One value is created at startup and shared (`Arc`) across scan workers, the index writer and
/// the UI. Per-thread state (buffers, descriptor caches) belongs in [`Platform::Lister`] values,
/// created once per worker with [`Platform::new_lister`].
pub trait Platform: Send + Sync + 'static {
    /// An open directory handle (an fd on Unix, a HANDLE on Windows). Closing happens on drop.
    type Dir: Send;
    /// A per-worker directory lister with its own reusable buffer.
    type Lister: DirectoryLister<Dir = Self::Dir>;
    /// Filesystem change history for a mapped root.
    type Monitor: ChangeMonitor;

    // --- Secure traversal -------------------------------------------------------------------

    /// Opens a root directory without following a final symlink and returns its identity.
    fn open_root(&self, path: &Path) -> Result<(Self::Dir, FileIdentity)>;

    /// Opens `name` inside `parent` without following symlinks, and fails with
    /// [`PlatformError::IdentityChanged`] unless the opened directory is `expected`.
    fn open_child(&self, parent: &Self::Dir, name: &[u8], expected: FileIdentity) -> Result<Self::Dir>;

    /// Identity of an already opened directory.
    fn dir_identity(&self, dir: &Self::Dir) -> Result<FileIdentity>;

    /// A stable name for the filesystem holding `dir` (macOS: `f_mntonname`). Used to decide
    /// whether traversal may cross into it; `None` when the OS cannot tell.
    fn mount_name(&self, dir: &Self::Dir) -> Option<String>;

    /// A new lister for one worker thread.
    fn new_lister(&self) -> Self::Lister;

    // --- Volumes ----------------------------------------------------------------------------

    /// Data the portable traversal scope needs to avoid double counting volume aliases
    /// (macOS firmlinks and the boot Data volume). Empty on platforms without such aliases.
    fn volume_layout(&self, root: &Path, root_identity: FileIdentity) -> VolumeLayout;

    /// Mounted, user-visible volumes for the Places sidebar and removal policy.
    fn volumes(&self) -> Vec<VolumeInfo>;

    // --- Cloud storage ----------------------------------------------------------------------

    /// Directories that hold synced cloud content and must never be mapped or collected.
    fn cloud_roots(&self, home: &Path) -> Vec<PathBuf>;

    /// Classifies one path without downloading content.
    fn cloud_status(&self, path: &Path) -> Result<CloudStatus>;

    /// Runs `body` with on-demand cloud downloads disabled for the *current thread*.
    /// Every thread that touches user files during mapping or removal must run inside this.
    fn without_materialization<R>(&self, body: impl FnOnce() -> R) -> Result<R>;

    // --- File operations --------------------------------------------------------------------

    /// Copy-on-write clone where supported, otherwise a full copy. `dst` must not exist.
    fn clone_file(&self, src: &Path, dst: &Path) -> Result<()>;

    /// Moves an item to the user's Trash / Recycle Bin. Returns the new location if known.
    fn trash(&self, path: &Path) -> Result<Option<PathBuf>>;

    // --- Identity & locations ---------------------------------------------------------------

    /// A stable identifier for this machine, used to validate saved change-history checkpoints.
    fn machine_identity(&self) -> Option<String>;

    /// Locations that removal policy must protect on this OS.
    fn protected_paths(&self, home: &Path) -> ProtectedPaths;

    /// Per-user application directories (cache, support, settings, runtime).
    fn app_dirs(&self) -> AppDirs;

    /// Creates a change monitor for this platform.
    fn new_monitor(&self) -> Self::Monitor;
}
