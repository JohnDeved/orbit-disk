//! The single search-name fold. The index and every query MUST use this function; index/query
//! consistency is what makes search correct (see TECHNICAL-NOTES "Compact filename search").
//!
//! Fold = full Unicode case fold, canonical decomposition, combining marks removed. It is
//! case- and diacritic-insensitive and treats NFC/NFD spellings of a name identically.
//! Names that are not valid UTF-8 are folded lossily (invalid sequences become U+FFFD).

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Appends the folded form of `name` to `out`.
pub fn fold_into(name: &[u8], out: &mut String) {
    if name.is_ascii() {
        out.reserve(name.len());
        out.extend(name.iter().map(|b| b.to_ascii_lowercase() as char));
        return;
    }
    let text = String::from_utf8_lossy(name);
    let folded = caseless::default_case_fold_str(&text);
    out.extend(folded.nfd().filter(|c| !is_combining_mark(*c)));
}

pub fn fold(name: &[u8]) -> String {
    let mut out = String::new();
    fold_into(name, &mut out);
    out
}

/// Folds a user query. Leading/trailing whitespace is not significant.
pub fn fold_query(query: &str) -> String {
    fold(query.trim().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_query_agree_across_case_diacritics_and_normalization() {
        let nfc = "Résumé Ångström.pdf";
        let nfd: String = nfc.nfd().collect();
        assert_eq!(fold(nfc.as_bytes()), fold(nfd.as_bytes()));
        assert!(fold(nfc.as_bytes()).contains(&fold_query(" RESUME angstrom ")));
        assert_eq!(fold("STRASSE".as_bytes()), fold("straße".as_bytes()));
    }

    #[test]
    fn non_utf8_names_fold_without_panicking() {
        assert_eq!(fold(b"ab\xffCD"), "ab\u{fffd}cd");
    }
}
