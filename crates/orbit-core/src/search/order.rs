//! Result ordering shared by every search source (live inventory, stage, saved map), so partial
//! and saved results agree byte-for-byte on ties and on the cutoff.
//!
//! - Default order: size descending, then row/node ID ascending ([`SizeIdKey`]).
//! - Stable path order (used where results from different sources must line up, e.g. the
//!   preview-to-saved handoff): size descending, then full path bytes ascending ([`SizePathKey`]).

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

/// Results returned when the caller does not choose a limit.
pub const DEFAULT_RESULT_LIMIT: usize = 300;
/// Hard cap on returned results; the total match count is still exact.
pub const MAX_RESULT_LIMIT: usize = 10_000;

pub fn clamp_limit(limit: usize) -> usize {
    limit.min(MAX_RESULT_LIMIT)
}

/// Size descending, then ID ascending. Smaller keys rank first.
pub type SizeIdKey = (Reverse<u64>, i64);
/// Size descending, then path bytes ascending. Smaller keys rank first.
pub type SizePathKey = (Reverse<u64>, Vec<u8>);

#[inline]
pub fn size_id_key(size: u64, id: i64) -> SizeIdKey {
    (Reverse(size), id)
}

struct Slot<K, T> {
    key: K,
    value: T,
}

impl<K: Ord, T> PartialEq for Slot<K, T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl<K: Ord, T> Eq for Slot<K, T> {}
impl<K: Ord, T> PartialOrd for Slot<K, T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<K: Ord, T> Ord for Slot<K, T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

/// Keeps the `limit` smallest keys seen, in O(n log limit), and counts everything offered.
/// Keys must be unique for a deterministic result (both standard keys are).
pub struct TopK<K: Ord, T> {
    limit: usize,
    seen: usize,
    /// Max-heap: the worst kept entry is on top.
    heap: BinaryHeap<Slot<K, T>>,
}

impl<K: Ord, T> std::fmt::Debug for TopK<K, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopK").field("limit", &self.limit).field("seen", &self.seen).finish_non_exhaustive()
    }
}

impl<K: Ord, T> TopK<K, T> {
    pub fn new(limit: usize) -> Self {
        Self { limit, seen: 0, heap: BinaryHeap::with_capacity(limit.min(1024) + 1) }
    }

    /// Number of entries offered (the total match count).
    pub fn seen(&self) -> usize {
        self.seen
    }

    /// The worst kept key once the heap is full; anything not smaller is rejected. Lets callers
    /// skip building expensive keys (e.g. paths) for candidates that cannot rank.
    pub fn threshold(&self) -> Option<&K> {
        if self.heap.len() >= self.limit { self.heap.peek().map(|s| &s.key) } else { None }
    }

    /// Counts a match without offering it (it is known not to rank, see [`Self::threshold`]).
    pub fn skip(&mut self) {
        self.seen += 1;
    }

    pub fn push(&mut self, key: K, value: T) {
        self.seen += 1;
        if self.limit == 0 {
            return;
        }
        if self.heap.len() < self.limit {
            self.heap.push(Slot { key, value });
        } else if let Some(mut worst) = self.heap.peek_mut()
            && key < worst.key
        {
            *worst = Slot { key, value };
        }
    }

    /// Kept entries, best first.
    pub fn into_sorted(self) -> Vec<(K, T)> {
        self.heap.into_sorted_vec().into_iter().map(|s| (s.key, s.value)).collect()
    }
}
