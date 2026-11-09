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

/// Spellings under which the OS exposes one folder at several absolute paths, independent of
/// any mapped root. The removal journal canonicalizes item paths with them so records written by
/// earlier builds keep matching.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathAliases {
    /// Mount path of the writable data volume whose folders also appear at `/` (macOS:
    /// `/System/Volumes/Data`).
    pub data_volume: Option<Vec<u8>>,
    /// A prefix the OS hides from resolved paths when the shorter path exists (macOS: `/private`).
    pub hidden_prefix: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDirs {
    /// Rebuildable data: saved maps and search caches.
    pub cache: PathBuf,
    /// Durable app data: mapping stages and locks.
    pub support: PathBuf,
    /// Parent of the cache-wide removal journal (`.orbit-removals`). On macOS this is the Swift
    /// app's `Mapping` directory so journals written before the update are recovered, never lost.
    pub removal_journal: PathBuf,
    /// Cache directories of earlier builds whose contents can never be read again (old formats).
    /// Deleted once on startup. Never contains anything durable such as the removal journal.
    pub obsolete_caches: Vec<PathBuf>,
    /// User preferences (Places, last location).
    pub settings: PathBuf,
    /// Per-user private runtime directory for the automation socket (mode 0700).
    pub runtime: PathBuf,
}
