//! Helpers shared by every Unix layer: no-follow directory opens with identity checks, `flock`,
//! and durable directory sync. Kept free of Apple-only symbols so Linux can reuse it.

use crate::{FileIdentity, PlatformError, Result};
use std::ffi::{CString, OsStr};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

fn c_name(bytes: &[u8], path: &Path) -> Result<CString> {
    CString::new(bytes).map_err(|_| PlatformError::os(path, libc::EINVAL))
}

/// `fstat` an open descriptor and return its identity.
pub fn identity(fd: BorrowedFd<'_>, path: &Path) -> Result<FileIdentity> {
    // SAFETY: `stat` is plain old data; zeroed is a valid value that fstat overwrites.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a live descriptor for the duration of the borrow; `info` is writable.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut info) } != 0 {
        return Err(PlatformError::last_os(path));
    }
    Ok(stat_identity(&info))
}

#[inline]
pub fn stat_identity(info: &libc::stat) -> FileIdentity {
    // `st_dev` is i32 on Darwin and u64 on Linux; widen through the unsigned bit pattern so
    // identities round-trip and compare equal across both representations.
    #[allow(clippy::unnecessary_cast)]
    let dev = info.st_dev as u64 & dev_mask();
    #[allow(clippy::unnecessary_cast)]
    FileIdentity { dev, ino: info.st_ino as u64 }
}

#[inline]
const fn dev_mask() -> u64 {
    if std::mem::size_of::<libc::dev_t>() == 4 { u32::MAX as u64 } else { u64::MAX }
}

/// Opens an absolute directory path without following a final symlink.
pub fn open_root(path: &Path) -> Result<(OwnedFd, FileIdentity)> {
    let c = c_name(path.as_os_str().as_bytes(), path)?;
    // SAFETY: `c` is a valid NUL-terminated string.
    let fd = unsafe { libc::open(c.as_ptr(), DIR_FLAGS) };
    if fd < 0 {
        return Err(PlatformError::last_os(path));
    }
    // SAFETY: `open` returned a fresh descriptor we now own.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let id = identity(fd.as_fd_borrowed(), path)?;
    Ok((fd, id))
}

/// Opens `name` inside `parent` without following symlinks and verifies its identity.
pub fn open_child(
    parent: BorrowedFd<'_>,
    name: &[u8],
    expected: FileIdentity,
    path_for_errors: &Path,
) -> Result<OwnedFd> {
    let c = c_name(name, path_for_errors)?;
    // SAFETY: `parent` is a live directory descriptor; `c` is NUL-terminated.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), c.as_ptr(), DIR_FLAGS) };
    if fd < 0 {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        // ELOOP/ENOTDIR: a symlink or file was swapped in where the directory was.
        return Err(if code == libc::ELOOP || code == libc::ENOTDIR {
            PlatformError::IdentityChanged { path: path_for_errors.to_path_buf() }
        } else {
            PlatformError::os(path_for_errors, code)
        });
    }
    // SAFETY: `openat` returned a fresh descriptor we now own.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if identity(fd.as_fd_borrowed(), path_for_errors)? != expected {
        return Err(PlatformError::IdentityChanged { path: path_for_errors.to_path_buf() });
    }
    Ok(fd)
}

/// `fstatat(AT_SYMLINK_NOFOLLOW)` on one entry.
pub fn stat_at(parent: BorrowedFd<'_>, name: &[u8], path_for_errors: &Path) -> Result<libc::stat> {
    let c = c_name(name, path_for_errors)?;
    // SAFETY: zeroed `stat` is valid POD.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: live descriptor, NUL-terminated name, writable out-param.
    if unsafe { libc::fstatat(parent.as_raw_fd(), c.as_ptr(), &mut info, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(PlatformError::last_os(path_for_errors));
    }
    Ok(info)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}

/// Non-blocking `flock`. Returns `Ok(false)` when another holder has it.
pub fn try_flock(fd: BorrowedFd<'_>, mode: LockMode, path: &Path) -> Result<bool> {
    let op = match mode {
        LockMode::Shared => libc::LOCK_SH,
        LockMode::Exclusive => libc::LOCK_EX,
    } | libc::LOCK_NB;
    // SAFETY: live descriptor.
    if unsafe { libc::flock(fd.as_raw_fd(), op) } == 0 {
        return Ok(true);
    }
    let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    if code == libc::EWOULDBLOCK { Ok(false) } else { Err(PlatformError::os(path, code)) }
}

/// Flushes a directory entry change (create/rename/unlink) to stable storage.
pub fn fsync_dir(dir: &Path) -> Result<()> {
    let (fd, _) = open_root(dir)?;
    // SAFETY: live descriptor.
    if unsafe { libc::fsync(fd.as_raw_fd()) } != 0 {
        return Err(PlatformError::last_os(dir));
    }
    Ok(())
}

/// Joins a directory path and a child name as raw bytes.
pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(dir.len() + 1 + name.len());
    out.extend_from_slice(dir);
    if dir.last() != Some(&b'/') {
        out.push(b'/');
    }
    out.extend_from_slice(name);
    out
}

pub fn path_from_bytes(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

trait AsFdBorrowed {
    fn as_fd_borrowed(&self) -> BorrowedFd<'_>;
}

impl AsFdBorrowed for OwnedFd {
    fn as_fd_borrowed(&self) -> BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_child_rejects_symlink_swap() {
        let dir = std::env::temp_dir().join(format!("orbit-posix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        let (root, _) = open_root(&dir).unwrap();
        let real = identity(open_root(&dir.join("real")).unwrap().0.as_fd_borrowed(), &dir).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let err = open_child(root.as_fd_borrowed(), b"link", real, &dir.join("link")).unwrap_err();
        assert!(matches!(err, PlatformError::IdentityChanged { .. }), "{err:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
