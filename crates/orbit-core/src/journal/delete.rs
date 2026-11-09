//! Permanent deletion that never follows symbolic links.
//!
//! The item is located through its parent directory descriptor, checked against the identity the
//! user collected, and removed with `unlinkat`. Directories are emptied through `openat` /
//! `fstatat(AT_SYMLINK_NOFOLLOW)` / `unlinkat`: a symlink anywhere in the tree (including one that
//! points at a directory) is unlinked as an entry and its target is never visited. Descent stops
//! at filesystem boundaries (a mount point inside the item is reported, not emptied).
//!
//! Residual race (same as the Swift app): POSIX has no "unlink this inode", so a *file* swapped in
//! under the same name between the identity check and `unlinkat` would be removed instead.
//! Directories are opened with an identity check, so their contents are always the checked ones.

use crate::paths;
use orbit_platform::posix as sys;
use orbit_platform::posix::{self, join, path_from_bytes};
use orbit_platform::{FileIdentity, PlatformError};
use std::io;
use std::os::fd::{AsFd, OwnedFd};

fn os_error(path: &[u8], error: &io::Error) -> PlatformError {
    PlatformError::os(path_from_bytes(path), error.raw_os_error().unwrap_or(libc::EIO))
}

/// Removes `path` (file, symlink or directory tree) if it still has `expected` identity and its
/// parent still has `expected_parent` (when given). Returns the first error; on a directory, the
/// remaining entries are still attempted so a partial failure removes as much as is safe.
pub fn remove_no_follow(
    path: &[u8],
    expected: FileIdentity,
    expected_parent: Option<FileIdentity>,
) -> Result<(), PlatformError> {
    let name = paths::file_name(path);
    let Some(parent) = paths::parent(path) else {
        return Err(PlatformError::os(path_from_bytes(path), libc::EPERM));
    };
    if name.is_empty() || name == b"." || name == b".." {
        return Err(PlatformError::os(path_from_bytes(path), libc::EINVAL));
    }
    let (parent_fd, parent_identity) = sys::open_root(path_from_bytes(parent)).map_err(|e| match e {
        PlatformError::Os { code, .. } if code == libc::ELOOP || code == libc::ENOTDIR => {
            PlatformError::IdentityChanged { path: path_from_bytes(parent).to_path_buf() }
        }
        other => other,
    })?;
    if expected_parent.is_some_and(|p| p != parent_identity) {
        return Err(PlatformError::IdentityChanged { path: path_from_bytes(parent).to_path_buf() });
    }
    let info = sys::fstat_at(parent_fd.as_fd(), name).map_err(|e| os_error(path, &e))?;
    if info.identity != expected {
        return Err(PlatformError::IdentityChanged { path: path_from_bytes(path).to_path_buf() });
    }
    if !info.is_dir() {
        return sys::unlink_at(parent_fd.as_fd(), name, false).map_err(|e| os_error(path, &e));
    }
    let dir = posix::open_child(parent_fd.as_fd(), name, expected, path_from_bytes(path))?;
    let mut first_error = empty_directory(dir, path, expected.dev).err();
    if let Err(e) = sys::unlink_at(parent_fd.as_fd(), name, true) {
        first_error.get_or_insert_with(|| os_error(path, &e));
    }
    first_error.map_or(Ok(()), Err)
}

struct Level {
    fd: OwnedFd,
    path: Vec<u8>,
    names: Vec<Vec<u8>>,
}

/// Empties an opened directory without following symlinks. Iterative so deep trees cannot
/// overflow the stack; holds one descriptor per level of the current branch.
fn empty_directory(dir: OwnedFd, path: &[u8], device: u64) -> Result<(), PlatformError> {
    let mut first_error: Option<PlatformError> = None;
    let names = sys::read_names(dir.as_fd()).map_err(|e| os_error(path, &e))?;
    let mut stack = vec![Level { fd: dir, path: path.to_vec(), names }];
    while let Some(top) = stack.last_mut() {
        let Some(name) = top.names.pop() else {
            let done = stack.pop().expect("non-empty");
            drop(done.fd);
            if let Some(parent) = stack.last()
                && let Err(e) = sys::unlink_at(parent.fd.as_fd(), paths::file_name(&done.path), true)
            {
                first_error.get_or_insert_with(|| os_error(&done.path, &e));
            }
            continue;
        };
        let child_path = join(&top.path, &name);
        let info = match sys::fstat_at(top.fd.as_fd(), &name) {
            Ok(info) => info,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => continue,
            Err(e) => {
                first_error.get_or_insert_with(|| os_error(&child_path, &e));
                continue;
            }
        };
        if !info.is_dir() {
            if let Err(e) = sys::unlink_at(top.fd.as_fd(), &name, false)
                && e.raw_os_error() != Some(libc::ENOENT)
            {
                first_error.get_or_insert_with(|| os_error(&child_path, &e));
            }
            continue;
        }
        if info.identity.dev != device {
            first_error.get_or_insert_with(|| PlatformError::os(path_from_bytes(&child_path), libc::EXDEV));
            continue;
        }
        let opened =
            posix::open_child(top.fd.as_fd(), &name, info.identity, path_from_bytes(&child_path)).and_then(|fd| {
                match sys::read_names(fd.as_fd()) {
                    Ok(names) => Ok((fd, names)),
                    Err(e) => Err(os_error(&child_path, &e)),
                }
            });
        match opened {
            Ok((fd, names)) => stack.push(Level { fd, path: child_path, names }),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}
