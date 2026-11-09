use crate::{Cancellation, Result};

/// Records are delivered to [`ListingSink::batch`] in slices of at most this many entries.
pub const LISTING_BATCH: usize = 128;

/// Device + inode (or volume serial + file id on Windows). Identifies a filesystem object while
/// it exists; hard links share one identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EntryKind {
    File = 0,
    Directory = 1,
    Symlink = 2,
    /// Sockets, devices, FIFOs. Counted as zero-size leaves.
    Other = 3,
}

/// One directory entry as reported by the OS. Borrowed from the lister's buffer; copy what you
/// keep before returning from [`ListingSink::batch`].
#[derive(Clone, Copy, Debug)]
pub struct EntryRecord<'a> {
    /// Never empty, never `.`/`..`, never contains `/` (or `\` on Windows).
    pub name: &'a [u8],
    pub kind: EntryKind,
    pub id: FileIdentity,
    /// Hard-link count; `1` for directories.
    pub nlink: u32,
    /// Logical length in bytes (data fork on macOS); `0` for directories.
    pub size: u64,
    /// Allocated bytes on disk; `0` for directories.
    pub allocated: u64,
    /// The directory is the root of another mounted filesystem.
    pub is_mount_point: bool,
}

impl EntryRecord<'_> {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }
}

/// A per-entry problem that does not stop the listing (unreadable child, undecodable name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// Full path bytes of the affected item (or the directory when the name is unknown).
    pub path: Vec<u8>,
    /// Raw OS error, if any.
    pub code: Option<i32>,
    pub message: String,
}

/// Returned by a sink to end a listing early (cancellation or a downstream failure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop;

pub trait ListingSink {
    fn batch(&mut self, records: &[EntryRecord<'_>]) -> std::result::Result<(), Stop>;
    fn issue(&mut self, issue: Issue);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListingTotals {
    pub entries: u64,
    pub logical_bytes: u64,
}

/// Lists one open directory. Owned by a single worker; reuses its buffer across calls.
pub trait DirectoryLister: Send {
    type Dir;

    /// Streams every entry of `dir` to `sink` in batches of at most [`LISTING_BATCH`].
    ///
    /// `path` is the directory's full path, used only for issues and errors. Entries whose
    /// metadata cannot be read are reported through [`ListingSink::issue`] and skipped. The
    /// lister checks `cancel` at least every [`LISTING_BATCH`] entries and returns
    /// [`crate::PlatformError::Cancelled`]. A [`Stop`] from the sink also ends with `Cancelled`.
    fn list<S: ListingSink>(
        &mut self,
        dir: &Self::Dir,
        path: &[u8],
        cancel: &Cancellation,
        sink: &mut S,
    ) -> Result<ListingTotals>;
}
