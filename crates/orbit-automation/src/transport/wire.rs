//! Non-blocking Unix-domain socket primitives with absolute deadlines and length-prefixed frames.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use crate::TransportError;

/// Largest payload in one frame (1 MiB). Larger declared lengths are rejected before allocating.
pub const MAX_FRAME: usize = 1_048_576;

#[cfg(target_vendor = "apple")]
const SEND_FLAGS: libc::c_int = 0; // SO_NOSIGPIPE is set per socket instead.
#[cfg(not(target_vendor = "apple"))]
const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;

pub(crate) fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

pub(crate) fn socket() -> Result<OwnedFd, TransportError> {
    // SAFETY: plain socket(2) call; the result is checked before use.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(TransportError::System("Create automation socket", errno()));
    }
    // SAFETY: `raw` is a freshly created descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    configure(fd.as_raw_fd())?;
    Ok(fd)
}

/// Non-blocking, close-on-exec, and (on Apple platforms) no SIGPIPE on writes to a closed peer.
pub(crate) fn configure(fd: RawFd) -> Result<(), TransportError> {
    let failed = || TransportError::System("Configure automation socket", errno());
    // SAFETY: fcntl on a descriptor owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: as above; only adds O_NONBLOCK / FD_CLOEXEC.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(failed());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        return Err(failed());
    }
    #[cfg(target_vendor = "apple")]
    {
        let one: libc::c_int = 1;
        // SAFETY: `one` outlives the call and the length matches its type.
        let result = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&one as *const libc::c_int).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if result != 0 {
            return Err(failed());
        }
    }
    Ok(())
}

pub(crate) fn address(endpoint: &Path) -> Result<libc::sockaddr_un, TransportError> {
    // SAFETY: sockaddr_un is plain old data; all-zero is a valid value.
    let mut result: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    result.sun_family = libc::AF_UNIX as libc::sa_family_t;
    #[cfg(any(target_vendor = "apple", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    {
        result.sun_len = size_of::<libc::sockaddr_un>() as u8;
    }
    let bytes = endpoint.as_os_str().as_bytes();
    if bytes.len() + 1 > result.sun_path.len() {
        return Err(TransportError::InvalidEndpoint("socket path is too long"));
    }
    for (slot, byte) in result.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    Ok(result)
}

pub(crate) fn bind(fd: RawFd, endpoint: &Path) -> Result<(), TransportError> {
    let value = address(endpoint)?;
    // SAFETY: `value` is a complete sockaddr_un and the length matches it.
    let result = unsafe {
        libc::bind(fd, (&value as *const libc::sockaddr_un).cast(), size_of::<libc::sockaddr_un>() as libc::socklen_t)
    };
    if result != 0 {
        return Err(TransportError::System("Bind automation socket", errno()));
    }
    Ok(())
}

pub(crate) fn connect(fd: RawFd, endpoint: &Path, deadline: Instant) -> Result<(), TransportError> {
    let value = address(endpoint)?;
    // SAFETY: `value` is a complete sockaddr_un and the length matches it.
    let result = unsafe {
        libc::connect(
            fd,
            (&value as *const libc::sockaddr_un).cast(),
            size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let code = errno();
    if code != libc::EINPROGRESS && code != libc::EINTR && code != libc::EAGAIN {
        return Err(TransportError::System("Connect to Orbit Disk", code));
    }
    wait(fd, libc::POLLOUT, deadline)?;
    let mut error: libc::c_int = 0;
    let mut length = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `error` and `length` are valid for writes of the declared size.
    let status = unsafe {
        libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_ERROR, (&mut error as *mut libc::c_int).cast(), &mut length)
    };
    if status != 0 {
        return Err(TransportError::System("Check automation connection", errno()));
    }
    if error != 0 {
        return Err(TransportError::System("Connect to Orbit Disk", error));
    }
    Ok(())
}

pub(crate) fn effective_uid() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(any(target_vendor = "apple", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
fn peer_uid(fd: RawFd) -> Result<libc::uid_t, TransportError> {
    let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
    // SAFETY: both out-pointers are valid for writes.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(TransportError::System("Verify automation peer", errno()));
    }
    Ok(uid)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(fd: RawFd) -> Result<libc::uid_t, TransportError> {
    // SAFETY: ucred is plain old data; all-zero is a valid value.
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` and `length` are valid for writes of the declared size.
    let status = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if status != 0 {
        return Err(TransportError::System("Verify automation peer", errno()));
    }
    Ok(credentials.uid)
}

/// Rejects a peer whose effective user differs from `expected`.
pub(crate) fn verify_peer(fd: RawFd, expected: libc::uid_t) -> Result<(), TransportError> {
    if peer_uid(fd)? != expected {
        return Err(TransportError::PeerMismatch);
    }
    Ok(())
}

/// Waits until `fd` reports `events` or the absolute `deadline` passes.
pub(crate) fn wait(fd: RawFd, events: libc::c_short, deadline: Instant) -> Result<(), TransportError> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(TransportError::Timeout);
        }
        let remaining = deadline - now;
        let milliseconds = remaining.as_nanos().div_ceil(1_000_000).min(libc::c_int::MAX as u128) as libc::c_int;
        let mut descriptor = libc::pollfd { fd, events, revents: 0 };
        // SAFETY: one valid pollfd.
        let result = unsafe { libc::poll(&mut descriptor, 1, milliseconds) };
        if result < 0 {
            let code = errno();
            if code == libc::EINTR {
                continue;
            }
            return Err(TransportError::System("Wait for automation", code));
        }
        if result == 0 {
            return Err(TransportError::Timeout);
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err(TransportError::Disconnected);
        }
        return Ok(());
    }
}

/// Reads one frame: a 4-byte big-endian length followed by that many bytes.
pub(crate) fn read_frame(fd: RawFd, deadline: Instant) -> Result<Vec<u8>, TransportError> {
    let mut header = [0u8; 4];
    read_exact(fd, &mut header, deadline)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME {
        return Err(TransportError::OversizedMessage);
    }
    let mut body = vec![0u8; length];
    read_exact(fd, &mut body, deadline)?;
    Ok(body)
}

pub(crate) fn write_frame(fd: RawFd, data: &[u8], deadline: Instant) -> Result<(), TransportError> {
    if data.len() > MAX_FRAME {
        return Err(TransportError::OversizedMessage);
    }
    write_all(fd, &(data.len() as u32).to_be_bytes(), deadline)?;
    write_all(fd, data, deadline)
}

fn read_exact(fd: RawFd, buffer: &mut [u8], deadline: Instant) -> Result<(), TransportError> {
    let mut offset = 0;
    while offset < buffer.len() {
        wait(fd, libc::POLLIN, deadline)?;
        let rest = &mut buffer[offset..];
        // SAFETY: `rest` is valid for writes of its length.
        let result = unsafe { libc::recv(fd, rest.as_mut_ptr().cast(), rest.len(), 0) };
        if result > 0 {
            offset += result as usize;
            continue;
        }
        if result == 0 {
            return Err(TransportError::Disconnected);
        }
        let code = errno();
        if code != libc::EINTR && code != libc::EAGAIN {
            return Err(TransportError::System("Read automation message", code));
        }
    }
    Ok(())
}

fn write_all(fd: RawFd, data: &[u8], deadline: Instant) -> Result<(), TransportError> {
    let mut offset = 0;
    while offset < data.len() {
        wait(fd, libc::POLLOUT, deadline)?;
        let rest = &data[offset..];
        // SAFETY: `rest` is valid for reads of its length.
        let result = unsafe { libc::send(fd, rest.as_ptr().cast(), rest.len(), SEND_FLAGS) };
        if result > 0 {
            offset += result as usize;
            continue;
        }
        if result == 0 {
            return Err(TransportError::Disconnected);
        }
        let code = errno();
        if code != libc::EINTR && code != libc::EAGAIN {
            return Err(TransportError::System("Write automation message", code));
        }
    }
    Ok(())
}

/// True when the peer has closed its end (orderly shutdown or reset). Never blocks.
pub(crate) fn peer_closed(fd: RawFd) -> bool {
    let mut descriptor = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd; zero timeout.
    if unsafe { libc::poll(&mut descriptor, 1, 0) } <= 0 {
        return false;
    }
    let mut byte = 0u8;
    // SAFETY: one writable byte; MSG_PEEK leaves the stream unchanged.
    let result = unsafe { libc::recv(fd, (&mut byte as *mut u8).cast(), 1, libc::MSG_PEEK) };
    result == 0 || (result < 0 && !matches!(errno(), libc::EAGAIN | libc::EINTR))
}

pub(crate) fn shutdown(fd: RawFd) {
    // SAFETY: shutdown on a descriptor kept open by the caller; errors are irrelevant here.
    unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
}

pub(crate) fn c_string(bytes: &[u8]) -> Option<CString> {
    CString::new(bytes).ok()
}
