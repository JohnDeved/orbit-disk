//! Byte-path helpers on top of `orbit_core::paths` for collection and hover logic.

use orbit_core::paths::{SEP, file_name, is_within};

use crate::hash::FxHashSet;
use std::sync::Arc;

/// Trims trailing separators (keeping `/`) and collapses repeated separators, so `/a//b/` and
/// `/a/b` name the same item. Other bytes, including `.` components and non-UTF-8 sequences,
/// are kept: canonically equivalent Unicode spellings stay distinct, as in the filesystem.
pub fn normalized(path: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let clean = !path.windows(2).any(|w| w == [SEP, SEP]) && (path.len() <= 1 || path.last() != Some(&SEP));
    if clean {
        return std::borrow::Cow::Borrowed(path);
    }
    let mut out = Vec::with_capacity(path.len());
    for &byte in path {
        if byte == SEP && out.last() == Some(&SEP) {
            continue;
        }
        out.push(byte);
    }
    while out.len() > 1 && out.last() == Some(&SEP) {
        out.pop();
    }
    std::borrow::Cow::Owned(out)
}

/// Parent of a normalized path: `/a/b` → `/a`, `/a` → `/`, `/` → `None`, `a` (relative) → `None`.
pub fn parent(path: &[u8]) -> Option<&[u8]> {
    if path == b"/" {
        return None;
    }
    let end = path.iter().rposition(|b| *b == SEP)?;
    Some(if end == 0 { b"/" } else { &path[..end] })
}

/// Parent, or `/` for the root and relative single components.
pub fn parent_or_root(path: &[u8]) -> &[u8] {
    parent(path).unwrap_or(b"/")
}

/// Whether `path` (or, with `including_self == false`, one of its proper ancestors) is in `set`.
/// Port of Swift `hasCollectedAncestor`.
pub fn has_collected_ancestor(path: &[u8], set: &FxHashSet<Arc<[u8]>>, including_self: bool) -> bool {
    if set.is_empty() {
        return false;
    }
    let path = normalized(path);
    let mut candidate: Option<&[u8]> = if including_self { Some(&path) } else { parent(&path) };
    while let Some(current) = candidate {
        if set.contains(current) {
            return true;
        }
        candidate = parent(current);
    }
    false
}

/// `~`-abbreviated display form of `path` (Swift `abbreviatingWithTildeInPath`).
pub fn abbreviate(path: &[u8], home: &[u8]) -> String {
    if !home.is_empty() && home != b"/" && is_within(path, home) {
        let rest = &path[home.len()..];
        return format!("~{}", String::from_utf8_lossy(rest));
    }
    String::from_utf8_lossy(path).into_owned()
}

/// How the map names a hovered path relative to the current folder (Swift `relativeHoverPath`).
pub fn relative_hover_path(path: &[u8], current: Option<&[u8]>, home: &[u8]) -> String {
    if let Some(current) = current {
        if path == current {
            let name = file_name(current);
            return String::from_utf8_lossy(if name.is_empty() { current } else { name }).into_owned();
        }
        if current == b"/" {
            if path.len() > 1 && path[0] == SEP {
                return String::from_utf8_lossy(&path[1..]).into_owned();
            }
        } else if path.len() > current.len() + 1 && path.starts_with(current) && path[current.len()] == SEP {
            return String::from_utf8_lossy(&path[current.len() + 1..]).into_owned();
        }
    }
    abbreviate(path, home)
}

/// The child of `folder` on the way to `path` (`/f/a/b/c` below `/f` → `/f/a`).
pub fn child_toward<'a>(path: &'a [u8], folder: &[u8]) -> Option<&'a [u8]> {
    if !is_within(path, folder) || path.len() == folder.len() {
        return None;
    }
    let start = if folder == b"/" { 1 } else { folder.len() + 1 };
    let end = path[start..].iter().position(|b| *b == SEP).map_or(path.len(), |i| start + i);
    Some(&path[..end])
}

/// Whether any component of `path` equals `component`.
pub fn has_component(path: &[u8], component: &[u8]) -> bool {
    path.split(|b| *b == SEP).any(|c| c == component)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> FxHashSet<Arc<[u8]>> {
        paths.iter().map(|p| Arc::from(p.as_bytes())).collect()
    }

    #[test]
    fn collected_ancestor_respects_components_normalization_and_unicode_bytes() {
        let collected = set(&["/a/b", "/x/caf\u{e9}"]);
        assert!(has_collected_ancestor(b"/a/b", &collected, true));
        assert!(!has_collected_ancestor(b"/a/b", &collected, false));
        assert!(has_collected_ancestor(b"/a/b/c/d", &collected, false));
        assert!(has_collected_ancestor(b"/a//b/c/", &collected, true));
        assert!(!has_collected_ancestor(b"/a/bc", &collected, true));
        assert!(!has_collected_ancestor(b"/a", &collected, true));
        // NFD spelling of the same name is a different filesystem item here.
        assert!(!has_collected_ancestor("/x/cafe\u{301}/f".as_bytes(), &collected, true));
        assert!(has_collected_ancestor("/x/caf\u{e9}/f".as_bytes(), &collected, true));
        assert!(!has_collected_ancestor(b"a/b", &collected, true));
        assert!(has_collected_ancestor(b"/anything", &set(&["/"]), true));
    }

    #[test]
    fn hover_paths_are_relative_to_the_current_folder() {
        assert_eq!(relative_hover_path(b"/r/f", Some(b"/r/f"), b"/Users/me"), "f");
        assert_eq!(relative_hover_path(b"/r/f/a/b", Some(b"/r/f"), b"/Users/me"), "a/b");
        assert_eq!(relative_hover_path(b"/r/fa", Some(b"/r/f"), b"/Users/me"), "/r/fa");
        assert_eq!(relative_hover_path(b"/Users/me/x", Some(b"/r"), b"/Users/me"), "~/x");
        assert_eq!(relative_hover_path(b"/a", Some(b"/"), b"/Users/me"), "a");
        assert_eq!(child_toward(b"/f/a/b", b"/f"), Some(&b"/f/a"[..]));
        assert_eq!(child_toward(b"/a/b", b"/"), Some(&b"/a"[..]));
        assert_eq!(child_toward(b"/fa/b", b"/f"), None);
    }
}
