//! Directory listing with `getattrlistbulk(2)`, falling back to `readdir` + `fstatat` on
//! filesystems without bulk attribute support.
//!
//! The bulk record layout below follows from the requested attribute set (attributes are packed
//! in bit order, `FSOPT_PACK_INVAL_ATTRS` keeps every requested slot present):
//!
//! | offset | attribute                                   |
//! |--------|---------------------------------------------|
//! | 0      | record length (u32)                         |
//! | 4      | returned `attribute_set_t` (5 × u32)        |
//! | 24     | `ATTR_CMN_ERROR` (u32)                      |
//! | 28     | `ATTR_CMN_NAME` (`attrreference_t`)         |
//! | 36     | `ATTR_CMN_DEVID` (i32)                      |
//! | 40     | `ATTR_CMN_OBJTYPE` (u32)                    |
//! | 44     | `ATTR_CMN_FILEID` (u64)                     |
//! | 52     | dir: `ATTR_DIR_MOUNTSTATUS` (u32)           |
//! | 52     | file: `ATTR_FILE_LINKCOUNT` (u32)           |
//! | 56     | file: `ATTR_FILE_ALLOCSIZE` (i64)           |
//! | 64     | file: `ATTR_FILE_DATALENGTH` (i64)          |
//!
//! Darwin omits the whole file or directory group for the other object kind, so the name data
//! starts at 56 for directories and 72 for everything else.

use std::ffi::{CStr, c_void};
use std::os::fd::{AsRawFd, OwnedFd};

use orbit_platform::{
    Cancellation, DirectoryLister, EntryKind, EntryRecord, FileIdentity, Issue, LISTING_BATCH, ListingSink,
    ListingTotals, PlatformError, Result,
};

use crate::ffi;
use crate::firmlinks::{FirmlinkDirectories, Link, verified_identity};
use crate::paths;

const BUFFER_BYTES: usize = 256 * 1024;
const FIXED_HEADER: usize = 52;

const COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | ffi::ATTR_CMN_ERROR
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_FILEID;
const REQUIRED_COMMON: u32 = libc::ATTR_CMN_DEVID | libc::ATTR_CMN_OBJTYPE | libc::ATTR_CMN_FILEID;
const FILE: u32 = libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;

const DIR_OPEN_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// Which enumeration strategy a lister uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListingMode {
    /// `getattrlistbulk`, falling back automatically when the filesystem does not support it.
    Bulk,
    /// Always `readdir` + `fstatat` (tests and benchmarks).
    Fallback,
}

/// Per-worker lister. Owns one 256 KiB bulk buffer plus small arenas for the fallback path; none
/// of them are reallocated per entry.
pub struct MacLister {
    buffer: Box<[u64]>,
    names: Vec<u8>,
    pending: Vec<Pending>,
    mode: ListingMode,
    firmlinks: &'static FirmlinkDirectories,
}

impl std::fmt::Debug for MacLister {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacLister").field("mode", &self.mode).finish_non_exhaustive()
    }
}

impl MacLister {
    pub fn new(mode: ListingMode) -> Self {
        Self {
            buffer: vec![0u64; BUFFER_BYTES / 8].into_boxed_slice(),
            names: Vec::with_capacity(LISTING_BATCH * 32),
            pending: Vec::with_capacity(LISTING_BATCH),
            mode,
            firmlinks: FirmlinkDirectories::system(),
        }
    }
}

#[derive(Clone, Copy)]
struct Metadata {
    kind: EntryKind,
    id: FileIdentity,
    nlink: u32,
    size: u64,
    allocated: u64,
    is_mount_point: bool,
}

#[derive(Clone, Copy)]
struct Pending {
    name_start: usize,
    name_len: usize,
    metadata: Metadata,
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

/// A fixed batch on the stack whose names borrow the lister's buffers.
struct Batch<'a> {
    items: [EntryRecord<'a>; LISTING_BATCH],
    len: usize,
}

impl<'a> Batch<'a> {
    fn new() -> Self {
        Self { items: [EMPTY; LISTING_BATCH], len: 0 }
    }

    fn push(&mut self, name: &'a [u8], metadata: Metadata) {
        self.items[self.len] = EntryRecord {
            name,
            kind: metadata.kind,
            id: metadata.id,
            nlink: metadata.nlink,
            size: metadata.size,
            allocated: metadata.allocated,
            is_mount_point: metadata.is_mount_point,
        };
        self.len += 1;
    }

    fn is_full(&self) -> bool {
        self.len == LISTING_BATCH
    }

    fn flush<S: ListingSink>(&mut self, sink: &mut S) -> Result<()> {
        if self.len == 0 {
            return Ok(());
        }
        let len = std::mem::take(&mut self.len);
        sink.batch(&self.items[..len]).map_err(|_| PlatformError::Cancelled)
    }
}

impl DirectoryLister for MacLister {
    type Dir = OwnedFd;

    fn list<S: ListingSink>(
        &mut self,
        dir: &OwnedFd,
        path: &[u8],
        cancel: &Cancellation,
        sink: &mut S,
    ) -> Result<ListingTotals> {
        let links = self.firmlinks.in_directory(path);
        match self.mode {
            ListingMode::Fallback => self.list_fallback(dir, path, links, cancel, sink),
            ListingMode::Bulk => match self.list_bulk(dir, path, links, cancel, sink)? {
                Some(totals) => Ok(totals),
                None => self.list_fallback(dir, path, links, cancel, sink),
            },
        }
    }
}

fn u32_at(record: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes(record[offset..offset + 4].try_into().expect("4 bytes"))
}

fn i32_at(record: &[u8], offset: usize) -> i32 {
    i32::from_ne_bytes(record[offset..offset + 4].try_into().expect("4 bytes"))
}

fn u64_at(record: &[u8], offset: usize) -> u64 {
    u64::from_ne_bytes(record[offset..offset + 8].try_into().expect("8 bytes"))
}

fn i64_at(record: &[u8], offset: usize) -> i64 {
    i64::from_ne_bytes(record[offset..offset + 8].try_into().expect("8 bytes"))
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

fn issue_for(path: &[u8], name: Option<&[u8]>, code: i32) -> Issue {
    Issue {
        path: name.map_or_else(|| path.to_vec(), |name| paths::child(path, name)),
        code: Some(code),
        message: std::io::Error::from_raw_os_error(code).to_string(),
    }
}

fn dev_from(raw: i32) -> u64 {
    // Same widening as `posix::stat_identity`: through the unsigned 32-bit pattern.
    u64::from(raw as u32)
}

impl MacLister {
    /// `Ok(None)` asks for the fallback: the filesystem rejected bulk enumeration before any
    /// record was delivered.
    fn list_bulk<S: ListingSink>(
        &mut self,
        dir: &OwnedFd,
        path: &[u8],
        links: Option<&std::collections::HashMap<Vec<u8>, Link>>,
        cancel: &Cancellation,
        sink: &mut S,
    ) -> Result<Option<ListingTotals>> {
        let fd = dir.as_raw_fd();
        // getattrlistbulk enumerates from the descriptor's offset; rewind so a listing can repeat.
        // SAFETY: live descriptor owned by the caller.
        if unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } < 0 {
            return Err(PlatformError::os(paths::to_path(path), errno()));
        }
        let mut attributes = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: COMMON,
            volattr: 0,
            dirattr: libc::ATTR_DIR_MOUNTSTATUS,
            fileattr: FILE,
            forkattr: 0,
        };
        let mut totals = ListingTotals::default();
        let mut first_fill = true;
        loop {
            if cancel.is_cancelled() {
                return Err(PlatformError::Cancelled);
            }
            let storage = self.buffer.as_mut_ptr().cast::<c_void>();
            // SAFETY: `attributes` and the 256 KiB buffer are valid for the call; the kernel
            // writes at most `BUFFER_BYTES` bytes.
            let count = orbit_platform::posix::retry_interrupted(|| unsafe {
                libc::getattrlistbulk(
                    fd,
                    (&raw mut attributes).cast(),
                    storage,
                    BUFFER_BYTES,
                    u64::from(libc::FSOPT_PACK_INVAL_ATTRS),
                )
            });
            if count < 0 {
                let code = errno();
                if first_fill && matches!(code, libc::ENOTSUP | libc::EINVAL | libc::ENOSYS) {
                    return Ok(None);
                }
                return Err(PlatformError::os(paths::to_path(path), code));
            }
            if count == 0 {
                return Ok(Some(totals));
            }
            first_fill = false;
            // SAFETY: the buffer is BUFFER_BYTES of initialized u64s viewed as bytes.
            let buffer: &[u8] = unsafe { std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), BUFFER_BYTES) };
            let mut batch = Batch::new();
            let mut offset = 0usize;
            for index in 0..count as usize {
                if index & (LISTING_BATCH - 1) == 0 && cancel.is_cancelled() {
                    return Err(PlatformError::Cancelled);
                }
                let corrupt = || PlatformError::os(paths::to_path(path), libc::EIO);
                if offset + 4 > BUFFER_BYTES {
                    return Err(corrupt());
                }
                let length = u32_at(buffer, offset) as usize;
                if length < FIXED_HEADER || length > BUFFER_BYTES - offset {
                    return Err(corrupt());
                }
                let record = &buffer[offset..offset + length];
                offset += length;
                let returned_common = u32_at(record, 4);
                let returned_dir = u32_at(record, 12);
                let returned_file = u32_at(record, 16);
                let error = u32_at(record, 24);
                let name_start = 28isize + i32_at(record, 28) as isize;
                let name_len = u32_at(record, 32) as usize;
                if returned_common & libc::ATTR_CMN_NAME == 0
                    || name_start < FIXED_HEADER as isize
                    || name_len == 0
                    || name_start as usize > length
                    || name_len > length - name_start as usize
                    || record[name_start as usize + name_len - 1] != 0
                {
                    return Err(corrupt());
                }
                let name_start = name_start as usize;
                let c_name =
                    CStr::from_bytes_until_nul(&record[name_start..name_start + name_len]).map_err(|_| corrupt())?;
                let name = c_name.to_bytes();
                if name.is_empty() || name.len() != name_len - 1 || name.contains(&b'/') {
                    sink.issue(Issue {
                        path: path.to_vec(),
                        code: None,
                        message: "a file name could not be decoded".into(),
                    });
                    continue;
                }
                if name == b"." || name == b".." {
                    continue;
                }
                if returned_common & ffi::ATTR_CMN_ERROR != 0 && error != 0 {
                    sink.issue(issue_for(path, Some(name), error as i32));
                    continue;
                }
                let kind = u32_at(record, 40);
                let is_dir = kind == ffi::VDIR;
                let complete = returned_common & REQUIRED_COMMON == REQUIRED_COMMON
                    && if is_dir {
                        returned_dir & libc::ATTR_DIR_MOUNTSTATUS != 0
                    } else {
                        returned_file & FILE == FILE
                    };
                let metadata = if complete {
                    if name_start < if is_dir { 56 } else { 72 } {
                        return Err(corrupt());
                    }
                    let id = FileIdentity { dev: dev_from(i32_at(record, 36)), ino: u64_at(record, 44) };
                    if is_dir {
                        let is_mount_point = u32_at(record, 52) & libc::DIR_MNTSTATUS_MNTPOINT != 0;
                        // Bulk records describe the covered directory of a mount point; report
                        // the mounted root that a no-follow open reaches (as the fallback does).
                        let id = if is_mount_point {
                            match stat_at(fd, c_name) {
                                Ok(info) if paths::is_dir(&info) => orbit_platform::posix::stat_identity(&info),
                                Ok(_) => {
                                    sink.issue(issue_for(path, Some(name), libc::EAGAIN));
                                    continue;
                                }
                                Err(code) => {
                                    sink.issue(issue_for(path, Some(name), code));
                                    continue;
                                }
                            }
                        } else {
                            id
                        };
                        Metadata { kind: EntryKind::Directory, id, nlink: 1, size: 0, allocated: 0, is_mount_point }
                    } else {
                        Metadata {
                            kind: match kind {
                                ffi::VREG => EntryKind::File,
                                ffi::VLNK => EntryKind::Symlink,
                                _ => EntryKind::Other,
                            },
                            id,
                            nlink: u32_at(record, 52),
                            size: i64_at(record, 64).max(0) as u64,
                            allocated: i64_at(record, 56).max(0) as u64,
                            is_mount_point: false,
                        }
                    }
                } else {
                    if cancel.is_cancelled() {
                        return Err(PlatformError::Cancelled);
                    }
                    match read_at(fd, c_name) {
                        Ok(metadata) => metadata,
                        Err(code) => {
                            sink.issue(issue_for(path, Some(name), code));
                            continue;
                        }
                    }
                };
                let Some(metadata) = normalize_firmlink(fd, links, c_name, metadata, sink) else { continue };
                totals.entries += 1;
                totals.logical_bytes += metadata.size;
                batch.push(name, metadata);
                if batch.is_full() {
                    batch.flush(sink)?;
                }
            }
            // Names borrow the buffer, which the next call overwrites.
            batch.flush(sink)?;
        }
    }

    fn list_fallback<S: ListingSink>(
        &mut self,
        dir: &OwnedFd,
        path: &[u8],
        links: Option<&std::collections::HashMap<Vec<u8>, Link>>,
        cancel: &Cancellation,
        sink: &mut S,
    ) -> Result<ListingTotals> {
        let error_path = || paths::to_path(path);
        // A separate descriptor keeps readdir's offset independent of the caller's.
        // SAFETY: live descriptor; "." is a static C string.
        let fd = orbit_platform::posix::retry_interrupted(|| unsafe {
            libc::openat(dir.as_raw_fd(), c".".as_ptr(), DIR_OPEN_FLAGS)
        });
        if fd < 0 {
            return Err(PlatformError::os(error_path(), errno()));
        }
        // SAFETY: `fd` is a fresh directory descriptor; on success the stream owns it.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let code = errno();
            // SAFETY: fdopendir failed, so we still own `fd`.
            unsafe { libc::close(fd) };
            return Err(PlatformError::os(error_path(), code));
        }
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                // SAFETY: the stream is open and closed exactly once here.
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Stream(stream);
        self.names.clear();
        self.pending.clear();
        let mut totals = ListingTotals::default();
        loop {
            if cancel.is_cancelled() {
                return Err(PlatformError::Cancelled);
            }
            // SAFETY: errno is thread-local.
            unsafe { *libc::__error() = 0 };
            // SAFETY: `stream.0` is an open directory stream.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let code = errno();
                // SAFETY: reading thread-local errno.
                if unsafe { *libc::__error() } != 0 {
                    return Err(PlatformError::os(error_path(), code));
                }
                self.flush_pending(sink)?;
                return Ok(totals);
            }
            // SAFETY: readdir returned a valid dirent that stays valid until the next call;
            // `d_name` is NUL-terminated and `d_namlen` bytes long.
            let (c_name, name) = unsafe {
                let entry = &*entry;
                let c_name = CStr::from_ptr(entry.d_name.as_ptr());
                (c_name, std::slice::from_raw_parts(entry.d_name.as_ptr().cast::<u8>(), usize::from(entry.d_namlen)))
            };
            if name.is_empty() || name.contains(&b'/') || c_name.to_bytes().len() != name.len() {
                sink.issue(Issue {
                    path: path.to_vec(),
                    code: None,
                    message: "a file name could not be decoded".into(),
                });
                continue;
            }
            if name == b"." || name == b".." {
                continue;
            }
            let dir_fd = stream_fd(stream.0);
            let metadata = match read_at(dir_fd, c_name) {
                Ok(metadata) => metadata,
                Err(code) => {
                    sink.issue(issue_for(path, Some(name), code));
                    continue;
                }
            };
            let Some(metadata) = normalize_firmlink(dir_fd, links, c_name, metadata, sink) else { continue };
            totals.entries += 1;
            totals.logical_bytes += metadata.size;
            self.pending.push(Pending { name_start: self.names.len(), name_len: name.len(), metadata });
            self.names.extend_from_slice(name);
            if self.pending.len() == LISTING_BATCH {
                self.flush_pending(sink)?;
            }
        }
    }

    fn flush_pending<S: ListingSink>(&mut self, sink: &mut S) -> Result<()> {
        let mut batch = Batch::new();
        for pending in &self.pending {
            batch.push(&self.names[pending.name_start..pending.name_start + pending.name_len], pending.metadata);
        }
        let result = batch.flush(sink);
        self.pending.clear();
        self.names.clear();
        result
    }
}

fn stream_fd(stream: *mut libc::DIR) -> libc::c_int {
    // SAFETY: `stream` is an open directory stream.
    unsafe { libc::dirfd(stream) }
}

/// Firmlinked folders listed in their System-volume parent report the stub's identity; replace it
/// with the verified Data-volume folder a no-follow open reaches. `None` skips the entry.
fn normalize_firmlink<S: ListingSink>(
    fd: libc::c_int,
    links: Option<&std::collections::HashMap<Vec<u8>, Link>>,
    c_name: &CStr,
    metadata: Metadata,
    sink: &mut S,
) -> Option<Metadata> {
    let Some(link) = links.and_then(|links| links.get(c_name.to_bytes())) else { return Some(metadata) };
    if metadata.kind != EntryKind::Directory {
        return Some(metadata);
    }
    let failure = |code: i32| Issue {
        path: link.logical.clone(),
        code: Some(code),
        message: std::io::Error::from_raw_os_error(code).to_string(),
    };
    let opened = match stat_at(fd, c_name) {
        Ok(info) => info,
        Err(code) => {
            sink.issue(failure(code));
            return None;
        }
    };
    let actual = orbit_platform::posix::stat_identity(&opened);
    if !paths::is_dir(&opened) || verified_identity(link) != Some(actual) {
        sink.issue(failure(libc::EAGAIN));
        return None;
    }
    Some(Metadata { id: actual, is_mount_point: false, ..metadata })
}

fn stat_at(fd: libc::c_int, name: &CStr) -> std::result::Result<libc::stat, i32> {
    // SAFETY: zeroed stat is valid POD.
    let mut info: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: live descriptor, NUL-terminated name, writable out-param.
    if orbit_platform::posix::retry_interrupted(|| unsafe {
        libc::fstatat(fd, name.as_ptr(), &mut info, libc::AT_SYMLINK_NOFOLLOW)
    }) != 0
    {
        return Err(errno());
    }
    Ok(info)
}

/// Metadata for one entry through `fstatat`, with mount detection for directories. Errors are
/// raw errno values reported as issues by the caller.
fn read_at(fd: libc::c_int, name: &CStr) -> std::result::Result<Metadata, i32> {
    let info = stat_at(fd, name)?;
    let format = info.st_mode & libc::S_IFMT;
    let is_dir = format == libc::S_IFDIR;
    let id = orbit_platform::posix::stat_identity(&info);
    let mut is_mount_point = false;
    if is_dir {
        // SAFETY: live descriptor, NUL-terminated name.
        let child =
            orbit_platform::posix::retry_interrupted(|| unsafe { libc::openat(fd, name.as_ptr(), DIR_OPEN_FLAGS) });
        if child >= 0 {
            struct Close(libc::c_int);
            impl Drop for Close {
                fn drop(&mut self) {
                    // SAFETY: we own this descriptor.
                    unsafe { libc::close(self.0) };
                }
            }
            let _close = Close(child);
            // SAFETY: zeroed stat is valid POD.
            let mut child_info: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: live descriptor, writable out-param.
            if orbit_platform::posix::retry_interrupted(|| unsafe { libc::fstat(child, &mut child_info) }) != 0
                || orbit_platform::posix::stat_identity(&child_info) != id
            {
                return Err(libc::EAGAIN);
            }
            let mut attributes = libc::attrlist {
                bitmapcount: libc::ATTR_BIT_MAP_COUNT,
                reserved: 0,
                commonattr: 0,
                volattr: 0,
                dirattr: libc::ATTR_DIR_MOUNTSTATUS,
                fileattr: 0,
                forkattr: 0,
            };
            let mut value = [0u32; 2];
            // SAFETY: `attributes` and the 8-byte out buffer are valid for the call.
            let status = orbit_platform::posix::retry_interrupted(|| unsafe {
                libc::fgetattrlist(
                    child,
                    (&raw mut attributes).cast(),
                    value.as_mut_ptr().cast(),
                    std::mem::size_of_val(&value),
                    0,
                )
            });
            if status == 0 && value[0] >= 8 {
                is_mount_point = value[1] & libc::DIR_MNTSTATUS_MNTPOINT != 0;
            } else {
                // SAFETY: zeroed statfs is valid POD.
                let (mut parent_fs, mut child_fs): (libc::statfs, libc::statfs) =
                    unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
                // SAFETY: live descriptors, writable out-params.
                let failed = orbit_platform::posix::retry_interrupted(|| unsafe { libc::fstatfs(fd, &mut parent_fs) })
                    != 0
                    || orbit_platform::posix::retry_interrupted(|| unsafe { libc::fstatfs(child, &mut child_fs) }) != 0;
                if failed {
                    return Err(errno());
                }
                is_mount_point = paths::fsid(&parent_fs) != paths::fsid(&child_fs);
            }
        } else {
            let code = errno();
            if code != libc::EACCES && code != libc::EPERM {
                return Err(code);
            }
        }
    }
    let kind = match format {
        libc::S_IFDIR => EntryKind::Directory,
        libc::S_IFREG => EntryKind::File,
        libc::S_IFLNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    Ok(Metadata {
        kind,
        id,
        nlink: if is_dir { 1 } else { u32::from(info.st_nlink) },
        size: if is_dir { 0 } else { info.st_size.max(0) as u64 },
        allocated: if is_dir { 0 } else { (info.st_blocks.max(0) as u64).saturating_mul(512) },
        is_mount_point,
    })
}
