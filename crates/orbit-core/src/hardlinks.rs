//! Hard-link ownership order (Swift `HardLinkPathOrder`).
//!
//! When several paths share one file identity, exactly one of them, the *owner*, is charged
//! the allocated bytes. The owner is the path that comes first in the scanner's depth-first
//! order: paths are compared component by component, a path that is a component-prefix of
//! another comes first (`/a` before `/a/a`), and identical component lists fall back to the
//! raw bytes.
//!
//! # Difference from the Swift reference
//!
//! Swift decoded the bytes as UTF-8 and compared components with `String <`, which treats
//! canonically equivalent spellings (NFC `é` and NFD `e` + U+0301) as *equal* and orders by
//! Unicode scalar values otherwise; invalid UTF-8 became U+FFFD. This port compares raw bytes:
//!
//! - Byte order on valid UTF-8 equals scalar-value order, so ordinary names order identically.
//! - NFC/NFD spellings of one component are *different* components here, ordered by bytes
//!   (`e` + U+0301 = `65 CC 81` sorts before `é` = `C3 A9`). Swift reached the same final order
//!   for the reference cases through its byte tie-break, but ranks `/é/a` before
//!   `/e\u{301}/z` (equal first components, then `a` vs `z`); bytes rank `/e\u{301}/z` first.
//!   Linux allows both spellings as siblings, so treating them as distinct is required there.
//! - Names that are not UTF-8 keep their own bytes instead of collapsing to U+FFFD, so distinct
//!   invalid names never compare equal.
//!
//! The order is a strict total order on byte paths, which is all ownership needs: every reader
//! (live inventory, stage, saved map) must use this one function.

use crate::paths::SEP;
use std::cmp::Ordering;

#[inline]
fn components(path: &[u8]) -> impl Iterator<Item = &[u8]> + '_ {
    path.split(|b| *b == SEP).filter(|c| !c.is_empty())
}

/// Compares two paths in hard-link ownership order.
pub fn compare(left: &[u8], right: &[u8]) -> Ordering {
    let mut a = components(left);
    let mut b = components(right);
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) => match x.cmp(y) {
                Ordering::Equal => continue,
                other => return other,
            },
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => return left.cmp(right),
        }
    }
}

/// `left` owns a shared identity before `right`.
#[inline]
pub fn precedes(left: &[u8], right: &[u8]) -> bool {
    compare(left, right) == Ordering::Less
}

/// Compares two paths given as component lists (root first). Equivalent to [`compare`] on the
/// joined paths when no component is empty; used by the live inventory, which stores names but
/// not paths.
pub fn compare_components<'a>(
    left: impl IntoIterator<Item = &'a [u8]>,
    right: impl IntoIterator<Item = &'a [u8]>,
) -> Ordering {
    let mut a = left.into_iter();
    let mut b = right.into_iter();
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) => match x.cmp(y) {
                Ordering::Equal => continue,
                other => return other,
            },
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => return Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_first_order_with_byte_ties_is_a_strict_order() {
        let paths: [&[u8]; 9] =
            [b"/b", b"/a-b", b"/a/b", b"/a", b"/a/a", b"/", "/é".as_bytes(), "/e\u{301}".as_bytes(), b"/\xff\xfe"];
        let mut sorted = paths.to_vec();
        sorted.sort_by(|a, b| compare(a, b));
        let expected: [&[u8]; 9] =
            [b"/", b"/a", b"/a/a", b"/a/b", b"/a-b", b"/b", "/e\u{301}".as_bytes(), "/é".as_bytes(), b"/\xff\xfe"];
        assert_eq!(sorted, expected);
        for a in paths {
            assert!(!precedes(a, a));
            for b in paths {
                assert!(!(precedes(a, b) && precedes(b, a)));
                for c in paths {
                    if precedes(a, b) && precedes(b, c) {
                        assert!(precedes(a, c));
                    }
                }
            }
        }
        // NFC/NFD spellings of a parent are distinct components ordered by bytes.
        assert!(precedes("/e\u{301}/z".as_bytes(), "/é/a".as_bytes()));
        // Component lists agree with joined paths.
        assert_eq!(compare_components([&b"a"[..], b"b"], [&b"a-b"[..]]), compare(b"/a/b", b"/a-b"));
    }
}
