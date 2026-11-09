//! macOS engine layer. Implements [`orbit_platform::Platform`] with `getattrlistbulk`, FSEvents,
//! firmlink-aware volume layout, `clonefile`, the Trash and the dataless-file I/O policy.
#![cfg(target_os = "macos")]

mod ffi;
pub mod firmlinks;
mod listing;
pub mod monitor;
mod paths;
mod system;

use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use orbit_platform::{
    AppDirs, CloudStatus, FileIdentity, Platform, ProtectedPaths, Result, VolumeInfo, VolumeLayout, posix,
};

pub use listing::{ListingMode, MacLister};
pub use monitor::FsEventsMonitor;

/// The macOS [`Platform`].
#[derive(Debug, Default)]
pub struct MacPlatform;

impl MacPlatform {
    pub fn new() -> Self {
        Self
    }

    /// A lister that always uses `readdir` + `fstatat` (benchmarks and parity tests).
    pub fn new_fallback_lister(&self) -> MacLister {
        MacLister::new(ListingMode::Fallback)
    }

    /// The current thread's dataless-materialization I/O policy (diagnostics and tests).
    pub fn materialization_policy(&self) -> i32 {
        system::materialization_policy()
    }
}

impl Platform for MacPlatform {
    type Dir = OwnedFd;
    type Lister = MacLister;
    type Monitor = FsEventsMonitor;

    fn open_root(&self, path: &Path) -> Result<(OwnedFd, FileIdentity)> {
        posix::open_root(path)
    }

    fn open_child(&self, parent: &OwnedFd, name: &[u8], expected: FileIdentity) -> Result<OwnedFd> {
        posix::open_child(parent.as_fd(), name, expected, posix::path_from_bytes(name))
    }

    fn dir_identity(&self, dir: &OwnedFd) -> Result<FileIdentity> {
        posix::identity(dir.as_fd(), Path::new(""))
    }

    fn mount_name(&self, dir: &OwnedFd) -> Option<String> {
        // SAFETY: zeroed statfs is valid POD.
        let mut info: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: live descriptor, writable out-param.
        if unsafe { libc::fstatfs(dir.as_raw_fd(), &mut info) } != 0 {
            return None;
        }
        Some(paths::lossy(&paths::mount_on_name(&info)))
    }

    fn new_lister(&self) -> MacLister {
        MacLister::new(ListingMode::Bulk)
    }

    fn volume_layout(&self, root: &Path, root_identity: FileIdentity) -> VolumeLayout {
        let root_bytes = paths::standardize(paths::bytes(root));
        let root_mount = paths::mount_of(&root_bytes).map(|mount| paths::lossy(&mount));
        let boot = if root_bytes == b"/" && paths::directory_identity(b"/") == Some(root_identity) {
            firmlinks::capture(firmlinks::system_links())
        } else {
            None
        };
        VolumeLayout { root_mount, boot }
    }

    fn volumes(&self) -> Vec<VolumeInfo> {
        system::volumes()
    }

    fn cloud_roots(&self, home: &Path) -> Vec<PathBuf> {
        system::cloud_roots(home)
    }

    fn cloud_status(&self, path: &Path) -> Result<CloudStatus> {
        system::cloud_status(path)
    }

    fn without_materialization<R>(&self, body: impl FnOnce() -> R) -> Result<R> {
        system::without_materialization(body)
    }

    fn clone_file(&self, src: &Path, dst: &Path) -> Result<()> {
        system::clone_file(src, dst)
    }

    fn trash(&self, path: &Path) -> Result<Option<PathBuf>> {
        system::trash(path)
    }

    fn machine_identity(&self) -> Option<String> {
        system::machine_identity()
    }

    fn protected_paths(&self, home: &Path) -> ProtectedPaths {
        system::protected_paths(home)
    }

    fn app_dirs(&self) -> AppDirs {
        system::app_dirs()
    }

    fn new_monitor(&self) -> FsEventsMonitor {
        FsEventsMonitor::default()
    }
}
