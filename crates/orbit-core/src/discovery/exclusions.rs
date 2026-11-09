//! Directories discovery must never enter: the app's own cache and stage directories (Swift
//! `IndexLocations.exclusionPaths`) and synced cloud storage (Swift
//! `CloudStoragePolicy.scanExclusions`).
//!
//! Each exclusion matches by path *and* by directory identity, so an excluded folder reached
//! through another spelling (a symlinked ancestor, a firmlink alias) is still skipped. On the
//! macOS boot volume, paths are also recorded in their alternate Data-volume spelling when both
//! spellings name the same directory.

use crate::paths::{SEP, is_within};
use orbit_platform::{FileIdentity, Platform, VolumeLayout};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exclusions {
    paths: HashSet<Vec<u8>>,
    identities: HashSet<FileIdentity>,
}

impl Exclusions {
    pub fn none() -> Self {
        Self::default()
    }

    /// Resolves `roots` (cache, stage, lock and cloud directories). Missing paths still
    /// exclude by path, so a cache created later is skipped too.
    pub fn resolve<P: Platform>(platform: &P, roots: &[PathBuf], layout: &VolumeLayout) -> Self {
        let mut exclusions = Self::default();
        for root in roots {
            let standardized = standardize(&path_bytes(root));
            let mut spellings = vec![standardized.clone()];
            if let Ok(resolved) = std::fs::canonicalize(root) {
                spellings.push(standardize(&path_bytes(&resolved)));
            }
            for spelling in spellings {
                if let Some(boot) = &layout.boot {
                    let data = &boot.data_path[..];
                    if is_within(&spelling, data) && spelling.len() > data.len() {
                        let native = spelling[data.len()..].to_vec();
                        if same_directory(platform, &spelling, &native) {
                            exclusions.paths.insert(native);
                        }
                    } else if spelling.first() == Some(&SEP) && spelling != b"/" {
                        let mut physical = data.to_vec();
                        physical.extend_from_slice(&spelling);
                        if same_directory(platform, &physical, &spelling) {
                            exclusions.paths.insert(physical);
                        }
                    }
                }
                if let Ok((_, identity)) = platform.open_root(&crate::discovery::native_path(&spelling)) {
                    exclusions.identities.insert(identity);
                }
                exclusions.paths.insert(spelling);
            }
        }
        exclusions
    }

    /// Adds a path and identity directly (tests and callers that resolved them already).
    pub fn insert(&mut self, path: Vec<u8>, identity: Option<FileIdentity>) {
        self.paths.insert(path);
        if let Some(identity) = identity {
            self.identities.insert(identity);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.identities.is_empty()
    }

    #[inline]
    pub fn excludes(&self, path: &[u8], identity: FileIdentity) -> bool {
        (!self.identities.is_empty() && self.identities.contains(&identity))
            || (!self.paths.is_empty() && self.paths.contains(path))
    }

    #[inline]
    pub fn has_paths(&self) -> bool {
        !self.paths.is_empty()
    }

    pub fn paths(&self) -> impl Iterator<Item = &[u8]> {
        self.paths.iter().map(Vec::as_slice)
    }

    pub fn identities(&self) -> impl Iterator<Item = FileIdentity> + '_ {
        self.identities.iter().copied()
    }
}

/// Cloud storage roots below `home` plus the app's own directories that every mapping skips.
pub fn scan_exclusions<P: Platform>(platform: &P, home: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = platform.cloud_roots(home);
    roots.extend_from_slice(extra);
    roots
}

pub(crate) fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

/// [`crate::paths::standardize`] for exclusion roots, which are absolute; a relative path is
/// treated as if it started at `/`.
fn standardize(path: &[u8]) -> Vec<u8> {
    let mut absolute = Vec::with_capacity(path.len() + 1);
    absolute.push(SEP);
    absolute.extend_from_slice(path);
    crate::paths::standardize(&absolute).expect("absolute")
}

/// Two spellings reach the same directory, checked through the nearest existing ancestor pair
/// (the excluded folder itself may not exist yet).
fn same_directory<P: Platform>(platform: &P, a: &[u8], b: &[u8]) -> bool {
    let (mut a, mut b) = (a, b);
    while b != b"/" && a != b"/" {
        let ia = platform.open_root(&crate::discovery::native_path(a)).map(|(_, id)| id);
        let ib = platform.open_root(&crate::discovery::native_path(b)).map(|(_, id)| id);
        if let (Ok(ia), Ok(ib)) = (&ia, &ib)
            && ia == ib
        {
            return true;
        }
        match (crate::paths::parent(a), crate::paths::parent(b)) {
            (Some(pa), Some(pb)) => {
                a = pa;
                b = pb;
            }
            _ => return false,
        }
    }
    false
}
