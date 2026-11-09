use super::NameIndex;
use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::time::Duration;

/// Estimated bytes the process retains across all prepared indexes (Swift: 128 MiB). Not an RSS
/// guarantee: readers may hold evicted indexes and a builder may be running.
pub const CACHE_BYTE_LIMIT: usize = 128 * 1024 * 1024;
/// Staged maps prepare their first prefix after this many observed entries.
const STAGED_PREPARATION_MIN: u64 = 8_192;
/// How often a waiting builder re-runs its cancellation check.
const BUILDER_POLL: Duration = Duration::from_millis(20);

/// A stage should (re)build its search prefix: it has at least 8,192 entries and has doubled
/// since the last preparation. Doubling bounds total rebuild work; newer rows stay searchable in
/// the tail.
pub fn preparation_due(item_count: u64, previous_count: u64) -> bool {
    item_count >= STAGED_PREPARATION_MIN && previous_count <= item_count / 2
}

/// Identifies one prepared index. Saved maps key on the database file's identity and stamps so
/// a replaced file never serves an old index; staged keys omit the changing size/mtime and carry a
/// name generation in [`PreparedIndex::generation`] instead.
pub trait PreparedKey: Clone + Eq + Send {
    /// Both keys describe the same database (path); inserting one evicts the other.
    fn same_database(&self, other: &Self) -> bool;
}

#[derive(Debug)]
pub struct PreparedIndex {
    pub index: NameIndex,
    /// Staged maps: the search-name generation the prefix was built against. A different current
    /// generation means an indexed name changed and the prefix must not be used.
    pub generation: Option<i64>,
}

#[derive(Debug)]
struct State<K> {
    /// Least recently used first.
    entries: Vec<(K, Arc<PreparedIndex>)>,
    building: Option<K>,
}

/// The process-wide LRU of prepared indexes, admitting one builder at a time.
#[derive(Debug)]
pub struct PreparedIndexes<K> {
    byte_limit: usize,
    state: Mutex<State<K>>,
    idle: Condvar,
}

impl<K: PreparedKey> Default for PreparedIndexes<K> {
    fn default() -> Self {
        Self::new(CACHE_BYTE_LIMIT)
    }
}

impl<K: PreparedKey> PreparedIndexes<K> {
    pub fn new(byte_limit: usize) -> Self {
        Self { byte_limit, state: Mutex::new(State { entries: Vec::new(), building: None }), idle: Condvar::new() }
    }

    /// The index for `key`, marking it most recently used.
    pub fn lookup(&self, key: &K) -> Option<Arc<PreparedIndex>> {
        let mut state = self.state.lock();
        let position = state.entries.iter().position(|(k, _)| k == key)?;
        let entry = state.entries.remove(position);
        let value = entry.1.clone();
        state.entries.push(entry);
        Some(value)
    }

    /// Waits until no other index is being built, then claims the builder slot for `key`.
    /// `check` runs at least every 20 ms while waiting; its error abandons the wait.
    pub fn begin<E>(&self, key: K, mut check: impl FnMut() -> Result<(), E>) -> Result<BuildPermit<'_, K>, E> {
        let mut state = self.state.lock();
        loop {
            check()?;
            if state.building.is_none() {
                state.building = Some(key);
                return Ok(BuildPermit { cache: self });
            }
            self.idle.wait_for(&mut state, BUILDER_POLL);
        }
    }

    /// Claims the builder slot without waiting.
    pub fn try_begin(&self, key: K) -> Option<BuildPermit<'_, K>> {
        let mut state = self.state.lock();
        if state.building.is_some() {
            return None;
        }
        state.building = Some(key);
        Some(BuildPermit { cache: self })
    }

    /// Stores a finished index: replaces any index of the same database, then evicts least
    /// recently used entries until the estimate fits (the new entry is always kept).
    pub fn insert(&self, key: K, prepared: PreparedIndex) -> Arc<PreparedIndex> {
        let prepared = Arc::new(prepared);
        let mut state = self.state.lock();
        state.entries.retain(|(k, _)| !k.same_database(&key));
        let mut total: usize = prepared.index.retained_bytes()
            + state.entries.iter().map(|(_, v)| v.index.retained_bytes()).sum::<usize>();
        while total > self.byte_limit && !state.entries.is_empty() {
            let (_, evicted) = state.entries.remove(0);
            total -= evicted.index.retained_bytes();
        }
        state.entries.push((key, prepared.clone()));
        prepared
    }

    /// Drops every index of the database `key` describes (e.g. the file was deleted).
    pub fn remove_database(&self, key: &K) {
        self.state.lock().entries.retain(|(k, _)| !k.same_database(key));
    }

    pub fn retained_bytes(&self) -> usize {
        self.state.lock().entries.iter().map(|(_, v)| v.index.retained_bytes()).sum()
    }
}

/// The builder slot; released on drop.
#[derive(Debug)]
pub struct BuildPermit<'a, K: PreparedKey> {
    cache: &'a PreparedIndexes<K>,
}

impl<K: PreparedKey> Drop for BuildPermit<'_, K> {
    fn drop(&mut self) {
        self.cache.state.lock().building = None;
        self.cache.idle.notify_all();
    }
}
