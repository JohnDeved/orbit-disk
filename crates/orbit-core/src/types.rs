use orbit_platform::{FileIdentity, PlatformError};

pub type Result<T, E = CoreError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Platform(#[from] PlatformError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
    /// Another process or session holds the stage / root lock.
    #[error("{0} is in use by another mapping")]
    InUse(String),
    /// The saved map changed underneath an operation that expected a specific snapshot.
    #[error("the saved map changed; refresh and try again")]
    SnapshotChanged,
    #[error("{0}")]
    Invalid(String),
}

impl CoreError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled | Self::Platform(PlatformError::Cancelled))
    }
}

/// An item removed by the user (Trash or delete) that views must hide until the index catches
/// up. Hidden only while the identity still matches the indexed row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScanRemoval {
    pub path: Vec<u8>,
    pub identity: FileIdentity,
}

/// Per-node mapping state for views built while discovery is still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum ScanState {
    /// Queued, never listed: shown with an explicit "waiting" label and no chart area.
    Pending = 0,
    /// Listing has started; totals are "so far".
    Scanning = 1,
    #[default]
    Complete = 2,
}
