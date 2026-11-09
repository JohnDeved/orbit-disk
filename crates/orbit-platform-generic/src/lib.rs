//! Portable fallback layer. Implements [`orbit_platform::Platform`] with `readdir` + `fstatat`
//! and no change history, so the engine's core tests run on any Unix (Linux CI included).
//!
//! Only portable POSIX calls are used here; anything that differs between Unix flavours is
//! limited to the `errno` location.
#![cfg(unix)]

mod listing;
mod monitor;
mod shell;

use std::ffi::CString;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use orbit_platform::{
    AppDirs, CloudStatus, FileIdentity, Platform, PlatformError, ProtectedPaths, Result, VolumeInfo, VolumeLayout,
    posix,
};

pub use listing::GenericLister;
pub use monitor::GenericMonitor;
pub use shell::GenericShell;

/// The portable [`Platform`].
#[derive(Debug, Default)]
pub struct GenericPlatform;

impl GenericPlatform {
    pub fn new() -> Self {
        Self
    }
}

pub(crate) fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

/// Clears `errno` so end-of-directory can be told apart from a `readdir` failure.
pub(crate) fn clear_errno() {
    // SAFETY: each accessor returns this thread's errno slot.
    unsafe {
        #[cfg(any(target_os = "linux", target_os = "emscripten", target_os = "hurd"))]
        {
            *libc::__errno_location() = 0;
        }
        #[cfg(any(target_vendor = "apple", target_os = "freebsd", target_os = "dragonfly"))]
        {
            *libc::__error() = 0;
        }
        #[cfg(any(target_os = "openbsd", target_os = "netbsd", target_os = "android"))]
        {
            *libc::__errno() = 0;
        }
    }
}

fn c_path(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| PlatformError::os(path, libc::EINVAL))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| PathBuf::from("/"))
}

/// An absolute XDG base directory, or `fallback` below the home directory.
fn xdg(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(fallback))
}

impl Platform for GenericPlatform {
    type Dir = OwnedFd;
    type Lister = GenericLister;
    type Monitor = GenericMonitor;

    fn open_root(&self, path: &Path) -> Result<(OwnedFd, FileIdentity)> {
        posix::open_root(path)
    }

    fn open_child(&self, parent: &OwnedFd, name: &[u8], expected: FileIdentity) -> Result<OwnedFd> {
        posix::open_child(parent.as_fd(), name, expected, posix::path_from_bytes(name))
    }

    fn child_identity(&self, parent: &OwnedFd, name: &[u8]) -> Result<FileIdentity> {
        posix::child_identity(parent.as_fd(), name, posix::path_from_bytes(name))
    }

    fn dir_identity(&self, dir: &OwnedFd) -> Result<FileIdentity> {
        posix::identity(dir.as_fd(), Path::new(""))
    }

    fn mount_name(&self, _dir: &OwnedFd) -> Option<String> {
        None
    }

    fn new_lister(&self) -> GenericLister {
        GenericLister::default()
    }

    fn volume_layout(&self, _root: &Path, _root_identity: FileIdentity) -> VolumeLayout {
        VolumeLayout::default()
    }

    fn volumes(&self) -> Vec<VolumeInfo> {
        // SAFETY: zeroed statvfs is valid POD.
        let mut info: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: static NUL-terminated path, writable out-param.
        if unsafe { libc::statvfs(c"/".as_ptr(), &mut info) } != 0 {
            return Vec::new();
        }
        // Field widths differ between Unix flavours.
        #[allow(clippy::unnecessary_cast)]
        let (blocks, available, fragment) = (info.f_blocks as u64, info.f_bavail as u64, info.f_frsize as u64);
        vec![VolumeInfo {
            path: PathBuf::from("/"),
            name: "/".into(),
            total_bytes: blocks.saturating_mul(fragment),
            available_bytes: available.saturating_mul(fragment),
            is_boot: true,
            is_removable: false,
            is_local: true,
        }]
    }

    fn cloud_roots(&self, _home: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    fn cloud_status(&self, path: &Path) -> Result<CloudStatus> {
        std::fs::symlink_metadata(path)
            .map(|_| CloudStatus::Local)
            .map_err(|e| PlatformError::os(path, e.raw_os_error().unwrap_or(libc::EIO)))
    }

    fn without_materialization<R>(&self, body: impl FnOnce() -> R) -> Result<R> {
        Ok(body())
    }

    fn clone_file(&self, src: &Path, dst: &Path) -> Result<()> {
        copy_exclusive(src, dst)
    }

    fn trash(&self, _path: &Path) -> Result<Option<PathBuf>> {
        Err(PlatformError::Unsupported("moving items to the Trash is not supported on this platform"))
    }

    fn machine_identity(&self) -> Option<String> {
        None
    }

    fn protected_paths(&self, home: &Path) -> ProtectedPaths {
        let system = [
            "/bin", "/boot", "/dev", "/etc", "/lib", "/lib32", "/lib64", "/libx32", "/opt", "/proc", "/root", "/run",
            "/sbin", "/srv", "/sys", "/usr",
        ];
        let mut system_roots: Vec<PathBuf> = system.iter().map(PathBuf::from).collect();
        system_roots.push(xdg("XDG_CONFIG_HOME", ".config"));
        system_roots.push(home.join(".local/share"));
        ProtectedPaths {
            system_roots,
            temporary_roots: vec![PathBuf::from("/tmp"), PathBuf::from("/var/tmp")],
            restricted_root: Some(PathBuf::from("/var")),
            trash_components: vec![".Trash", ".Trashes"],
            volumes_root: Some(PathBuf::from("/media")),
        }
    }

    fn app_dirs(&self) -> AppDirs {
        // SAFETY: geteuid has no preconditions.
        let euid = unsafe { libc::geteuid() };
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|dir| dir.join("orbit-disk"))
            .unwrap_or_else(|| std::env::temp_dir().join(format!("orbit-disk-{euid}")));
        AppDirs {
            cache: xdg("XDG_CACHE_HOME", ".cache").join("orbit-disk/maps"),
            support: xdg("XDG_DATA_HOME", ".local/share").join("orbit-disk/engine"),
            settings: xdg("XDG_CONFIG_HOME", ".config").join("orbit-disk"),
            runtime,
        }
    }

    fn new_monitor(&self) -> GenericMonitor {
        GenericMonitor::default()
    }
}

/// Copies `src` into a newly created `dst`; never replaces or follows anything at `dst`.
fn copy_exclusive(src: &Path, dst: &Path) -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let os = |path: &Path, e: std::io::Error| PlatformError::os(path, e.raw_os_error().unwrap_or(libc::EIO));
    let mut input = std::fs::File::open(src).map_err(|e| os(src, e))?;
    let mode = input.metadata().map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions())).unwrap_or(0o600);
    let c_dst = c_path(dst)?;
    // SAFETY: NUL-terminated path; O_EXCL refuses an existing entry, including a symlink.
    let fd = unsafe {
        libc::open(
            c_dst.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(PlatformError::os(dst, errno()));
    }
    // SAFETY: `open` returned a fresh descriptor we own.
    let mut output = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    let result = std::io::copy(&mut input, &mut output).and_then(|_| output.sync_all()).and_then(|_| {
        // SAFETY: live descriptor.
        if unsafe { libc::fchmod(output.as_raw_fd(), (mode & 0o7777) as libc::mode_t) } != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    });
    if let Err(error) = result {
        drop(output);
        let _ = std::fs::remove_file(dst);
        return Err(os(dst, error));
    }
    Ok(())
}
