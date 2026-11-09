//! Previously completed listings that discovery may reuse instead of listing again (Swift
//! `DiscoveryCacheReader`). The durable-index owner implements these traits over a read-only
//! snapshot of the stage database; discovery only consumes them.

use crate::records::RecordBatch;
use orbit_platform::FileIdentity;

/// What the cache knows about one queued directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CachedListing {
    /// The complete listing, or `None` when the directory is known but must be listed again
    /// (unfinished or dirty). Its revision/generation still become the batch expectations.
    pub records: Option<RecordBatch>,
    /// The directory's `dirty.revision` the stage expects (0 = fresh).
    pub revision: i64,
    /// The full-refresh generation the listing belongs to.
    pub generation: i64,
}

/// Opens one reader per worker (SQLite handles are not shared across threads).
pub trait ListingCache: Send + Sync {
    /// A reader for the calling worker, or `None` when the cache cannot be used right now
    /// (busy, invalid, wrong root): discovery then lists the filesystem.
    fn open_reader(&self) -> Option<Box<dyn ListingCacheReader>>;
}

pub trait ListingCacheReader: Send {
    /// The full-refresh generation of the cached stage.
    fn generation(&self) -> i64;

    /// The cached state of the directory at `path` with `identity`, if the cache has it. Must
    /// not block on writers (no busy timeout).
    fn listing(&mut self, path: &[u8], identity: FileIdentity) -> Option<CachedListing>;
}
