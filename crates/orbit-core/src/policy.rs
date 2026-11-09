//! Removal policy (what may be moved to the Trash or deleted) and cloud-storage exclusion.
//!
//! Ported from `TrashPolicy.swift` and `CloudStoragePolicy.swift`. The OS-specific inputs come
//! from the platform layer ([`ProtectedPaths`], cloud roots, mounted volumes); the filesystem
//! facts the checks need come through [`PolicyFs`] so tests can inject them. Rejection reasons
//! are user-facing and match the Swift app word for word.
//!
//! Path comparisons are byte-exact and component-wise on canonical (symlink-resolved) paths:
//! `/a/bc` is not inside `/a/b`, and NFC/NFD spellings are different paths. Case-variant spellings
//! on case-insensitive volumes are caught by directory identity, not by path text.
//!
//! A [`CloudValidator`] / [`RemovalPolicy`] caches ancestor classifications and is meant for one
//! synchronous validation batch; the removal itself must re-check with a fresh one.

use crate::paths::{SEP, is_within};
use orbit_platform::{FileIdentity, ProtectedPaths};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const CLOUD_EXCLUDED: &str = "Cloud storage is excluded from mapping and collection.";
pub const CLOUD_DATALESS: &str = "This cloud-only location is excluded; Orbit Disk will not download it.";
pub const NOT_LOCAL: &str = "Only local files can be removed.";
pub const NEEDS_DOWNLOAD: &str = "This item cannot be checked without downloading cloud content.";
pub const MAPPED_FOLDER: &str = "The mapped folder cannot be removed.";
pub const OUTSIDE_ROOT: &str = "This item is outside the scanned folder.";
pub const PROTECTED_LOCATION: &str = "This location is protected.";
pub const PROTECTED_SYSTEM: &str = "System and Library folders are protected.";
pub const PROTECTED_SYSTEM_LOCATION: &str = "This system location is protected.";
pub const IN_TRASH: &str = "Items already in Trash are protected.";
pub const VOLUME: &str = "A disk or volume cannot be removed.";
pub const SYMLINK: &str = "Symbolic links cannot be removed here.";
pub const HIDDEN_FOLDER: &str = "Hidden configuration folders are protected.";
/// Followed by the error description.
pub const NO_LONGER_ACCESSIBLE: &str = "This item is no longer accessible: ";

/// Lexically normalizes an absolute path: drops empty and `.` components, applies `..` (never
/// above `/`), removes a trailing slash. `None` for relative or empty paths. Does not touch the
/// filesystem and never changes component bytes.
pub fn standardize(path: &[u8]) -> Option<Vec<u8>> {
    if path.first() != Some(&SEP) {
        return None;
    }
    let mut components: Vec<&[u8]> = Vec::new();
    for component in path.split(|b| *b == SEP) {
        match component {
            b"" | b"." => {}
            b".." => {
                components.pop();
            }
            name => components.push(name),
        }
    }
    let mut out = Vec::with_capacity(path.len());
    for component in &components {
        out.push(SEP);
        out.extend_from_slice(component);
    }
    if out.is_empty() {
        out.push(SEP);
    }
    Some(out)
}

/// `path` lies inside (or is) one of `roots`.
pub fn is_within_any<R: AsRef<[u8]>>(path: &[u8], roots: &[R]) -> bool {
    roots.iter().any(|root| is_within(path, root.as_ref()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsKind {
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsError {
    /// The item is a cloud placeholder; reading it would download content.
    Dataless,
    /// Any other failure, with a user-readable description.
    Other(String),
}

/// Filesystem facts the policies need.
pub trait PolicyFs {
    /// `path` with every symlink resolved. Components that do not exist are kept as given (the
    /// existing prefix is still resolved).
    fn canonicalize(&self, path: &[u8]) -> Vec<u8>;
    /// The kind of `path` itself (a final symlink is not followed).
    fn lstat_kind(&self, path: &[u8]) -> Result<FsKind, FsError>;
    /// Identity of the directory at `path`, following symlinks; `None` if it is not a readable
    /// directory.
    fn directory_identity(&self, path: &[u8]) -> Option<FileIdentity>;
    /// Whether `path` is synced cloud content (`None`: not known). Must not download anything;
    /// fails with [`FsError::Dataless`] for placeholders.
    fn is_cloud_item(&self, path: &[u8]) -> Result<Option<bool>, FsError>;
    /// Runs `body` with on-demand cloud downloads disabled for the current thread.
    fn without_materialization<R>(&self, body: impl FnOnce() -> R) -> Result<R, FsError> {
        Ok(body())
    }
}

/// Cloud exclusion roots for mapping and removal: the platform's `known` roots, plus each
/// `optional` folder (e.g. a synced Desktop or Documents) whose own metadata says it is cloud
/// content or a placeholder. Unreadable metadata adds nothing. Both the standardized and the
/// symlink-resolved spelling of every root are returned, without duplicates.
pub fn cloud_exclusions<F: PolicyFs, R: AsRef<[u8]>>(known: &[R], optional: &[R], fs: &F) -> Vec<Vec<u8>> {
    let standard: Vec<Vec<u8>> = known.iter().filter_map(|r| standardize(r.as_ref())).collect();
    let expanded = fs.without_materialization(|| {
        let mut roots = standard.clone();
        for candidate in optional.iter().filter_map(|r| standardize(r.as_ref())) {
            match fs.is_cloud_item(&candidate) {
                Ok(Some(true)) | Err(FsError::Dataless) => roots.push(candidate),
                _ => {}
            }
        }
        let mut seen = HashSet::new();
        roots
            .into_iter()
            .flat_map(|root| {
                let resolved = fs.canonicalize(&root);
                [root, resolved]
            })
            .filter(|root| seen.insert(root.clone()))
            .collect()
    });
    expanded.unwrap_or(standard)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Local,
    Excluded(&'static str),
}

/// Decides whether an item is (inside) cloud storage. Discard after each validation batch.
pub struct CloudValidator<'f, F> {
    fs: &'f F,
    roots: Vec<Vec<u8>>,
    protect_containing: bool,
    root_identities: HashSet<FileIdentity>,
    containing_identities: HashSet<FileIdentity>,
    ancestors: HashMap<Vec<u8>, Decision>,
}

impl<F> std::fmt::Debug for CloudValidator<'_, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudValidator").field("roots", &self.roots.len()).finish_non_exhaustive()
    }
}

impl<'f, F: PolicyFs> CloudValidator<'f, F> {
    /// `roots`: exclusion roots (see [`cloud_exclusions`]). `protect_containing`: folders that
    /// contain a cloud root are excluded too (removal); mapping passes `false`.
    pub fn new<R: AsRef<[u8]>>(fs: &'f F, roots: &[R], protect_containing: bool) -> Self {
        let roots: Vec<Vec<u8>> = roots.iter().filter_map(|r| standardize(r.as_ref())).collect();
        let mut root_identities = HashSet::new();
        let mut containing_identities = HashSet::new();
        let _ = fs.without_materialization(|| {
            root_identities = roots.iter().filter_map(|r| fs.directory_identity(r)).collect();
            if protect_containing {
                let mut visited: HashSet<&[u8]> = HashSet::new();
                for root in &roots {
                    let mut ancestor = Some(root.as_slice());
                    while let Some(path) = ancestor
                        && visited.insert(path)
                    {
                        containing_identities.extend(fs.directory_identity(path));
                        ancestor = crate::paths::parent(path);
                    }
                }
            }
        });
        Self { fs, roots, protect_containing, root_identities, containing_identities, ancestors: HashMap::new() }
    }

    /// The exclusion message for `path`, or `None` if it is local.
    pub fn exclusion_reason(&mut self, path: &[u8]) -> Option<&'static str> {
        let original = standardize(path)?;
        if self.overlaps(&original) {
            return Some(CLOUD_EXCLUDED);
        }
        let fs = self.fs;
        match fs.without_materialization(|| self.classify_tree(&original)) {
            Ok(reason) => reason,
            Err(FsError::Dataless) => Some(CLOUD_DATALESS),
            Err(FsError::Other(_)) => None,
        }
    }

    fn classify_tree(&mut self, original: &[u8]) -> Option<&'static str> {
        let target = self.fs.canonicalize(original);
        if self.overlaps(&target) {
            return Some(CLOUD_EXCLUDED);
        }
        // Every requested item is classified afresh, even if it was an ancestor earlier on.
        if let Decision::Excluded(reason) = self.classify(&target, self.protect_containing) {
            return Some(reason);
        }
        let mut ancestor = crate::paths::parent(&target)?.to_vec();
        let mut visited = Vec::new();
        let result = loop {
            if let Some(cached) = self.ancestors.get(&ancestor) {
                break *cached;
            }
            let decision = self.classify(&ancestor, false);
            visited.push(ancestor.clone());
            match crate::paths::parent(&ancestor) {
                Some(parent) if decision == Decision::Local => ancestor = parent.to_vec(),
                _ => break decision,
            }
        };
        for key in visited {
            self.ancestors.insert(key, result);
        }
        match result {
            Decision::Local => None,
            Decision::Excluded(reason) => Some(reason),
        }
    }

    fn overlaps(&self, path: &[u8]) -> bool {
        self.roots.iter().any(|root| is_within(path, root) || (self.protect_containing && is_within(root, path)))
    }

    fn classify(&self, path: &[u8], include_containing: bool) -> Decision {
        if let Some(identity) = self.fs.directory_identity(path)
            && (self.root_identities.contains(&identity)
                || (include_containing && self.containing_identities.contains(&identity)))
        {
            return Decision::Excluded(CLOUD_EXCLUDED);
        }
        match self.fs.is_cloud_item(path) {
            Ok(Some(true)) => Decision::Excluded(CLOUD_EXCLUDED),
            Err(FsError::Dataless) => Decision::Excluded(CLOUD_DATALESS),
            _ => Decision::Local,
        }
    }
}

/// Validates removals (Trash or delete) for one batch.
pub struct RemovalPolicy<'f, F> {
    fs: &'f F,
    cloud: CloudValidator<'f, F>,
    scan_root: Vec<u8>,
    home: Vec<u8>,
    system_roots: Vec<Vec<u8>>,
    temporary_roots: Vec<Vec<u8>>,
    restricted_root: Option<Vec<u8>>,
    trash_components: Vec<&'static [u8]>,
    volumes_root: Option<Vec<u8>>,
    volumes: Vec<Vec<u8>>,
}

impl<F> std::fmt::Debug for RemovalPolicy<'_, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemovalPolicy")
            .field("scan_root", &String::from_utf8_lossy(&self.scan_root))
            .finish_non_exhaustive()
    }
}

/// Path bytes of a native path (WTF-8 on Windows).
fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_encoded_bytes()
}

impl<'f, F: PolicyFs> RemovalPolicy<'f, F> {
    /// `scan_root`: the mapped folder. `cloud_roots`: exclusion roots (see [`cloud_exclusions`]).
    /// `volumes`: mount points of mounted volumes.
    pub fn new<R: AsRef<[u8]>>(
        fs: &'f F,
        scan_root: &[u8],
        home: &[u8],
        protected: &ProtectedPaths,
        cloud_roots: &[R],
        volumes: &[R],
    ) -> Self {
        let resolve = |path: &[u8]| standardize(path).map(|p| fs.canonicalize(&p)).unwrap_or_else(|| path.to_vec());
        let resolve_all = |paths: &[std::path::PathBuf]| paths.iter().map(|p| resolve(path_bytes(p))).collect();
        let (scan_root, home, system_roots, temporary_roots, volumes) = fs
            .without_materialization(|| {
                (
                    resolve(scan_root),
                    resolve(home),
                    resolve_all(&protected.system_roots),
                    resolve_all(&protected.temporary_roots),
                    volumes.iter().map(|v| resolve(v.as_ref())).collect(),
                )
            })
            .unwrap_or_else(|_| (scan_root.to_vec(), home.to_vec(), Vec::new(), Vec::new(), Vec::new()));
        Self {
            fs,
            cloud: CloudValidator::new(fs, cloud_roots, true),
            scan_root,
            home,
            system_roots,
            temporary_roots,
            restricted_root: protected.restricted_root.as_deref().and_then(|p| standardize(path_bytes(p))),
            trash_components: protected.trash_components.iter().map(|c| c.as_bytes()).collect(),
            volumes_root: protected.volumes_root.as_deref().and_then(|p| standardize(path_bytes(p))),
            volumes,
        }
    }

    /// Why `path` must not be removed, or `None` if it may be.
    pub fn rejection_reason(&mut self, path: &[u8]) -> Option<String> {
        let Some(original) = standardize(path) else { return Some(NOT_LOCAL.into()) };
        let fs = self.fs;
        match fs.without_materialization(|| self.local_rejection_reason(&original)) {
            Ok(reason) => reason,
            Err(_) => Some(NEEDS_DOWNLOAD.into()),
        }
    }

    fn local_rejection_reason(&mut self, original: &[u8]) -> Option<String> {
        if let Some(reason) = self.cloud.exclusion_reason(original) {
            return Some(reason.into());
        }
        let target = self.fs.canonicalize(original);
        let target = target.as_slice();
        if target == self.scan_root.as_slice() {
            return Some(MAPPED_FOLDER.into());
        }
        if !is_within(target, &self.scan_root) {
            return Some(OUTSIDE_ROOT.into());
        }
        if target == b"/" || is_within(&self.home, target) {
            return Some(PROTECTED_LOCATION.into());
        }
        if is_within_any(target, &self.system_roots) {
            return Some(PROTECTED_SYSTEM.into());
        }
        // Only the temporary locations inside the restricted root (macOS `/private`) are open.
        if let Some(restricted) = &self.restricted_root
            && is_within(target, restricted)
            && !self.temporary_roots.iter().any(|t| is_within(target, t) && target != t.as_slice())
        {
            return Some(PROTECTED_SYSTEM_LOCATION.into());
        }
        if target.split(|b| *b == SEP).any(|c| self.trash_components.contains(&c)) {
            return Some(IN_TRASH.into());
        }
        if self.volumes.iter().any(|v| is_within(v, target))
            || self
                .volumes_root
                .as_deref()
                .is_some_and(|root| target == root || crate::paths::parent(target) == Some(root))
        {
            return Some(VOLUME.into());
        }
        let inaccessible = |error: FsError| {
            Some(match error {
                FsError::Dataless => format!("{NO_LONGER_ACCESSIBLE}cloud content is not available offline"),
                FsError::Other(message) => format!("{NO_LONGER_ACCESSIBLE}{message}"),
            })
        };
        match self.fs.lstat_kind(original) {
            Ok(FsKind::Symlink) => return Some(SYMLINK.into()),
            Ok(_) => {}
            Err(error) => return inaccessible(error),
        }
        let mut ancestor = Some(target);
        while let Some(path) = ancestor.filter(|p| *p != b"/") {
            if crate::paths::file_name(path).first() == Some(&b'.') {
                match self.fs.lstat_kind(path) {
                    Ok(FsKind::Directory) => return Some(HIDDEN_FOLDER.into()),
                    Ok(_) => {}
                    Err(error) => return inaccessible(error),
                }
            }
            ancestor = crate::paths::parent(path);
        }
        None
    }
}

/// Path facts from the standard library (Unix).
#[cfg(unix)]
pub mod std_fs {
    use super::{FsError, FsKind};
    use orbit_platform::FileIdentity;
    use orbit_platform::posix::path_from_bytes;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    /// Resolves symlinks in the longest existing prefix of `path` and appends the rest.
    pub fn canonicalize(path: &[u8]) -> Vec<u8> {
        if let Ok(resolved) = std::fs::canonicalize(path_from_bytes(path)) {
            return resolved.as_os_str().as_bytes().to_vec();
        }
        match crate::paths::parent(path) {
            Some(parent) if parent != path => crate::paths::join(&canonicalize(parent), crate::paths::file_name(path)),
            _ => path.to_vec(),
        }
    }

    pub fn lstat_kind(path: &[u8]) -> Result<FsKind, FsError> {
        let metadata = std::fs::symlink_metadata(path_from_bytes(path)).map_err(|e| FsError::Other(e.to_string()))?;
        let kind = metadata.file_type();
        Ok(if kind.is_symlink() {
            FsKind::Symlink
        } else if kind.is_dir() {
            FsKind::Directory
        } else {
            FsKind::Other
        })
    }

    pub fn directory_identity(path: &[u8]) -> Option<FileIdentity> {
        let metadata = std::fs::metadata(path_from_bytes(path)).ok()?;
        metadata.is_dir().then(|| FileIdentity { dev: metadata.dev(), ino: metadata.ino() })
    }
}

/// [`PolicyFs`] backed by the standard library for path facts and the platform layer for cloud
/// status and materialization control.
#[cfg(unix)]
#[derive(Debug)]
pub struct PlatformPolicyFs<'p, P>(pub &'p P);

#[cfg(unix)]
impl<P: orbit_platform::Platform> PolicyFs for PlatformPolicyFs<'_, P> {
    fn canonicalize(&self, path: &[u8]) -> Vec<u8> {
        std_fs::canonicalize(path)
    }

    fn lstat_kind(&self, path: &[u8]) -> Result<FsKind, FsError> {
        std_fs::lstat_kind(path)
    }

    fn directory_identity(&self, path: &[u8]) -> Option<FileIdentity> {
        std_fs::directory_identity(path)
    }

    fn is_cloud_item(&self, path: &[u8]) -> Result<Option<bool>, FsError> {
        use orbit_platform::{CloudStatus, PlatformError};
        match self.0.cloud_status(orbit_platform::posix::path_from_bytes(path)) {
            Ok(CloudStatus::Local) => Ok(Some(false)),
            Ok(CloudStatus::Cloud) => Ok(Some(true)),
            Ok(CloudStatus::Dataless) | Err(PlatformError::Dataless { .. }) => Err(FsError::Dataless),
            Err(PlatformError::Unsupported(_)) => Ok(None),
            Err(error) => Err(FsError::Other(error.to_string())),
        }
    }

    fn without_materialization<R>(&self, body: impl FnOnce() -> R) -> Result<R, FsError> {
        self.0.without_materialization(body).map_err(|e| match e {
            orbit_platform::PlatformError::Dataless { .. } => FsError::Dataless,
            other => FsError::Other(other.to_string()),
        })
    }
}

#[cfg(all(test, unix))]
mod tests;
