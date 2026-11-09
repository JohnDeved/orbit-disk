//! Canonical spellings of directories, used to match journal items against a recovery root that
//! reaches the same folder through a different path (symlinks, volume aliases).
//!
//! Ported from `RemovalJournal.canonicalDirectory` and `IndexLocations.exclusionPaths`. The
//! algorithm must stay byte-for-byte compatible with the Swift app so `canonical` columns written
//! by Swift journals keep matching after an update.

use crate::paths;
use orbit_platform::FileIdentity;
use orbit_platform::posix as sys;
use orbit_platform::posix::path_from_bytes;
use std::os::unix::ffi::OsStrExt;

/// OS volume aliases that make one folder reachable under several absolute paths, from
/// [`orbit_platform::Platform::path_aliases`] (macOS: the boot Data volume and `/private`, matching
/// Foundation's `resolvingSymlinksInPath`); empty elsewhere.
pub type CanonicalAliases = orbit_platform::PathAliases;

trait Spellings {
    fn spellings(&self, resolved: &[u8]) -> Vec<Vec<u8>>;
}

impl Spellings for CanonicalAliases {
    /// Every known spelling of an already resolved path (Swift `exclusionPaths([resolved])`).
    fn spellings(&self, resolved: &[u8]) -> Vec<Vec<u8>> {
        let mut bases = vec![resolved.to_vec()];
        if let Some(hidden) = &self.hidden_prefix
            && paths::is_within(resolved, hidden)
            && resolved.len() > hidden.len()
        {
            let stripped = &resolved[hidden.len()..];
            if sys::lstat(path_from_bytes(stripped)).is_ok() {
                bases.push(stripped.to_vec());
            }
        }
        let mut out = bases.clone();
        if let Some(data) = &self.data_volume {
            for path in &bases {
                if paths::is_within(path, data) && path.len() > data.len() {
                    let native = &path[data.len()..];
                    if matches_through_ancestor(path, native) {
                        out.push(native.to_vec());
                    }
                } else if path.first() == Some(&b'/') && path.as_slice() != b"/" {
                    let mut physical = data.clone();
                    physical.extend_from_slice(path);
                    if matches_through_ancestor(&physical, path) {
                        out.push(physical);
                    }
                }
            }
        }
        out
    }
}

fn same_file(a: &[u8], b: &[u8]) -> bool {
    match (sys::stat(path_from_bytes(a)), sys::stat(path_from_bytes(b))) {
        (Ok(x), Ok(y)) => x.identity == y.identity,
        _ => false,
    }
}

fn matches_through_ancestor(first: &[u8], second: &[u8]) -> bool {
    let (mut a, mut b) = (first, second);
    while b != b"/" {
        if same_file(a, b) {
            return true;
        }
        a = paths::parent(a).unwrap_or(b"/");
        b = paths::parent(b).unwrap_or(b"/");
    }
    false
}

/// Identity of a path, following symlinks, if it is a directory.
pub(crate) fn directory_identity(path: &[u8]) -> Option<FileIdentity> {
    sys::stat(path_from_bytes(path)).ok().filter(sys::Stat::is_dir).map(|m| m.identity)
}

/// Identity of a path if it is a directory itself (a final symlink is not followed).
pub(crate) fn directory_identity_no_follow(path: &[u8]) -> Option<FileIdentity> {
    sys::lstat(path_from_bytes(path)).ok().filter(sys::Stat::is_dir).map(|m| m.identity)
}

/// `realpath` of a directory reduced to its lexicographically smallest alias, or `None` when the
/// path is not a directory or changed while it was being resolved.
pub(crate) fn canonical_directory(path: &[u8], aliases: &CanonicalAliases) -> Option<Vec<u8>> {
    let before = directory_identity(path)?;
    let resolved = std::fs::canonicalize(path_from_bytes(path)).ok()?;
    let canonical = aliases.spellings(resolved.as_os_str().as_bytes()).into_iter().min()?;
    (sys::stat(path_from_bytes(path)).ok()?.identity == before).then_some(canonical)
}

/// The identity and canonical spelling of a parent directory at intent time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParentIdentity {
    pub canonical: Option<Vec<u8>>,
    pub identity: Option<FileIdentity>,
}

pub(crate) fn parent_identity(path: &[u8], aliases: &CanonicalAliases) -> ParentIdentity {
    let resolve = || {
        let before = directory_identity(path)?;
        let canonical = canonical_directory(path, aliases)?;
        (directory_identity(path)? == before)
            .then_some(ParentIdentity { canonical: Some(canonical), identity: Some(before) })
    };
    resolve().unwrap_or_default()
}

/// The binding recovery holds a root to: identity plus canonical spelling. Recovery fails with
/// `JournalChanged` when this changes between journal pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootBinding {
    pub identity: FileIdentity,
    pub canonical: Vec<u8>,
}

pub(crate) fn root_binding(root: &[u8], aliases: &CanonicalAliases) -> Option<RootBinding> {
    let before = directory_identity(root)?;
    let canonical = canonical_directory(root, aliases)?;
    (directory_identity(root)? == before).then_some(RootBinding { identity: before, canonical })
}

/// An absolute, standardized byte path: leading `/`, no empty, `.` or `..` components, no trailing
/// slash (except `/` itself), no NUL. Matches Swift's `standardizedFileURL.path == path` check.
pub fn is_standard_path(path: &[u8]) -> bool {
    if path.first() != Some(&b'/') || path.contains(&0) {
        return false;
    }
    if path == b"/" {
        return true;
    }
    path[1..].split(|b| *b == b'/').all(|c| !c.is_empty() && c != b"." && c != b"..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_paths_reject_lexical_tricks() {
        assert!(is_standard_path(b"/"));
        assert!(is_standard_path(b"/a/b\xff"));
        for bad in [&b""[..], b"a/b", b"/a/", b"/a//b", b"/a/./b", b"/a/../b", b"/a\0b"] {
            assert!(!is_standard_path(bad), "{:?}", String::from_utf8_lossy(bad));
        }
    }
}
