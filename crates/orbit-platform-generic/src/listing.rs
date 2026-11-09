//! `readdir` + `fstatat` listing. Names are copied into a reused arena so a batch can outlive the
//! `dirent` it came from; nothing is allocated per entry once the arena has grown.

use std::ffi::CStr;
use std::os::fd::{AsRawFd, OwnedFd};

use orbit_platform::{
    Cancellation, DirectoryLister, EntryKind, EntryRecord, FileIdentity, Issue, LISTING_BATCH, ListingSink,
    ListingTotals, PlatformError, Result, posix,
};

use crate::{clear_errno, errno};

const DIR_OPEN_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

#[derive(Debug, Default)]
pub struct GenericLister {
    names: Vec<u8>,
    pending: Vec<Pending>,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    name_start: usize,
    name_len: usize,
    kind: EntryKind,
    id: FileIdentity,
    nlink: u32,
    size: u64,
    allocated: u64,
    is_mount_point: bool,
}

const EMPTY: EntryRecord<'static> = EntryRecord {
    name: b"",
    kind: EntryKind::File,
    id: FileIdentity { dev: 0, ino: 0 },
    nlink: 0,
    size: 0,
    allocated: 0,
    is_mount_point: false,
};

struct Stream(*mut libc::DIR);

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: the stream is open and closed exactly once here.
        unsafe { libc::closedir(self.0) };
    }
}

fn issue(path: &[u8], name: Option<&[u8]>, code: Option<i32>, message: String) -> Issue {
    Issue { path: name.map_or_else(|| path.to_vec(), |name| posix::join(path, name)), code, message }
}

impl GenericLister {
    fn flush<S: ListingSink>(&mut self, sink: &mut S) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut batch = [EMPTY; LISTING_BATCH];
        for (slot, entry) in batch.iter_mut().zip(&self.pending) {
            *slot = EntryRecord {
                name: &self.names[entry.name_start..entry.name_start + entry.name_len],
                kind: entry.kind,
                id: entry.id,
                nlink: entry.nlink,
                size: entry.size,
                allocated: entry.allocated,
                is_mount_point: entry.is_mount_point,
            };
        }
        let result = sink.batch(&batch[..self.pending.len()]).map_err(|_| PlatformError::Cancelled);
        self.pending.clear();
        self.names.clear();
        result
    }
}

impl DirectoryLister for GenericLister {
    type Dir = OwnedFd;

    fn list<S: ListingSink>(
        &mut self,
        dir: &OwnedFd,
        path: &[u8],
        cancel: &Cancellation,
        sink: &mut S,
    ) -> Result<ListingTotals> {
        let error_path = || posix::path_from_bytes(path).to_path_buf();
        // A fresh descriptor gives this listing its own offset, so listings can repeat.
        // SAFETY: live descriptor; "." is a static C string.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), c".".as_ptr(), DIR_OPEN_FLAGS) };
        if fd < 0 {
            return Err(PlatformError::os(error_path(), errno()));
        }
        // SAFETY: zeroed stat is valid POD.
        let mut parent: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: live descriptor, writable out-param.
        if unsafe { libc::fstat(fd, &mut parent) } != 0 {
            let code = errno();
            // SAFETY: we own `fd`.
            unsafe { libc::close(fd) };
            return Err(PlatformError::os(error_path(), code));
        }
        let parent_dev = posix::stat_identity(&parent).dev;
        // SAFETY: `fd` is a fresh directory descriptor; on success the stream owns it.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let code = errno();
            // SAFETY: fdopendir failed, so we still own `fd`.
            unsafe { libc::close(fd) };
            return Err(PlatformError::os(error_path(), code));
        }
        let stream = Stream(stream);
        self.names.clear();
        self.pending.clear();
        let mut totals = ListingTotals::default();
        loop {
            if cancel.is_cancelled() {
                return Err(PlatformError::Cancelled);
            }
            clear_errno();
            // SAFETY: `stream.0` is an open directory stream.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if code != 0 {
                    return Err(PlatformError::os(error_path(), code));
                }
                self.flush(sink)?;
                return Ok(totals);
            }
            // SAFETY: readdir returned a valid dirent whose `d_name` is NUL-terminated and stays
            // valid until the next readdir call on this stream.
            let c_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            let name = c_name.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if name.is_empty() || name.contains(&b'/') {
                sink.issue(issue(path, None, None, "a file name could not be decoded".into()));
                continue;
            }
            // SAFETY: zeroed stat is valid POD.
            let mut info: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: live descriptor, NUL-terminated name, writable out-param.
            let status =
                unsafe { libc::fstatat(libc::dirfd(stream.0), c_name.as_ptr(), &mut info, libc::AT_SYMLINK_NOFOLLOW) };
            if status != 0 {
                let code = errno();
                sink.issue(issue(path, Some(name), Some(code), std::io::Error::from_raw_os_error(code).to_string()));
                continue;
            }
            let kind = match info.st_mode & libc::S_IFMT {
                libc::S_IFDIR => EntryKind::Directory,
                libc::S_IFREG => EntryKind::File,
                libc::S_IFLNK => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
            let is_dir = kind == EntryKind::Directory;
            let id = posix::stat_identity(&info);
            // `st_nlink` width differs between Unix flavours.
            #[allow(clippy::unnecessary_cast)]
            let nlink = if is_dir { 1 } else { (info.st_nlink as u64).min(u64::from(u32::MAX)) as u32 };
            let size = if is_dir { 0 } else { info.st_size.max(0) as u64 };
            #[allow(clippy::unnecessary_cast)]
            let allocated = if is_dir { 0 } else { (info.st_blocks as i64).max(0) as u64 * 512 };
            totals.entries += 1;
            totals.logical_bytes += size;
            self.pending.push(Pending {
                name_start: self.names.len(),
                name_len: name.len(),
                kind,
                id,
                nlink,
                size,
                allocated,
                is_mount_point: is_dir && id.dev != parent_dev,
            });
            self.names.extend_from_slice(name);
            if self.pending.len() == LISTING_BATCH {
                self.flush(sink)?;
            }
        }
    }
}
