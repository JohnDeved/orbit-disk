use super::*;
use crate::fold::{fold, fold_query};
use std::cmp::Reverse;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const PIECES: &[&str] = &[
    "café",
    "cafe\u{301}",
    "CAFÉ",
    "日本語",
    "👩🏽‍💻",
    "%_",
    "[x]'",
    "Straße",
    "STRASSE",
    "\u{212A}",
    "が",
    "か\u{3099}",
    "Ångström",
    "\0",
    "-",
    ".dat",
    "report",
    "a",
    "ab",
    "abc",
    "zz",
];

fn random_name(rng: &mut Rng) -> String {
    let mut name = String::new();
    for _ in 0..1 + rng.below(4) {
        if rng.below(3) == 0 {
            for _ in 0..1 + rng.below(5) {
                name.push(b"abcxyz0129"[rng.below(10)] as char);
            }
        } else {
            name.push_str(PIECES[rng.below(PIECES.len())]);
        }
    }
    name
}

fn brute_force(rows: &[(i64, String)], needle: &[u8]) -> Vec<i64> {
    rows.iter().filter(|(_, n)| name_matches(n.as_bytes(), needle)).map(|(id, _)| *id).collect()
}

fn ok() -> Result<(), ()> {
    Ok(())
}

fn check_parity(index: &NameIndex, rows: &[(i64, String)], needle: &[u8]) {
    let expected = brute_force(rows, needle);
    for limit in [0, 1, 50, CANDIDATE_LIMIT, 10_000] {
        let options = QueryOptions { candidate_limit: limit, stop_after_limit: false };
        let matches = index.query(needle, options, ok).unwrap();
        assert_eq!(matches.count, expected.len(), "count for {needle:?}");
        if expected.len() <= limit {
            assert_eq!(matches.ids.as_deref(), Some(&expected[..]), "ids for {needle:?}");
        } else {
            assert_eq!(matches.ids, None, "{needle:?} exceeded its candidate budget");
        }
    }
}

#[test]
fn accelerator_matches_brute_force_on_random_unicode_corpus() {
    let mut rng = Rng(7);
    // Gaps in IDs and a row count that is not a multiple of the block size.
    let mut id = 0;
    let rows: Vec<(i64, String)> = (0..9_001)
        .map(|_| {
            id += 1 + rng.below(3) as i64;
            (id, fold(random_name(&mut rng).as_bytes()))
        })
        .collect();
    let index = NameIndex::build(rows.iter().map(|(id, n)| (*id, n.as_bytes())));
    assert!(index.is_complete());
    assert_eq!(index.high_water(), id);

    let mut needles: Vec<Vec<u8>> = PIECES.iter().map(|p| fold_query(p).into_bytes()).collect();
    needles.extend([b"\0".to_vec(), b"dat\0x".to_vec(), b"not-present".to_vec(), b"q".to_vec(), Vec::new()]);
    // Random 1..=6-byte slices of indexed names, including ones that split UTF-8 sequences and
    // ones that span the boundary between two adjacent names.
    for _ in 0..300 {
        let (_, name) = &rows[rng.below(rows.len())];
        let bytes = name.as_bytes();
        if bytes.is_empty() {
            continue;
        }
        let start = rng.below(bytes.len());
        let len = 1 + rng.below(6);
        needles.push(bytes[start..(start + len).min(bytes.len())].to_vec());
    }
    for pair in rows.windows(2).step_by(997) {
        let (a, b) = (pair[0].1.as_bytes(), pair[1].1.as_bytes());
        let mut spanning = a[a.len().saturating_sub(2)..].to_vec();
        spanning.extend_from_slice(&b[..b.len().min(2)]);
        needles.push(spanning);
    }
    for needle in &needles {
        check_parity(&index, &rows, needle);
    }
}

#[test]
fn swift_fixture_parity_block_boundaries_and_literal_queries() {
    let suffixes = ["été", "e\u{301}", "日本語", "👩🏽‍💻", "%_[quote]'", "repeated"];
    let rows: Vec<(i64, String)> = (0..8_400)
        .map(|row| (row * 3 + 7, fold(format!("common-{row}-{}.dat", suffixes[(row as usize % 11).min(5)]).as_bytes())))
        .collect();
    let index = NameIndex::build(rows.iter().map(|(id, n)| (*id, n.as_bytes())));
    let queries = [
        "common", ".dat", "127-", "128-", "4095-", "4096-", "8399-", "été", "e\u{301}", "日本", "%_[", "quote]'", "a",
        "at",
    ];
    for query in queries {
        check_parity(&index, &rows, fold_query(query).as_bytes());
    }
    // NFC and NFD spellings fold identically, so both find both spellings.
    assert_eq!(
        index.query(fold_query("été").as_bytes(), QueryOptions::default(), ok).unwrap().count,
        index.query(fold_query("e\u{301}te\u{301}").as_bytes(), QueryOptions::default(), ok).unwrap().count,
    );
}

#[test]
fn capped_index_keeps_prefix_and_tail_completes_results() {
    let mut rng = Rng(11);
    let rows: Vec<(i64, String)> = (1..=4_000).map(|id| (id, fold(random_name(&mut rng).as_bytes()))).collect();
    // Budget just above the fixed bucket overhead: admits a prefix, then refuses everything.
    let mut index = NameIndex::with_byte_limit((65_536 * 24 + 40_000) * 2);
    assert!(!index.extend(rows.iter().map(|(id, n)| (*id, n.as_bytes()))));
    assert!(!index.is_complete());
    let prefix = index.len();
    assert!(prefix > 0 && prefix < rows.len());
    assert_eq!(index.high_water(), rows[prefix - 1].0);
    assert!(!index.push(rows.last().unwrap().0 + 1, b"late"), "a capped index admitted a later row");
    assert!(index.retained_bytes() <= (65_536 * 24 + 40_000) * 2);

    for needle in ["a", "ab", "cafe", "日本", "zz", "0"] {
        let needle = fold_query(needle);
        let cached = index.query(needle.as_bytes(), QueryOptions::default(), ok).unwrap();
        let tail =
            rows.iter().filter(|(id, n)| *id > index.high_water() && name_matches(n.as_bytes(), needle.as_bytes()));
        assert_eq!(cached.count + tail.count(), brute_force(&rows, needle.as_bytes()).len());
    }

    let mut tiny = NameIndex::with_byte_limit(1);
    assert!(!tiny.push(1, b"file"));
    assert!(tiny.is_empty() && tiny.high_water() == 0, "the name budget admitted a partial record");
}

#[test]
fn appended_suffix_is_searchable_like_a_rebuild() {
    let mut rng = Rng(5);
    let rows: Vec<(i64, String)> = (1..=3_000).map(|id| (id, fold(random_name(&mut rng).as_bytes()))).collect();
    let mut grown = NameIndex::build(rows[..1_000].iter().map(|(id, n)| (*id, n.as_bytes())));
    assert!(grown.extend(rows[1_000..].iter().map(|(id, n)| (*id, n.as_bytes()))));
    for needle in ["caf", "ab", "語", "x"] {
        check_parity(&grown, &rows, fold_query(needle).as_bytes());
    }
}

#[test]
fn query_checks_cancellation_in_flight_and_can_stop_early() {
    let rows: Vec<(i64, String)> = (1..=8_400).map(|id| (id, format!("common-{id}"))).collect();
    let index = NameIndex::build(rows.iter().map(|(id, n)| (*id, n.as_bytes())));
    let mut checkpoints = 0;
    let result = index.query(b"common", QueryOptions::default(), || {
        checkpoints += 1;
        if checkpoints == 3 { Err("cancelled") } else { Ok(()) }
    });
    assert_eq!(result, Err("cancelled"));
    assert_eq!(checkpoints, 3, "broad matching did not check cancellation while scanning");

    let bounded = index.query(b"common", QueryOptions { candidate_limit: 10, stop_after_limit: true }, ok).unwrap();
    assert_eq!(bounded, Matches { count: 11, ids: None });
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    path: &'static str,
    stamp: u32,
}

impl PreparedKey for Key {
    fn same_database(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

fn prepared(names: usize) -> PreparedIndex {
    let rows: Vec<(i64, String)> = (1..=names as i64).map(|id| (id, format!("name-{id}"))).collect();
    PreparedIndex { index: NameIndex::build(rows.iter().map(|(id, n)| (*id, n.as_bytes()))), generation: None }
}

#[test]
fn prepared_cache_is_lru_by_bytes_and_replaces_stale_database_entries() {
    let one = prepared(10).index.retained_bytes();
    let cache = PreparedIndexes::new(one * 2 + one / 2);
    let (a, b, c) = (Key { path: "a", stamp: 1 }, Key { path: "b", stamp: 1 }, Key { path: "c", stamp: 1 });
    cache.insert(a.clone(), prepared(10));
    cache.insert(b.clone(), prepared(10));
    assert!(cache.lookup(&a).is_some()); // `b` is now least recently used
    cache.insert(c.clone(), prepared(10));
    assert!(cache.lookup(&b).is_none() && cache.lookup(&a).is_some() && cache.lookup(&c).is_some());

    // A new stamp of the same database replaces the old entry; the old key no longer hits.
    let a2 = Key { path: "a", stamp: 2 };
    cache.insert(a2.clone(), prepared(10));
    assert!(cache.lookup(&a).is_none() && cache.lookup(&a2).is_some());
    cache.remove_database(&a);
    assert!(cache.lookup(&a2).is_none());
}

#[test]
fn one_builder_at_a_time() {
    let cache: PreparedIndexes<Key> = PreparedIndexes::default();
    let permit = cache.try_begin(Key { path: "a", stamp: 1 }).unwrap();
    assert!(cache.try_begin(Key { path: "b", stamp: 1 }).is_none());
    // A waiting builder still honours cancellation.
    let mut calls = 0;
    let waited = cache.begin(Key { path: "b", stamp: 1 }, || {
        calls += 1;
        if calls == 2 { Err(()) } else { Ok(()) }
    });
    assert!(waited.is_err());
    drop(permit);
    assert!(cache.begin(Key { path: "b", stamp: 1 }, ok).is_ok());
}

#[test]
fn staged_preparation_waits_for_size_then_doubling() {
    assert!(!preparation_due(8_191, 0));
    assert!(preparation_due(8_192, 0));
    assert!(preparation_due(16_384, 8_192));
    assert!(!preparation_due(16_383, 8_192));
}

#[test]
fn top_k_matches_full_sort_with_ties() {
    let mut rng = Rng(3);
    let items: Vec<(u64, i64)> = (0..5_000).map(|id| (rng.below(40) as u64, id)).collect();
    let mut expected = items.clone();
    expected.sort_by_key(|(size, id)| (Reverse(*size), *id));
    for limit in [0, 1, 300, 10_000] {
        let mut top = TopK::new(limit);
        for (size, id) in &items {
            top.push(size_id_key(*size, *id), ());
        }
        assert_eq!(top.seen(), items.len());
        let got: Vec<(u64, i64)> = top.into_sorted().into_iter().map(|((Reverse(s), id), ())| (s, id)).collect();
        assert_eq!(got, expected[..limit.min(items.len())]);
    }
}
