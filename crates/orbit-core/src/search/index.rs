use memchr::memmem;

/// Records per candidate block. Postings name blocks, not rows, which keeps them small.
pub const BLOCK_RECORDS: usize = 128;
/// Per-index byte budget (Swift: 96 MiB). An index that reaches it keeps its prefix.
pub const BUILDER_BYTE_LIMIT: usize = 96 * 1024 * 1024;
/// Queries matching more rows than this report only a count; the caller falls back to a scan
/// ordered by the database (Swift: 2,048).
pub const CANDIDATE_LIMIT: usize = 2_048;

const BUCKETS: usize = 1 << 16;
/// Cancellation is checked before every this many candidate blocks.
const CHECK_INTERVAL: usize = 32;
/// Fixed per-bucket overhead used by the size estimate (one `Vec` header each).
const BUCKET_OVERHEAD: usize = 24;

#[inline]
fn bucket(a: u8, b: u8, c: u8) -> usize {
    ((u32::from(a).wrapping_mul(25_193) ^ u32::from(b).wrapping_mul(4_051) ^ u32::from(c).wrapping_mul(97)) & 0xFFFF)
        as usize
}

/// `folded_name` contains `folded_needle` (both from [`crate::fold`]). An empty needle matches
/// nothing, like an empty search. This is the reference semantics the index accelerates; use it
/// for rows beyond an index's high-water mark.
#[inline]
pub fn name_matches(folded_name: &[u8], folded_needle: &[u8]) -> bool {
    !folded_needle.is_empty() && memmem::find(folded_name, folded_needle).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryOptions {
    /// Row IDs are returned only while the match count stays within this limit.
    pub candidate_limit: usize,
    /// Stop scanning once the limit is exceeded; [`Matches::count`] is then a lower bound
    /// (`candidate_limit + 1`). Use when only "selective or not" matters.
    pub stop_after_limit: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self { candidate_limit: CANDIDATE_LIMIT, stop_after_limit: false }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matches {
    /// Indexed rows whose folded name contains the needle (a lower bound when the scan stopped
    /// early, see [`QueryOptions::stop_after_limit`]).
    pub count: usize,
    /// Matching row IDs in ascending order, or `None` when there are more than the candidate
    /// limit ("too many": order and limit the query in the database instead).
    pub ids: Option<Vec<i64>>,
}

/// Folded names in one byte arena with offsets and row IDs, plus hashed three-byte postings over
/// blocks of [`BLOCK_RECORDS`] records. Append-only: rows must arrive in ascending ID order.
pub struct NameIndex {
    names: Vec<u8>,
    /// `offsets[i]..offsets[i + 1]` is row `i`'s name; `len == rows + 1`.
    offsets: Vec<u32>,
    ids: Vec<i64>,
    /// Per hash bucket: ascending block numbers containing that trigram.
    postings: Box<[Vec<u32>]>,
    /// Per bucket: the last block appended to its posting list (dedupes within a block).
    last_block: Box<[u32]>,
    posting_count: usize,
    byte_limit: usize,
    capped: bool,
}

impl std::fmt::Debug for NameIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NameIndex")
            .field("records", &self.ids.len())
            .field("high_water", &self.high_water())
            .field("complete", &!self.capped)
            .field("retained_bytes", &self.retained_bytes())
            .finish()
    }
}

impl Default for NameIndex {
    fn default() -> Self {
        Self::with_byte_limit(BUILDER_BYTE_LIMIT)
    }
}

impl NameIndex {
    pub fn with_byte_limit(byte_limit: usize) -> Self {
        Self {
            names: Vec::new(),
            offsets: vec![0],
            ids: Vec::new(),
            postings: vec![Vec::new(); BUCKETS].into_boxed_slice(),
            last_block: vec![u32::MAX; BUCKETS].into_boxed_slice(),
            posting_count: 0,
            byte_limit,
            capped: false,
        }
    }

    /// Builds an index from `(row_id, folded_name)` rows in ascending ID order, stopping at the
    /// default byte budget. Check [`Self::is_complete`] afterwards.
    pub fn build<'a>(rows: impl IntoIterator<Item = (i64, &'a [u8])>) -> Self {
        let mut index = Self::default();
        index.extend(rows);
        index
    }

    /// Appends rows until one is refused. Returns `false` if the index is (now) capped.
    pub fn extend<'a>(&mut self, rows: impl IntoIterator<Item = (i64, &'a [u8])>) -> bool {
        rows.into_iter().all(|(id, name)| self.push(id, name))
    }

    /// Appends one row. Returns `false`, and refuses every later row, once the byte budget or the
    /// 32-bit arena limits are reached, so the index always covers exactly `id <= high_water()`.
    ///
    /// # Panics
    /// If `id` is not greater than [`Self::high_water`]; a non-monotonic ID would let tail
    /// searches skip rows.
    pub fn push(&mut self, id: i64, folded_name: &[u8]) -> bool {
        assert!(self.ids.last().is_none_or(|last| id > *last), "NameIndex rows must have ascending IDs");
        if self.capped {
            return false;
        }
        // Leave room for geometric capacity growth before admitting another record (Swift).
        let admitted = self.estimated_bytes() + folded_name.len() * 5 + 12 <= self.byte_limit / 2
            && self.ids.len() < u32::MAX as usize
            && self.names.len() + folded_name.len() < u32::MAX as usize;
        if !admitted {
            self.capped = true;
            return false;
        }
        let block = (self.ids.len() / BLOCK_RECORDS) as u32;
        for w in folded_name.windows(3) {
            let b = bucket(w[0], w[1], w[2]);
            if self.last_block[b] != block {
                self.last_block[b] = block;
                self.postings[b].push(block);
                self.posting_count += 1;
            }
        }
        self.names.extend_from_slice(folded_name);
        self.offsets.push(self.names.len() as u32);
        self.ids.push(id);
        true
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The highest indexed row ID (0 when empty). Rows above it are the caller's tail.
    pub fn high_water(&self) -> i64 {
        self.ids.last().copied().unwrap_or(0)
    }

    /// No row was refused for lack of budget.
    pub fn is_complete(&self) -> bool {
        !self.capped
    }

    fn estimated_bytes(&self) -> usize {
        self.names.len()
            + self.offsets.len() * 4
            + self.ids.len() * 8
            + self.posting_count * 4
            + BUCKETS * BUCKET_OVERHEAD
    }

    /// Conservative retained-memory estimate used by the process cache (twice the payload, to
    /// account for spare `Vec` capacity), as in Swift.
    pub fn retained_bytes(&self) -> usize {
        self.estimated_bytes() * 2
    }

    /// Finds indexed rows whose folded name contains `folded_needle`.
    ///
    /// `check` runs before the scan, before every 32 candidate blocks and at the end; its error
    /// aborts the query (wire cancellation through it).
    pub fn query<E>(
        &self,
        folded_needle: &[u8],
        options: QueryOptions,
        mut check: impl FnMut() -> Result<(), E>,
    ) -> Result<Matches, E> {
        check()?;
        if folded_needle.is_empty() || self.ids.is_empty() {
            return Ok(Matches { count: 0, ids: Some(Vec::new()) });
        }
        let candidates = match self.candidate_blocks(folded_needle) {
            Candidates::None => return Ok(Matches { count: 0, ids: Some(Vec::new()) }),
            other => other,
        };
        let block_count = self.ids.len().div_ceil(BLOCK_RECORDS);
        let finder = memmem::Finder::new(folded_needle);
        let mut scan =
            Scan { index: self, finder: &finder, needle_len: folded_needle.len(), options, count: 0, ids: Vec::new() };
        match &candidates {
            Candidates::All => {
                for start in (0..block_count).step_by(CHECK_INTERVAL) {
                    check()?;
                    if !scan.blocks(start, (start + CHECK_INTERVAL).min(block_count)) {
                        break;
                    }
                }
            }
            Candidates::Blocks(blocks) => {
                'chunks: for chunk in blocks.chunks(CHECK_INTERVAL) {
                    check()?;
                    // Adjacent blocks are contiguous in the arena: search each run in one pass.
                    let mut i = 0;
                    while i < chunk.len() {
                        let first = chunk[i] as usize;
                        let mut end = first + 1;
                        i += 1;
                        while i < chunk.len() && chunk[i] as usize == end {
                            end += 1;
                            i += 1;
                        }
                        if !scan.blocks(first, end) {
                            break 'chunks;
                        }
                    }
                }
            }
            Candidates::None => unreachable!(),
        }
        check()?;
        let Scan { count, ids, .. } = scan;
        Ok(Matches { count, ids: (count <= options.candidate_limit).then_some(ids) })
    }

    /// Blocks that can contain the needle: the intersection of the posting lists of all its
    /// trigrams (or every block for needles shorter than three bytes).
    fn candidate_blocks(&self, needle: &[u8]) -> Candidates {
        if needle.len() < 3 {
            return Candidates::All;
        }
        let mut buckets: Vec<usize> = needle.windows(3).map(|w| bucket(w[0], w[1], w[2])).collect();
        buckets.sort_unstable();
        buckets.dedup();
        let mut lists: Vec<&[u32]> = buckets.iter().map(|b| self.postings[*b].as_slice()).collect();
        lists.sort_unstable_by_key(|l| l.len());
        if lists[0].is_empty() {
            return Candidates::None;
        }
        let mut blocks = lists[0].to_vec();
        for list in &lists[1..] {
            intersect_in_place(&mut blocks, list);
            if blocks.is_empty() {
                return Candidates::None;
            }
        }
        Candidates::Blocks(blocks)
    }
}

enum Candidates {
    None,
    All,
    Blocks(Vec<u32>),
}

/// Keeps the elements of `target` (ascending) that also occur in `other` (ascending).
fn intersect_in_place(target: &mut Vec<u32>, other: &[u32]) {
    let mut j = 0;
    target.retain(|value| {
        // Gallop: `other` is usually much longer than `target`.
        let rest = &other[j..];
        let skip = rest.partition_point(|x| x < value);
        j += skip;
        other.get(j) == Some(value)
    });
}

struct Scan<'a> {
    index: &'a NameIndex,
    finder: &'a memmem::Finder<'a>,
    needle_len: usize,
    options: QueryOptions,
    count: usize,
    ids: Vec<i64>,
}

impl Scan<'_> {
    /// Scans the rows of blocks `first..end`. Returns `false` to stop the whole query.
    fn blocks(&mut self, first: usize, end: usize) -> bool {
        let index = self.index;
        let rows = index.ids.len();
        let row_start = first * BLOCK_RECORDS;
        let row_end = (end * BLOCK_RECORDS).min(rows);
        let base = index.offsets[row_start] as usize;
        let hay = &index.names[base..index.offsets[row_end] as usize];
        let (mut pos, mut row) = (0, row_start);
        while pos < hay.len() {
            let Some(hit) = self.finder.find(&hay[pos..]) else { break };
            let at = base + pos + hit;
            while index.offsets[row + 1] as usize <= at {
                row += 1;
            }
            let name_end = index.offsets[row + 1] as usize;
            if at + self.needle_len > name_end {
                // The hit spans two names; keep looking from the next byte.
                pos = at - base + 1;
                continue;
            }
            self.count += 1;
            if self.count <= self.options.candidate_limit {
                self.ids.push(index.ids[row]);
            } else if self.options.stop_after_limit {
                return false;
            }
            pos = name_end - base;
            row += 1;
        }
        true
    }
}
