//! Helpers shared by every Unix layer and by the engine's lock and journal files: no-follow
//! directory opens with identity checks, `*at` file primitives, masked `stat`, `flock`, and durable
//! directory sync. Kept free of Apple-only symbols so Linux can reuse it.

use crate::{FileIdentity, PlatformError, Result};
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Calls `call` again while it fails with `EINTR`: a signal arrived before the call did anything,
/// so repeating it is exact. macOS returns `EINTR` from opens and stats in some folders (app
/// containers, file providers), which otherwise failed whole listings and mappings. Never use
/// it for `close`.
pub fn retry_interrupted<T: Copy + PartialEq + From<i8>>(mut call: impl FnMut() -> T) -> T {
    loop {
        let result = call();
        if result != T::from(-1) || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return result;
        }
    }
}

const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

fn c_name(bytes: &[u8], path: &Path) -> Result<CString> {
    CString::new(bytes).map_err(|_| PlatformError::os(path, libc::EINVAL))
}

/// `fstat` an open descriptor and return its identity.
pub fn identity(fd: BorrowedFd<'_>, path: &Path) -> Result<FileIdentity> {
    // SAFETY: `stat` is plain old data; zeroed is a valid value that fstat overwrites.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a live descriptor for the duration of the borrow; `info` is writable.
    if retry_interrupted(|| unsafe { libc::fstat(fd.as_raw_fd(), &mut info) }) != 0 {
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
    let fd = retry_interrupted(|| unsafe { libc::open(c.as_ptr(), DIR_FLAGS) });
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
    let fd = retry_interrupted(|| unsafe { libc::openat(parent.as_raw_fd(), c.as_ptr(), DIR_FLAGS) });
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
    if retry_interrupted(|| unsafe {
        libc::fstatat(parent.as_raw_fd(), c.as_ptr(), &mut info, libc::AT_SYMLINK_NOFOLLOW)
    }) != 0
    {
        return Err(PlatformError::last_os(path_for_errors));
    }
    Ok(info)
}

/// Identity of one entry without following a final symlink.
pub fn child_identity(parent: BorrowedFd<'_>, name: &[u8], path_for_errors: &Path) -> Result<FileIdentity> {
    stat_at(parent, name, path_for_errors).map(|info| stat_identity(&info))
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
    if retry_interrupted(|| unsafe { libc::flock(fd.as_raw_fd(), op) }) == 0 {
        return Ok(true);
    }
    let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    if code == libc::EWOULDBLOCK { Ok(false) } else { Err(PlatformError::os(path, code)) }
}

/// Flushes a directory entry change (create/rename/unlink) to stable storage.
pub fn fsync_dir(dir: &Path) -> Result<()> {
    let (fd, _) = open_root(dir)?;
    // SAFETY: live descriptor.
    if retry_interrupted(|| unsafe { libc::fsync(fd.as_raw_fd()) }) != 0 {
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

/// Owned copy of the `stat` fields that file-safety checks compare. The device is masked like
/// [`stat_identity`]; times are `(seconds, nanoseconds)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub identity: FileIdentity,
    pub mode: u32,
    pub uid: u32,
    pub nlink: u64,
    pub size: i64,
    pub mtime: (i64, i64),
    pub ctime: (i64, i64),
}

impl Stat {
    #[allow(clippy::unnecessary_cast)]
    pub fn from_raw(info: &libc::stat) -> Self {
        Self {
            identity: stat_identity(info),
            mode: info.st_mode as u32,
            uid: info.st_uid as u32,
            nlink: info.st_nlink as u64,
            size: info.st_size as i64,
            mtime: (info.st_mtime as i64, info.st_mtime_nsec as i64),
            ctime: (info.st_ctime as i64, info.st_ctime_nsec as i64),
        }
    }

    #[inline]
    fn file_type(&self) -> u32 {
        self.mode & libc::S_IFMT as u32
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == libc::S_IFDIR as u32
    }

    /// A regular file.
    pub fn is_file(&self) -> bool {
        self.file_type() == libc::S_IFREG as u32
    }

    pub fn permissions(&self) -> u32 {
        self.mode & 0o777
    }

    pub fn same_file(&self, other: &Stat) -> bool {
        self.identity == other.identity
    }

    /// Owned by this user, exactly `mode`, and (for non-directories) a single link.
    pub fn is_private(&self, mode: u32) -> bool {
        self.uid == euid() && self.permissions() == mode && (self.is_dir() || self.nlink == 1)
    }
}

/// The effective user id of this process.
pub fn euid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn c_bytes(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

/// `Ok(None)` for `ENOENT`, everything else unchanged.
pub fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn fstat(fd: BorrowedFd<'_>) -> io::Result<Stat> {
    // SAFETY: zeroed `stat` is valid plain old data.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is live for the borrow; `info` is writable.
    if retry_interrupted(|| unsafe { libc::fstat(fd.as_raw_fd(), &mut info) }) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Stat::from_raw(&info))
}

/// `fstatat(AT_SYMLINK_NOFOLLOW)`.
pub fn fstat_at(dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<Stat> {
    let c = c_bytes(name)?;
    // SAFETY: zeroed `stat` is valid plain old data.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: live descriptor, NUL-terminated name, writable out-param.
    if retry_interrupted(|| unsafe { libc::fstatat(dir.as_raw_fd(), c.as_ptr(), &mut info, libc::AT_SYMLINK_NOFOLLOW) })
        != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Stat::from_raw(&info))
}

/// `lstat`: the final component is not followed.
pub fn lstat(path: &Path) -> io::Result<Stat> {
    let c = c_bytes(path.as_os_str().as_bytes())?;
    // SAFETY: zeroed `stat` is valid plain old data.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated path, writable out-param.
    if retry_interrupted(|| unsafe { libc::lstat(c.as_ptr(), &mut info) }) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Stat::from_raw(&info))
}

/// `stat`: follows symlinks.
pub fn stat(path: &Path) -> io::Result<Stat> {
    let c = c_bytes(path.as_os_str().as_bytes())?;
    // SAFETY: zeroed `stat` is valid plain old data.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated path, writable out-param.
    if retry_interrupted(|| unsafe { libc::stat(c.as_ptr(), &mut info) }) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Stat::from_raw(&info))
}

/// `openat(dir, name, flags | O_NOFOLLOW | O_CLOEXEC, mode)`.
pub fn open_at(dir: BorrowedFd<'_>, name: &[u8], flags: libc::c_int, mode: u32) -> io::Result<OwnedFd> {
    let c = c_bytes(name)?;
    // SAFETY: live descriptor and NUL-terminated name; `mode` is the variadic argument `openat`
    // reads only with O_CREAT.
    let fd = retry_interrupted(|| unsafe {
        libc::openat(dir.as_raw_fd(), c.as_ptr(), flags | libc::O_NOFOLLOW | libc::O_CLOEXEC, mode as libc::c_uint)
    });
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh descriptor we now own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Opens the directory `name` inside `dir` without following a symlink.
pub fn open_dir_at(dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<OwnedFd> {
    open_at(dir, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
}

/// Creates `name` (`O_EXCL`, write-only) with `mode`; never follows or replaces anything.
pub fn create_exclusive_at(dir: BorrowedFd<'_>, name: &[u8], mode: u32) -> io::Result<OwnedFd> {
    open_at(dir, name, libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY, mode)
}

pub fn mkdir_at(dir: BorrowedFd<'_>, name: &[u8], mode: u32) -> io::Result<()> {
    let c = c_bytes(name)?;
    // SAFETY: live descriptor and NUL-terminated name.
    if retry_interrupted(|| unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), mode as libc::mode_t) }) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `unlinkat`; `directory` selects `AT_REMOVEDIR`. Never follows a symlink: the entry itself goes.
pub fn unlink_at(dir: BorrowedFd<'_>, name: &[u8], directory: bool) -> io::Result<()> {
    let c = c_bytes(name)?;
    let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: live descriptor and NUL-terminated name.
    if retry_interrupted(|| unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), flags) }) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Renames `from` to `to`, both inside `dir`.
pub fn rename_at(dir: BorrowedFd<'_>, from: &[u8], to: &[u8]) -> io::Result<()> {
    let (from, to) = (c_bytes(from)?, c_bytes(to)?);
    // SAFETY: live descriptor and NUL-terminated names.
    if retry_interrupted(|| unsafe { libc::renameat(dir.as_raw_fd(), from.as_ptr(), dir.as_raw_fd(), to.as_ptr()) })
        != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn fchmod(fd: BorrowedFd<'_>, mode: u32) -> io::Result<()> {
    // SAFETY: live descriptor.
    if retry_interrupted(|| unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) }) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn fsync(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: live descriptor.
    if retry_interrupted(|| unsafe { libc::fsync(fd.as_raw_fd()) }) != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn read(fd: BorrowedFd<'_>, buffer: &mut [u8]) -> io::Result<usize> {
    // SAFETY: live descriptor; the buffer is writable for its full length.
    let count = retry_interrupted(|| unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) });
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(count as usize)
}

pub fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        // SAFETY: live descriptor; `bytes` is readable for its full length.
        let count = retry_interrupted(|| unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) });
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        bytes = &bytes[count as usize..];
    }
    Ok(())
}

/// Names in an open directory, excluding `.` and `..`. The descriptor is duplicated, so `dir` stays
/// usable. A read error part-way ends the listing early; callers treat the result as advisory.
pub fn read_names(dir: BorrowedFd<'_>) -> io::Result<Vec<Vec<u8>>> {
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

#[cfg(test)]
mod interrupted_tests {
    fn set_errno(value: libc::c_int) {
        // SAFETY: the thread's errno slot.
        unsafe {
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            {
                *libc::__error() = value;
            }
            #[cfg(target_os = "linux")]
            {
                *libc::__errno_location() = value;
            }
        }
    }

    /// An interrupted call is repeated until it completes; other failures return at once.
    #[test]
    fn interrupted_calls_are_repeated_and_other_failures_returned() {
        let mut calls = 0;
        let result = super::retry_interrupted(|| {
            calls += 1;
            if calls < 3 {
                set_errno(libc::EINTR);
                -1
            } else {
                7
            }
        });
        assert_eq!((result, calls), (7, 3));
        let mut calls = 0;
        let result = super::retry_interrupted(|| {
            calls += 1;
            set_errno(libc::ENOENT);
            -1
        });
        assert_eq!((result, calls), (-1, 1));
    }
}
