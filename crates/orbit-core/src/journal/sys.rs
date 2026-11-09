//! Portable POSIX calls the journal needs beyond `orbit_platform::posix`.
//!
//! Everything here is plain POSIX (no Apple- or Linux-only symbols). These helpers are candidates
//! for `orbit_platform::posix`; they live here until the integrator moves them.

use orbit_platform::posix::{self, LockMode};
use orbit_platform::{FileIdentity, PlatformError};
use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(crate) const FILE_MODE: libc::mode_t = 0o600;
pub(crate) const DIR_MODE: libc::mode_t = 0o700;

fn c_bytes(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn c_path(path: &Path) -> io::Result<CString> {
    c_bytes(path.as_os_str().as_bytes())
}

/// Owned copy of the `stat` fields the journal checks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Meta {
    pub identity: FileIdentity,
    pub mode: libc::mode_t,
    pub uid: libc::uid_t,
    #[allow(clippy::struct_field_names)]
    pub nlink: u64,
}

impl Meta {
    fn from_stat(info: &libc::stat) -> Self {
        #[allow(clippy::unnecessary_cast)]
        Self { identity: posix::stat_identity(info), mode: info.st_mode, uid: info.st_uid, nlink: info.st_nlink as u64 }
    }

    pub fn is_dir(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFDIR
    }

    pub fn is_file(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFREG
    }

    pub fn permissions(&self) -> libc::mode_t {
        self.mode & 0o777
    }

    /// Owned by this user, exactly `mode`, and (for files) a single link.
    pub fn is_private(&self, mode: libc::mode_t) -> bool {
        self.uid == euid() && self.permissions() == mode && (self.is_dir() || self.nlink == 1)
    }
}

pub(crate) fn euid() -> libc::uid_t {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

pub(crate) fn fstat(fd: BorrowedFd<'_>) -> io::Result<Meta> {
    // SAFETY: zeroed `stat` is valid plain old data.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is live for the borrow; `info` is writable.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut info) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Meta::from_stat(&info))
}

/// `fstatat(AT_SYMLINK_NOFOLLOW)`.
pub(crate) fn stat_at(dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<Meta> {
    let c = c_bytes(name)?;
    // SAFETY: zeroed `stat` is valid plain old data.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: live descriptor, NUL-terminated name, writable out-param.
    if unsafe { libc::fstatat(dir.as_raw_fd(), c.as_ptr(), &mut info, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Meta::from_stat(&info))
}

/// `lstat`: the final component is not followed.
pub(crate) fn lstat(path: &Path) -> io::Result<Meta> {
    let c = c_path(path)?;
    // SAFETY: zeroed `stat` is valid plain old data.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated path, writable out-param.
    if unsafe { libc::lstat(c.as_ptr(), &mut info) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Meta::from_stat(&info))
}

/// `stat`: follows symlinks.
pub(crate) fn stat(path: &Path) -> io::Result<Meta> {
    let c = c_path(path)?;
    // SAFETY: zeroed `stat` is valid plain old data.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated path, writable out-param.
    if unsafe { libc::stat(c.as_ptr(), &mut info) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Meta::from_stat(&info))
}

/// `openat(dir, name, flags | O_NOFOLLOW | O_CLOEXEC, mode)`.
pub(crate) fn open_at(dir: BorrowedFd<'_>, name: &[u8], flags: libc::c_int, mode: libc::mode_t) -> io::Result<OwnedFd> {
    let c = c_bytes(name)?;
    // SAFETY: live descriptor and NUL-terminated name; `mode` is passed as the variadic argument
    // `openat` reads only with O_CREAT.
    let fd = unsafe {
        libc::openat(dir.as_raw_fd(), c.as_ptr(), flags | libc::O_NOFOLLOW | libc::O_CLOEXEC, libc::c_uint::from(mode))
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh descriptor we now own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub(crate) fn mkdir_at(dir: BorrowedFd<'_>, name: &[u8], mode: libc::mode_t) -> io::Result<()> {
    let c = c_bytes(name)?;
    // SAFETY: live descriptor and NUL-terminated name.
    if unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn fchmod(fd: BorrowedFd<'_>, mode: libc::mode_t) -> io::Result<()> {
    // SAFETY: live descriptor.
    if unsafe { libc::fchmod(fd.as_raw_fd(), mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn fsync(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: live descriptor.
    if unsafe { libc::fsync(fd.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `unlinkat`; `directory` selects `AT_REMOVEDIR`. Never follows a symlink: the entry itself goes.
pub(crate) fn unlink_at(dir: BorrowedFd<'_>, name: &[u8], directory: bool) -> io::Result<()> {
    let c = c_bytes(name)?;
    let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: live descriptor and NUL-terminated name.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Names in an open directory, excluding `.` and `..`. The descriptor is duplicated, so `dir` stays
/// usable. A read error part-way ends the listing early; callers treat the result as advisory.
pub(crate) fn read_names(dir: BorrowedFd<'_>) -> io::Result<Vec<Vec<u8>>> {
    // SAFETY: live descriptor; `dup` returns a new descriptor or -1.
    let copy = unsafe { libc::dup(dir.as_raw_fd()) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `copy` is a fresh descriptor; on success `fdopendir` takes ownership of it.
    let stream = unsafe { libc::fdopendir(copy) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `copy` is still ours to close.
        unsafe { libc::close(copy) };
        return Err(error);
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `stream` is a live DIR* we own until `closedir` below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `readdir` returned a valid entry whose `d_name` is NUL-terminated and stays valid
        // until the next `readdir`/`closedir` on this stream; we copy it immediately.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(name.to_vec());
        }
    }
    // SAFETY: `stream` is live and closed exactly once (this also closes `copy`).
    unsafe { libc::closedir(stream) };
    Ok(names)
}

/// Opens a directory path without following its final component.
pub(crate) fn open_dir(path: &Path) -> Result<(OwnedFd, FileIdentity), PlatformError> {
    posix::open_root(path)
}

/// Non-blocking `flock`. `Ok(false)` when another description holds a conflicting lock.
pub(crate) fn try_lock(fd: BorrowedFd<'_>, shared: bool, path: &Path) -> Result<bool, PlatformError> {
    posix::try_flock(fd, if shared { LockMode::Shared } else { LockMode::Exclusive }, path)
}
