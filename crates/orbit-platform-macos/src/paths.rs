//! Byte-path helpers shared by the volume layout and the change monitor. Paths are raw bytes;
//! nothing here assumes UTF-8.

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use orbit_platform::FileIdentity;
use orbit_platform::posix::stat_identity;

pub fn bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

pub fn to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

fn c_path(path: &[u8]) -> Option<CString> {
    CString::new(path).ok()
}

/// Lexical standardization of an absolute path (like `URL.standardizedFileURL.path`): collapses
/// repeated separators, drops `.`, applies `..` and removes a trailing separator.
pub fn standardize(path: &[u8]) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in path.split(|&b| b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    join_components(&parts)
}

pub fn join_components(parts: &[&[u8]]) -> Vec<u8> {
    if parts.is_empty() {
        return b"/".to_vec();
    }
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.len() + 1).sum());
    for part in parts {
        out.push(b'/');
        out.extend_from_slice(part);
    }
    out
}

/// `path` equals `root` or lies below it (component-wise, never by byte prefix alone).
pub fn contains(path: &[u8], root: &[u8]) -> bool {
    if path == root {
        return true;
    }
    if root == b"/" {
        return path.first() == Some(&b'/');
    }
    path.len() > root.len() && path.starts_with(root) && path[root.len()] == b'/'
}

pub fn trim_slashes(path: &[u8]) -> &[u8] {
    let start = path.iter().position(|&b| b != b'/').unwrap_or(path.len());
    let end = path.iter().rposition(|&b| b != b'/').map_or(start, |i| i + 1);
    &path[start..end.max(start)]
}

/// `mount` + `/` + `relative`, without doubling the separator for the root mount.
pub fn under_mount(mount: &[u8], relative: &[u8]) -> Vec<u8> {
    let mut out = if mount == b"/" { Vec::new() } else { mount.to_vec() };
    out.push(b'/');
    out.extend_from_slice(relative);
    out
}

/// The parent of an absolute standardized path; `/` is its own parent.
pub fn parent(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(0) | None => b"/",
        Some(index) => &path[..index],
    }
}

pub fn has_dot_dot(path: &[u8]) -> bool {
    path.split(|&b| b == b'/').any(|part| part == b"..")
}

pub fn child(dir: &[u8], name: &[u8]) -> Vec<u8> {
    orbit_platform::posix::join(dir, name)
}

/// `realpath(3)`; on failure returns the errno.
pub fn realpath(path: &[u8]) -> Result<Vec<u8>, i32> {
    let c = c_path(path).ok_or(libc::EINVAL)?;
    // SAFETY: `c` is NUL-terminated; a null buffer asks libc to allocate the result.
    let resolved = unsafe { libc::realpath(c.as_ptr(), std::ptr::null_mut()) };
    if resolved.is_null() {
        return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
    }
    // SAFETY: realpath returned a malloc'd NUL-terminated string that we free below.
    let out = unsafe { CStr::from_ptr(resolved) }.to_bytes().to_vec();
    // SAFETY: allocated by realpath with malloc.
    unsafe { libc::free(resolved.cast()) };
    Ok(out)
}

pub fn lstat(path: &[u8]) -> Option<libc::stat> {
    let c = c_path(path)?;
    // SAFETY: zeroed stat is valid POD.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated path, writable out-param.
    (unsafe { libc::lstat(c.as_ptr(), &mut info) } == 0).then_some(info)
}

pub fn stat(path: &[u8]) -> Option<libc::stat> {
    let c = c_path(path)?;
    // SAFETY: zeroed stat is valid POD.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated path, writable out-param.
    (unsafe { libc::stat(c.as_ptr(), &mut info) } == 0).then_some(info)
}

pub fn is_dir(info: &libc::stat) -> bool {
    info.st_mode & libc::S_IFMT == libc::S_IFDIR
}

/// Identity of the directory at `path` without following a final symlink.
pub fn directory_identity(path: &[u8]) -> Option<FileIdentity> {
    lstat(path).filter(is_dir).map(|info| stat_identity(&info))
}

/// Two paths name the same object (following symlinks).
pub fn same_file(first: &[u8], second: &[u8]) -> bool {
    match (stat(first), stat(second)) {
        (Some(a), Some(b)) => a.st_dev == b.st_dev && a.st_ino == b.st_ino,
        _ => false,
    }
}

/// `physical` and `alias` are the same object, or their nearest existing ancestors at equal depth
/// are. Used to recognize `/System/Volumes/Data/<x>` as the firmlinked `/<x>`.
pub fn matches_through_existing_ancestor(physical: &[u8], alias: &[u8]) -> bool {
    let mut first = physical.to_vec();
    let mut second = alias.to_vec();
    while second != b"/" {
        if same_file(&first, &second) {
            return true;
        }
        first = parent(&first).to_vec();
        second = parent(&second).to_vec();
    }
    false
}

pub fn statfs_path(path: &[u8]) -> Option<libc::statfs> {
    let c = c_path(path)?;
    // SAFETY: zeroed statfs is valid POD.
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated path, writable out-param.
    (unsafe { libc::statfs(c.as_ptr(), &mut info) } == 0).then_some(info)
}

/// `f_fsid` of a statfs record.
pub fn fsid(info: &libc::statfs) -> [i32; 2] {
    // SAFETY: `fsid_t` is `struct { int32_t val[2]; }`.
    unsafe { std::mem::transmute::<libc::fsid_t, [i32; 2]>(info.f_fsid) }
}

/// `f_mntonname` of a statfs record, as raw bytes.
pub fn mount_on_name(info: &libc::statfs) -> Vec<u8> {
    // SAFETY: f_mntonname is a NUL-terminated C array inside `info`.
    unsafe { CStr::from_ptr(info.f_mntonname.as_ptr()) }.to_bytes().to_vec()
}

/// Mount point of the filesystem holding `path` (follows symlinks, like `statfs`).
pub fn mount_of(path: &[u8]) -> Option<Vec<u8>> {
    statfs_path(path).map(|info| mount_on_name(&info))
}

/// Every mounted filesystem, without blocking on unresponsive network mounts.
pub fn mounted_filesystems() -> Vec<libc::statfs> {
    for _ in 0..4 {
        // SAFETY: a null buffer asks only for the count.
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if count <= 0 {
            return Vec::new();
        }
        let capacity = count as usize + 8;
        // SAFETY: zeroed statfs is valid POD.
        let mut buffer: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; capacity];
        let bytes = (capacity * std::mem::size_of::<libc::statfs>()) as libc::c_int;
        // SAFETY: `buffer` holds `capacity` writable records.
        let filled = unsafe { libc::getfsstat(buffer.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
        if filled < 0 {
            return Vec::new();
        }
        if (filled as usize) < capacity {
            buffer.truncate(filled as usize);
            return buffer;
        }
    }
    Vec::new()
}

pub fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_is_component_wise() {
        assert!(contains(b"/a/b", b"/a"));
        assert!(contains(b"/a", b"/a"));
        assert!(contains(b"/x", b"/"));
        assert!(!contains(b"/ab", b"/a"));
        assert!(!contains(b"/a", b"/a/b"));
        assert_eq!(standardize(b"/a//b/./c/../d/"), b"/a/b/d");
        assert_eq!(standardize(b"/.."), b"/");
    }
}
