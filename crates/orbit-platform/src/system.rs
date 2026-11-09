use crate::FileIdentity;
use std::path::PathBuf;

/// Volume aliases the traversal scope must understand. On macOS the boot volume group exposes
/// the same Data-volume folders twice (e.g. `/Users` and `/System/Volumes/Data/Users`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeLayout {
    /// Mount name of the mapped root's filesystem (see `Platform::mount_name`).
    pub root_mount: Option<String>,
    /// Present only when the mapped root is the boot volume's `/`.
    pub boot: Option<BootLayout>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootLayout {
    /// Path of the writable data volume mount (`/System/Volumes/Data`).
    pub data_path: Vec<u8>,
    pub data_mount: String,
    pub data_identity: FileIdentity,
    /// Logical paths (e.g. `/Users`) that are firmlinks into the data volume, with the identity
    /// they must resolve to.
    pub logical_aliases: Vec<(Vec<u8>, FileIdentity)>,
    /// Physical paths under `data_path` that duplicate a logical alias and must be skipped.
    pub physical_aliases: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VolumeInfo {
    pub path: PathBuf,
    pub name: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub is_boot: bool,
    pub is_removable: bool,
    pub is_local: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudStatus {
    Local,
    /// Synced by a cloud provider (may or may not be downloaded).
    Cloud,
    /// A placeholder whose content is not on disk; reading it would download.
    Dataless,
}

/// OS-specific removal guards. The portable policy in `orbit-core` applies them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProtectedPaths {
    /// Never removable, nor anything inside (e.g. `/System`, `/usr`, `~/Library`).
    pub system_roots: Vec<PathBuf>,
    /// Removable only *inside* these, even though their parent is protected (e.g. `/private/tmp`).
    pub temporary_roots: Vec<PathBuf>,
    /// A root whose descendants are protected except under `temporary_roots` (e.g. `/private`).
    pub restricted_root: Option<PathBuf>,
    /// Path components that mark trash folders (`.Trash`, `.Trashes`, `$Recycle.Bin`).
    pub trash_components: Vec<&'static str>,
    /// Where removable volumes mount (`/Volumes`); the folder and its children are protected.
    pub volumes_root: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDirs {
    /// Rebuildable data: saved maps and search caches.
    pub cache: PathBuf,
    /// Durable app data: mapping stages, removal journal, locks.
    pub support: PathBuf,
    /// User preferences (Places, last location).
    pub settings: PathBuf,
    /// Per-user private runtime directory for the automation socket (mode 0700).
    pub runtime: PathBuf,
}
