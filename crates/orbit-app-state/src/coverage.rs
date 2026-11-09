//! Collection coverage: which paths are collected (hatched + check badge) and which contain
//! collected items (minus badge). Port of Swift `CollectionCoverage`. Built once per collection
//! change, never queried against the filesystem.

use std::sync::Arc;

pub use orbit_sunburst::CollectionState;

use crate::hash::FxHashSet;
use crate::paths::{normalized, parent};

#[derive(Clone, Debug, Default)]
pub struct CollectionCoverage {
    collected: FxHashSet<Arc<[u8]>>,
    ancestors: FxHashSet<Arc<[u8]>>,
}

impl CollectionCoverage {
    pub fn new<'a>(paths: impl IntoIterator<Item = &'a [u8]>) -> Self {
        let mut coverage = Self::default();
        for path in paths {
            let path = normalized(path);
            if path.is_empty() {
                continue;
            }
            coverage.collected.insert(Arc::from(&*path));
            let mut ancestor = parent(&path);
            while let Some(current) = ancestor {
                // A previously visited ancestor already has its complete parent chain.
                if coverage.ancestors.contains(current) {
                    break;
                }
                coverage.ancestors.insert(Arc::from(current));
                ancestor = parent(current);
            }
        }
        coverage
    }

    pub fn is_empty(&self) -> bool {
        self.collected.is_empty()
    }

    pub fn len(&self) -> usize {
        self.collected.len()
    }

    /// The collected path set (normalized).
    pub fn collected(&self) -> &FxHashSet<Arc<[u8]>> {
        &self.collected
    }

    pub fn state(&self, path: &[u8]) -> CollectionState {
        if self.is_empty() {
            return CollectionState::None;
        }
        let path = normalized(path);
        if path.is_empty() {
            return CollectionState::None;
        }
        let mut candidate: Option<&[u8]> = Some(&path);
        while let Some(current) = candidate {
            if self.collected.contains(current) {
                return CollectionState::Included;
            }
            candidate = parent(current);
        }
        if self.ancestors.contains(&*path) { CollectionState::Partial } else { CollectionState::None }
    }

    /// Combined state of several paths: all included → included, any touched → partial.
    pub fn state_of_all<'a>(&self, paths: impl IntoIterator<Item = &'a [u8]>) -> CollectionState {
        if self.is_empty() {
            return CollectionState::None;
        }
        let mut all_included = true;
        let mut any = false;
        let mut empty = true;
        for path in paths {
            empty = false;
            let state = self.state(path);
            all_included &= state == CollectionState::Included;
            any |= state != CollectionState::None;
            if any && !all_included {
                return CollectionState::Partial;
            }
        }
        if empty {
            CollectionState::None
        } else if all_included {
            CollectionState::Included
        } else if any {
            CollectionState::Partial
        } else {
            CollectionState::None
        }
    }

    /// State of a group segment: `member_count` distinct children of `parent`. When the collection
    /// is smaller than the group, walk the collection instead of the members: the group is
    /// included only through `parent`, and partial when some collected path lies under a member
    /// (`is_member` answers membership for a direct child path of `parent`). Otherwise falls back
    /// to `per_member` (typically [`Self::state_of_all`] over the member paths).
    pub fn state_of_group(
        &self,
        parent_path: &[u8],
        member_count: usize,
        distinct: bool,
        mut is_member: impl FnMut(&[u8]) -> bool,
        per_member: impl FnOnce(&Self) -> CollectionState,
    ) -> CollectionState {
        if self.is_empty() || member_count == 0 {
            return CollectionState::None;
        }
        if !distinct || self.collected.len() >= member_count {
            return per_member(self);
        }
        if self.state(parent_path) == CollectionState::Included {
            return CollectionState::Included;
        }
        let parent_path = normalized(parent_path);
        for path in &self.collected {
            let mut current: &[u8] = path;
            while let Some(ancestor) = parent(current) {
                if ancestor == &*parent_path {
                    if is_member(current) {
                        return CollectionState::Partial;
                    }
                    break;
                }
                current = ancestor;
            }
        }
        CollectionState::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CollectionState::*;

    #[test]
    fn coverage_respects_component_boundaries_unicode_spellings_and_root() {
        let nfc = "/r/caf\u{e9}";
        let nfd = "/r/cafe\u{301}";
        let coverage = CollectionCoverage::new([&b"/r/a/b"[..], b"/r/x/", nfc.as_bytes()]);
        assert_eq!(coverage.state(b"/r/a/b"), Included);
        assert_eq!(coverage.state(b"/r/a/b/c"), Included);
        assert_eq!(coverage.state(b"/r/a"), Partial);
        assert_eq!(coverage.state(b"/r"), Partial);
        assert_eq!(coverage.state(b"/"), Partial);
        assert_eq!(coverage.state(b"/r/a/bc"), None);
        assert_eq!(coverage.state(b"/r/x/y"), Included);
        assert_eq!(coverage.state(nfc.as_bytes()), Included);
        assert_eq!(coverage.state(nfd.as_bytes()), None);
        assert_eq!(coverage.state(format!("{nfd}/f").as_bytes()), None);
        assert_eq!(CollectionCoverage::new([&b"/"[..]]).state(b"/anything"), Included);
        assert_eq!(coverage.state_of_all([&b"/r/a/b"[..], b"/r/x"]), Included);
        assert_eq!(coverage.state_of_all([&b"/r/a/b"[..], b"/r/q"]), Partial);
        assert_eq!(coverage.state_of_all([&b"/r/q"[..]]), None);
    }

    /// Grouped coverage must equal per-member coverage for every subset of a fixture, including
    /// Unicode spellings, ancestors, descendants and duplicate members.
    #[test]
    fn grouped_coverage_matches_per_member_coverage_exhaustively() {
        let parent = "/r/p";
        let members = ["/r/p/a", "/r/p/b", "/r/p/caf\u{e9}", "/r/p/d"];
        let candidates =
            ["/r/p/a", "/r/p/b/deep", "/r/p/cafe\u{301}", "/r/p/caf\u{e9}", "/r/p", "/r/p/zz", "/r/q", "/r/p/d/x"];
        for mask in 0u32..(1 << candidates.len()) {
            let chosen: Vec<&[u8]> = candidates
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, p)| p.as_bytes())
                .collect();
            let coverage = CollectionCoverage::new(chosen.iter().copied());
            for take in 1..=members.len() {
                let group = &members[..take];
                let expected = coverage.state_of_all(group.iter().map(|m| m.as_bytes()));
                let grouped = coverage.state_of_group(
                    parent.as_bytes(),
                    group.len(),
                    true,
                    |child| group.iter().any(|m| m.as_bytes() == child),
                    |coverage| coverage.state_of_all(group.iter().map(|m| m.as_bytes())),
                );
                assert_eq!(grouped, expected, "mask {mask:#b} take {take}");
            }
            // Duplicate members are not distinct: always the per-member answer.
            let dup = [members[0], members[0]];
            let grouped = coverage.state_of_group(
                parent.as_bytes(),
                2,
                false,
                |_| true,
                |coverage| coverage.state_of_all(dup.iter().map(|m| m.as_bytes())),
            );
            assert_eq!(grouped, coverage.state_of_all(dup.iter().map(|m| m.as_bytes())));
        }
    }
}
