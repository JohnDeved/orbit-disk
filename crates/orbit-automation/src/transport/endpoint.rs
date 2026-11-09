//! The private socket directory: owner-only, never followed through symlinks, with a lock file
//! that makes one server per user exclusive.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::wire::{self, errno};
use crate::TransportError;

const LOCK_NAME: &[u8] = b"control.lock";

/// Device and inode of the socket file this server bound, so it never unlinks a replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SocketIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

pub(crate) struct EndpointDirectory {
    fd: OwnedFd,
    name: CString,
}

fn stat_mode_is(info: &libc::stat, kind: libc::mode_t, permissions: libc::mode_t) -> bool {
    info.st_mode & libc::S_IFMT == kind && info.st_mode & 0o777 == permissions
}

impl EndpointDirectory {
    /// Opens the endpoint's parent directory. With `create`, a missing directory is created 0700.
    pub(crate) fn open(endpoint: &Path, create: bool) -> Result<Self, TransportError> {
        let invalid = || TransportError::InvalidEndpoint("invalid path");
        let bytes = endpoint.as_os_str().as_bytes();
        let name = endpoint.file_name().ok_or_else(invalid)?.as_bytes();
        let parent = endpoint.parent().ok_or_else(invalid)?.as_os_str().as_bytes();
        if !endpoint.is_absolute() || bytes.contains(&0) || name.is_empty() {
            return Err(invalid());
        }
        let parent = wire::c_string(if parent.is_empty() { b"/" } else { parent }).ok_or_else(invalid)?;
        let name = wire::c_string(name).ok_or_else(invalid)?;
        // SAFETY: `parent` is a valid NUL-terminated path.
        if create && unsafe { libc::mkdir(parent.as_ptr(), 0o700) } != 0 && errno() != libc::EEXIST {
            return Err(TransportError::System("Create automation directory", errno()));
        }
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `parent` is a valid NUL-terminated path.
        let raw = unsafe { libc::open(parent.as_ptr(), flags) };
        if raw < 0 {
            if !create && errno() == libc::ENOENT {
                return Err(TransportError::Unavailable);
            }
            return Err(TransportError::InvalidEndpoint("directory is missing or is a symbolic link"));
        }
        // SAFETY: `raw` is a freshly opened descriptor that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        match fstat(&fd) {
            Some(info) if info.st_uid == wire::effective_uid() && stat_mode_is(&info, libc::S_IFDIR, 0o700) => {}
            _ => return Err(TransportError::InvalidEndpoint("directory must belong to this user and have mode 0700")),
        }
        wire::address(endpoint)?;
        Ok(Self { fd, name })
    }

    /// Takes the exclusive, non-blocking single-instance lock. Dropping the result releases it.
    pub(crate) fn lock(&self) -> Result<OwnedFd, TransportError> {
        let name = CString::new(LOCK_NAME).expect("static name has no NUL");
        let flags = libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: valid directory descriptor and NUL-terminated relative name.
        let raw = unsafe { libc::openat(self.fd.as_raw_fd(), name.as_ptr(), flags, 0o600 as libc::c_uint) };
        if raw < 0 {
            return Err(TransportError::InvalidEndpoint("lock file is unavailable"));
        }
        // SAFETY: `raw` is a freshly opened descriptor that nothing else owns.
        let lock = unsafe { OwnedFd::from_raw_fd(raw) };
        match fstat(&lock) {
            Some(info)
                if info.st_uid == wire::effective_uid()
                    && info.st_nlink == 1
                    && stat_mode_is(&info, libc::S_IFREG, 0o600) => {}
            _ => return Err(TransportError::InvalidEndpoint("lock file must be private and owned by this user")),
        }
        // SAFETY: flock on an owned descriptor.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let code = errno();
            if code == libc::EWOULDBLOCK {
                return Err(TransportError::AlreadyRunning);
            }
            return Err(TransportError::System("Lock automation endpoint", code));
        }
        Ok(lock)
    }

    /// The current socket file, if any. Anything that is not a private socket of this user
    /// (including a symlink) is rejected rather than reported as absent.
    pub(crate) fn socket_identity(&self) -> Result<Option<SocketIdentity>, TransportError> {
        // SAFETY: stat is plain old data; all-zero is a valid value.
        let mut info: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid directory descriptor, NUL-terminated name, writable stat buffer.
        let result =
            unsafe { libc::fstatat(self.fd.as_raw_fd(), self.name.as_ptr(), &mut info, libc::AT_SYMLINK_NOFOLLOW) };
        if result != 0 {
            let code = errno();
            if code == libc::ENOENT {
                return Ok(None);
            }
            return Err(TransportError::System("Read automation endpoint", code));
        }
        if info.st_uid != wire::effective_uid() || !stat_mode_is(&info, libc::S_IFSOCK, 0o600) {
            return Err(TransportError::InvalidEndpoint(
                "socket must be private, owned by this user, and not a symbolic link",
            ));
        }
        Ok(Some(SocketIdentity { device: info.st_dev, inode: info.st_ino }))
    }

    pub(crate) fn protect_socket(&self) -> Result<(), TransportError> {
        // SAFETY: valid directory descriptor and NUL-terminated name.
        if unsafe { libc::fchmodat(self.fd.as_raw_fd(), self.name.as_ptr(), 0o600, 0) } != 0 {
            return Err(TransportError::System("Protect automation socket", errno()));
        }
        Ok(())
    }

    /// Unlinks the socket only if it is still the one identified by `expected`.
    pub(crate) fn unlink_socket(&self, expected: SocketIdentity) {
        if matches!(self.socket_identity(), Ok(Some(current)) if current == expected) {
            // SAFETY: valid directory descriptor and NUL-terminated name.
            unsafe { libc::unlinkat(self.fd.as_raw_fd(), self.name.as_ptr(), 0) };
        }
    }
}

fn fstat(fd: &OwnedFd) -> Option<libc::stat> {
    // SAFETY: stat is plain old data; all-zero is a valid value.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: owned descriptor and writable stat buffer.
    (unsafe { libc::fstat(fd.as_raw_fd(), &mut info) } == 0).then_some(info)
}
