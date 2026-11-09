//! Filename search: the disposable trigram accelerator over folded names, its process-wide
//! cache, and result-ordering helpers.
//!
//! Ported from `CompactNameIndex.swift` and the accelerator management in `DiskIndexStore.swift`
//! (see TECHNICAL-NOTES "Compact filename search and mapping writes").
//!
//! The database owns the records; a [`NameIndex`] only narrows which rows can match. Every name
//! it holds and every needle it is queried with must come from [`crate::fold`]
//! ([`crate::fold::fold`] for names, [`crate::fold::fold_query`] for queries). Matching is a byte
//! substring test on the folded forms, so punctuation, `%`, `_` and embedded NULs are literal.
//!
//! Staged maps keep growing while they are searched. Row IDs are monotonic, so an index built from
//! a prefix covers exactly the rows `id <= high_water()`; the reader searches the remaining
//! `id > high_water()` tail itself (SQL `instr`, or [`name_matches`] over in-memory rows) and adds
//! both counts. The same rule covers an index that hit its byte budget ([`NameIndex::is_complete`]
//! is false): it keeps its useful prefix and everything after it is tail.

mod cache;
mod index;
mod order;

pub use cache::{BuildPermit, CACHE_BYTE_LIMIT, PreparedIndex, PreparedIndexes, PreparedKey, preparation_due};
pub use index::{BLOCK_RECORDS, BUILDER_BYTE_LIMIT, CANDIDATE_LIMIT, Matches, NameIndex, QueryOptions, name_matches};
pub use order::{DEFAULT_RESULT_LIMIT, MAX_RESULT_LIMIT, SizeIdKey, SizePathKey, TopK, clamp_limit, size_id_key};

#[cfg(test)]
mod tests;
