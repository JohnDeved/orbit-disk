//! Name-sort collation: the replacement for Foundation's `localizedStandardCompare`.
//!
//! Ordering is ICU (CLDR) collation with numeric ordering on: case-insensitive at the first
//! level ("a" < "B" < "c"), digit runs compared by value ("file2" < "file10"), diacritics at the
//! second level and case at the third ("file" < "File"), and canonically equivalent spellings
//! (NFC/NFD) compare equal. Leading zeros are not significant ("a01" == "a1"); callers that need a
//! total order break ties themselves (e.g. by name bytes or ID).
//!
//! Most names are ASCII, so [`Collator::compare`] uses a hand-written natural comparison for two
//! ASCII names. Its weight table is derived from the ICU collator at construction and the fast
//! path is disabled for locales whose tailoring it cannot reproduce (contractions such as Czech
//! "ch" or Danish "aa", or second-level differences between ASCII letters). Results are identical
//! to ICU, which the tests verify on random ASCII input. Everything else goes through ICU.
//!
//! For large sorts precompute [`SortKey`]s ([`Collator::sort_key`]): they are byte strings whose
//! order equals [`Collator::compare`]. `orbit-bench` (`collate/*`) compares both strategies.
//!
//! Names that are not valid UTF-8 are compared as if invalid sequences were U+FFFD.

use icu_collator::options::{CollatorOptions, Strength};
use icu_collator::preferences::CollationNumericOrdering;
use icu_collator::{CollatorBorrowed, CollatorPreferences};
use std::cmp::Ordering;
use std::sync::OnceLock;

/// Digit runs longer than this go through ICU (which splits very long numbers into segments).
const MAX_FAST_DIGITS: usize = 200;
/// Sorts at least this large use precomputed sort keys in [`Collator::sort_by_name`]; keys are
/// about 2.5x faster than comparisons from 100k names up (`orbit-bench` `collate/*`).
const SORT_KEY_THRESHOLD: usize = 2_000;

/// Compares two names with the shared root-locale collator.
pub fn compare(a: &[u8], b: &[u8]) -> Ordering {
    root().compare(a, b)
}

/// The process-wide root-locale collator.
pub fn root() -> &'static Collator {
    static ROOT: OnceLock<Collator> = OnceLock::new();
    ROOT.get_or_init(Collator::new)
}

/// Precomputed collation key; byte order equals [`Collator::compare`] order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SortKey(Box<[u8]>);

impl SortKey {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug)]
pub struct Collator {
    icu: CollatorBorrowed<'static>,
    prefs: CollatorPreferences,
    ascii: Option<Box<AsciiTable>>,
}

impl Default for Collator {
    fn default() -> Self {
        Self::new()
    }
}

impl Collator {
    /// Root-locale (CLDR default) ordering.
    pub fn new() -> Self {
        Self::with_preferences(CollatorPreferences::default()).expect("compiled root collation data")
    }

    /// Ordering for a BCP 47 locale (e.g. `"sv-SE"`); falls back to the root ordering when the
    /// tag is malformed or has no data.
    pub fn for_locale(tag: &str) -> Self {
        icu_locale_core::Locale::try_from_str(tag)
            .ok()
            .and_then(|locale| Self::with_preferences(CollatorPreferences::from(&locale)))
            .unwrap_or_default()
    }

    fn with_preferences(mut prefs: CollatorPreferences) -> Option<Self> {
        prefs.numeric_ordering = Some(CollationNumericOrdering::True);
        let icu = icu_with(prefs, Strength::Tertiary)?;
        let ascii = AsciiTable::derive(prefs, &icu).map(Box::new);
        Some(Self { icu, prefs, ascii })
    }

    /// Enables or disables the ASCII fast path (benchmarks and parity tests). Enabling has no
    /// effect when the locale does not support it.
    pub fn with_ascii_fast_path(mut self, enabled: bool) -> Self {
        if !enabled {
            self.ascii = None;
        } else if self.ascii.is_none() {
            self.ascii = AsciiTable::derive(self.prefs, &self.icu).map(Box::new);
        }
        self
    }

    pub fn has_ascii_fast_path(&self) -> bool {
        self.ascii.is_some()
    }

    pub fn compare(&self, a: &[u8], b: &[u8]) -> Ordering {
        if let Some(table) = &self.ascii
            && a.is_ascii()
            && b.is_ascii()
            && let Some(order) = table.compare(a, b)
        {
            return order;
        }
        self.icu.compare_utf8(a, b)
    }

    pub fn sort_key(&self, name: &[u8]) -> SortKey {
        let mut out = Vec::with_capacity(name.len() * 3 + 4);
        self.write_sort_key(name, &mut out);
        SortKey(out.into_boxed_slice())
    }

    /// Appends the sort key of `name` to `out`.
    pub fn write_sort_key(&self, name: &[u8], out: &mut Vec<u8>) {
        let Ok(()) = self.icu.write_sort_key_utf8_to(name, out);
    }

    /// Stable sort by collated name. Large inputs use precomputed sort keys.
    pub fn sort_by_name<T>(&self, items: &mut [T], name: impl Fn(&T) -> &[u8]) {
        if items.len() < SORT_KEY_THRESHOLD {
            items.sort_by(|a, b| self.compare(name(a), name(b)));
            return;
        }
        let mut keys: Vec<(SortKey, u32)> =
            items.iter().enumerate().map(|(i, item)| (self.sort_key(name(item)), i as u32)).collect();
        // Index tie-break keeps the sort stable.
        keys.sort_unstable();
        let mut order: Vec<u32> = keys.into_iter().map(|(_, i)| i).collect();
        apply_permutation(items, &mut order);
    }
}

/// Reorders `items` so that `items[k]` becomes the old `items[order[k]]`. Clobbers `order`.
fn apply_permutation<T>(items: &mut [T], order: &mut [u32]) {
    for start in 0..order.len() {
        let mut current = start;
        while order[current] as usize != start {
            let next = order[current] as usize;
            items.swap(current, next);
            order[current] = current as u32;
            current = next;
        }
        order[current] = current as u32;
    }
}

fn icu_with(prefs: CollatorPreferences, strength: Strength) -> Option<CollatorBorrowed<'static>> {
    let mut options = CollatorOptions::default();
    options.strength = Some(strength);
    CollatorBorrowed::try_new(prefs, options).ok()
}

const IGNORABLE: u8 = 0;

/// Collation weights for ASCII derived from ICU: a first-level rank per byte (0 = ignorable;
/// digits share `digit_rank` and are compared as numbers) and a third-level rank within each
/// first-level group (case).
#[derive(Debug)]
struct AsciiTable {
    primary: [u8; 128],
    tertiary: [u8; 128],
    digit_rank: u8,
}

enum Token<'a> {
    Char(u8),
    /// Digits without leading zeros ("0" for a run of zeros).
    Number(&'a [u8]),
}

impl AsciiTable {
    fn derive(prefs: CollatorPreferences, tertiary: &CollatorBorrowed<'static>) -> Option<Self> {
        let primary = icu_with(prefs, Strength::Primary)?;
        let secondary = icu_with(prefs, Strength::Secondary)?;
        let mut table = Self { primary: [IGNORABLE; 128], tertiary: [0; 128], digit_rank: 0 };
        let mut weighted: Vec<u8> = Vec::new();
        for c in 0u8..128 {
            if c.is_ascii_digit() && c != b'0' {
                continue;
            }
            if primary.compare_utf8(&[c], b"") == Ordering::Equal {
                // Only completely ignorable characters can be skipped.
                if tertiary.compare_utf8(&[c], b"") != Ordering::Equal {
                    return None;
                }
            } else {
                weighted.push(c);
            }
        }
        weighted.sort_by(|a, b| tertiary.compare_utf8(&[*a], &[*b]));
        let (mut rank, mut group_start) = (0u8, 0usize);
        for i in 0..weighted.len() {
            let c = weighted[i];
            if i == 0 || primary.compare_utf8(&[weighted[i - 1]], &[c]) != Ordering::Equal {
                rank += 1;
                group_start = i;
            } else if secondary.compare_utf8(&[weighted[group_start]], &[c]) != Ordering::Equal || c == b'0' {
                // Second-level differences (or a digit sharing a first-level weight) are not
                // modelled by the fast path.
                return None;
            }
            table.primary[c as usize] = rank;
            let tertiary_rank = weighted[group_start..=i]
                .windows(2)
                .filter(|w| tertiary.compare_utf8(&[w[0]], &[w[1]]) != Ordering::Equal)
                .count();
            table.tertiary[c as usize] = tertiary_rank as u8;
        }
        table.digit_rank = table.primary[b'0' as usize];
        for d in b'1'..=b'9' {
            table.primary[d as usize] = table.digit_rank;
        }
        // Contractions (Czech "ch", Danish "aa", ...) change multi-letter order; detect them by
        // checking that ICU agrees with the fast path on every adjacent pair of two-letter names.
        let pairs: Vec<[u8; 2]> = (b'a'..=b'z').flat_map(|x| (b'a'..=b'z').map(move |y| [x, y])).collect();
        let mut sorted = pairs.clone();
        sorted.sort_by(|a, b| table.compare(a, b).unwrap_or(Ordering::Equal));
        let consistent = sorted.windows(2).all(|w| {
            let fast = table.compare(&w[0], &w[1]);
            fast == Some(tertiary.compare_utf8(&w[0], &w[1]))
        });
        consistent.then_some(table)
    }

    #[inline]
    fn next_token<'a>(&self, s: &'a [u8], i: &mut usize) -> Option<Token<'a>> {
        while *i < s.len() {
            let c = s[*i];
            if c.is_ascii_digit() {
                let start = *i;
                while *i < s.len() && s[*i].is_ascii_digit() {
                    *i += 1;
                }
                let run = &s[start..*i];
                let zeros = run.iter().take_while(|d| **d == b'0').count().min(run.len() - 1);
                return Some(Token::Number(&run[zeros..]));
            }
            *i += 1;
            let rank = self.primary[c as usize];
            if rank != IGNORABLE {
                return Some(Token::Char(rank));
            }
        }
        None
    }

    /// ICU-equivalent comparison of two ASCII strings, or `None` to defer to ICU.
    fn compare(&self, a: &[u8], b: &[u8]) -> Option<Ordering> {
        // Skip the identical prefix, backing up to the start of a digit run it ends in.
        let mut start = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        if start == a.len() && start == b.len() {
            return Some(Ordering::Equal);
        }
        while start > 0 && a[start - 1].is_ascii_digit() {
            start -= 1;
        }
        let (a, b) = (&a[start..], &b[start..]);
        let (mut i, mut j) = (0, 0);
        loop {
            let order = match (self.next_token(a, &mut i), self.next_token(b, &mut j)) {
                (None, None) => break,
                (None, Some(_)) => return Some(Ordering::Less),
                (Some(_), None) => return Some(Ordering::Greater),
                (Some(Token::Char(x)), Some(Token::Char(y))) => x.cmp(&y),
                (Some(Token::Char(x)), Some(Token::Number(_))) => x.cmp(&self.digit_rank),
                (Some(Token::Number(_)), Some(Token::Char(y))) => self.digit_rank.cmp(&y),
                (Some(Token::Number(x)), Some(Token::Number(y))) => {
                    if x.len() > MAX_FAST_DIGITS || y.len() > MAX_FAST_DIGITS {
                        return None;
                    }
                    x.len().cmp(&y.len()).then_with(|| x.cmp(y))
                }
            };
            if order != Ordering::Equal {
                return Some(order);
            }
        }
        // First level equal: compare case (third level) of the weighted non-digit characters,
        // which now correspond one to one.
        Some(self.case_ranks(a).cmp(self.case_ranks(b)))
    }

    fn case_ranks<'s>(&'s self, s: &'s [u8]) -> impl Iterator<Item = u8> + 's {
        s.iter()
            .filter(|c| !c.is_ascii_digit() && self.primary[**c as usize] != IGNORABLE)
            .map(|c| self.tertiary[*c as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_normalization::UnicodeNormalization;

    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    #[test]
    fn finder_like_order_table() {
        let c = root();
        // Each row sorts strictly before the next.
        let ascending: &[&str] = &[
            " leading space",
            "_underscore",
            "-dash",
            "(paren)",
            "$dollar",
            "1",
            "2",
            "10",
            "a",
            "a b",
            "a1",
            "a2",
            "a10",
            "ab",
            "Ab",
            "Ångström",
            "b",
            "file",
            "File",
            "file 2",
            "file2",
            "file2b",
            "file10",
            "file10.txt",
            "file100",
            "resume",
            "résumé",
            "Résumé",
            "resumf",
            "x99999999999999999999",
            "x100000000000000000000",
            "zebra",
            "Ελλάδα",
            "日本",
        ];
        for pair in ascending.windows(2) {
            assert_eq!(c.compare(pair[0].as_bytes(), pair[1].as_bytes()), Ordering::Less, "{pair:?}");
            assert_eq!(c.compare(pair[1].as_bytes(), pair[0].as_bytes()), Ordering::Greater, "{pair:?}");
            assert!(c.sort_key(pair[0].as_bytes()) < c.sort_key(pair[1].as_bytes()), "sort key {pair:?}");
        }
        let nfc = "Résumé Ångström";
        let nfd: String = nfc.nfd().collect();
        assert_eq!(c.compare(nfc.as_bytes(), nfd.as_bytes()), Ordering::Equal);
        assert_eq!(c.sort_key(nfc.as_bytes()), c.sort_key(nfd.as_bytes()));
        assert_eq!(c.compare(b"a01", b"a1"), Ordering::Equal, "leading zeros are not significant");
        // Invalid UTF-8 compares as U+FFFD instead of panicking.
        assert_eq!(c.compare(b"ab\xff", "ab\u{fffd}".as_bytes()), Ordering::Equal);
    }

    fn random_ascii(rng: &mut Rng) -> Vec<u8> {
        const ALPHABET: &[u8] = b"aAbBzZ0019 _-.~$!\x01\x7f\t";
        (0..rng.below(8))
            .map(|_| if rng.below(6) == 0 { rng.below(128) as u8 } else { ALPHABET[rng.below(ALPHABET.len())] })
            .collect()
    }

    #[test]
    fn ascii_fast_path_agrees_with_icu() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        for locale in ["und", "en-US", "de", "fr", "sv", "lt", "ja", "da", "cs"] {
            let fast = Collator::for_locale(locale);
            if matches!(locale, "und" | "en-US" | "de" | "fr" | "ja") {
                assert!(fast.has_ascii_fast_path(), "{locale} lost its fast path");
            }
            for _ in 0..20_000 {
                let (a, b) = (random_ascii(&mut rng), random_ascii(&mut rng));
                assert_eq!(fast.compare(&a, &b), fast.icu.compare_utf8(&a, &b), "{locale}: {a:?} vs {b:?}");
            }
        }
        assert!(!Collator::for_locale("cs").has_ascii_fast_path(), "Czech 'ch' contraction not detected");
        assert!(!Collator::for_locale("da").has_ascii_fast_path(), "Danish 'aa' contraction not detected");
    }

    #[test]
    fn sort_keys_and_sort_by_name_agree_with_compare() {
        let mut rng = Rng(99);
        let pieces = ["file", "File", "é", "e\u{301}", "10", "2", "日本", " ", "ß", "SS", "Z"];
        let names: Vec<Vec<u8>> = (0..60_000)
            .map(|_| (0..1 + rng.below(4)).flat_map(|_| pieces[rng.below(pieces.len())].bytes()).collect())
            .collect();
        let c = root();
        for pair in names.chunks(2).take(5_000) {
            assert_eq!(c.compare(&pair[0], &pair[1]), c.sort_key(&pair[0]).cmp(&c.sort_key(&pair[1])));
        }
        let mut sorted: Vec<(usize, &[u8])> = names.iter().map(Vec::as_slice).enumerate().collect();
        c.sort_by_name(&mut sorted, |(_, n)| n);
        for w in sorted.windows(2) {
            let order = c.compare(w[0].1, w[1].1);
            assert!(order == Ordering::Less || (order == Ordering::Equal && w[0].0 < w[1].0), "unstable or unsorted");
        }
    }
}
