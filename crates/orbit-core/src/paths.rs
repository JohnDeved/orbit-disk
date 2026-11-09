//! Byte-path helpers. Core paths are absolute, `/`-separated byte strings without a trailing
//! slash (except the root `/`). Platform layers convert native paths at the boundary.

pub const SEP: u8 = b'/';

/// `path` equals `root` or lies inside it, comparing whole components.
#[inline]
pub fn is_within(path: &[u8], root: &[u8]) -> bool {
    if root == b"/" {
        return path.first() == Some(&SEP);
    }
    path.starts_with(root) && (path.len() == root.len() || path[root.len()] == SEP)
}

/// The parent directory, or `None` for `/`.
pub fn parent(path: &[u8]) -> Option<&[u8]> {
    if path == b"/" {
        return None;
    }
    let end = memchr::memrchr(SEP, path)?;
    Some(if end == 0 { b"/" } else { &path[..end] })
}

pub fn file_name(path: &[u8]) -> &[u8] {
    memchr::memrchr(SEP, path).map_or(path, |i| &path[i + 1..])
}

pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(dir.len() + 1 + name.len());
    out.extend_from_slice(dir);
    if dir.last() != Some(&SEP) {
        out.push(SEP);
    }
    out.extend_from_slice(name);
    out
}

/// Components below `root` (empty when `path == root`). `None` if `path` is outside `root`.
pub fn relative_components<'a>(path: &'a [u8], root: &[u8]) -> Option<impl Iterator<Item = &'a [u8]>> {
    if !is_within(path, root) {
        return None;
    }
    let rest = if root == b"/" { &path[1..] } else { &path[root.len()..] };
    Some(rest.split(|b| *b == SEP).filter(|c| !c.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_is_component_wise() {
        assert!(is_within(b"/a/b", b"/a"));
        assert!(is_within(b"/a", b"/a"));
        assert!(!is_within(b"/ab", b"/a"));
        assert!(is_within(b"/x", b"/"));
        assert_eq!(parent(b"/a"), Some(&b"/"[..]));
        assert_eq!(parent(b"/"), None);
        assert_eq!(relative_components(b"/a/b/c", b"/a").unwrap().collect::<Vec<_>>(), [&b"b"[..], b"c"]);
    }
}
