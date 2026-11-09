use std::path::PathBuf;

pub type Result<T, E = PlatformError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// An OS call failed; `code` is the raw OS error (errno / GetLastError).
    #[error("{}: {}", path.display(), std::io::Error::from_raw_os_error(*code))]
    Os { path: PathBuf, code: i32 },
    /// The directory at a path is no longer the one that was queued (replaced, renamed, or a
    /// symlink was swapped in). Callers treat this as "rescan the parent", never as deletion.
    #[error("{}: the folder changed while it was being read", path.display())]
    IdentityChanged { path: PathBuf },
    /// The item is a cloud placeholder and reading it would download content.
    #[error("{}: cloud content is not available offline", path.display())]
    Dataless { path: PathBuf },
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Unsupported(&'static str),
}

impl PlatformError {
    pub fn os(path: impl Into<PathBuf>, code: i32) -> Self {
        Self::Os { path: path.into(), code }
    }

    /// The last OS error for `path`.
    pub fn last_os(path: impl Into<PathBuf>) -> Self {
        Self::os(path, std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }

    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Os { code, .. } => Some(*code),
            _ => None,
        }
    }
}
