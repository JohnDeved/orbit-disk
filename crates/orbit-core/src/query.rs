//! Filename search over an inventory database (a saved map or a mapping stage). Ported from
//! `DiskIndexStore.search` / `stagedSearch` / `projectedSearch`.
//!
//! - With a prepared [`NameIndex`] (see [`crate::search`]) the index narrows the rows; rows with
//!   `id > high_water` (a staged map that kept growing, or an index that hit its byte budget) are
//!   the *tail* and are matched in SQL with `instr`. Counts are `index + tail`.
//! - When the index reports too many candidates (or there is none), SQL scans
//!   `nodes_by_size` (`size DESC, id, search_name, parent`) in result order and stops at the limit;
//!   the count is a separate `COUNT(*)`.
//! - Removals are projected in Rust rather than with Swift's TEMP tables: the matching rows are
//!   streamed once with their parent paths, hidden rows are dropped and ancestors' totals
//!   reduced, and a top-k keeps the limit. One pass over the matches gives the exact count.
//!
//! Results are ordered largest first, then by row id (the database's insertion order).

use crate::fold::fold_query;
use crate::live::SearchHit;
use crate::schema;
use crate::search::{Matches, NameIndex, PreparedIndex, QueryOptions, TopK, clamp_limit, size_id_key};
use crate::store::{Projection, Reader, Row, columns, read_row};
use crate::{CoreError, Result, ScanRemoval};
use orbit_platform::Cancellation;
use rusqlite::{Connection, OptionalExtension, params};

/// Matches of one query: the first `limit` hits and the exact number of visible matches.
#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub total: usize,
}

fn check(cancel: Option<&Cancellation>) -> Result<()> {
    schema::check(cancel)
}

/// Builds a search accelerator from every non-root row, in id order. Returns the index and
/// whether it is complete (no row was refused for lack of budget).
pub(crate) fn build_index(conn: &Connection, byte_limit: usize, cancel: Option<&Cancellation>) -> Result<NameIndex> {
    let mut index = NameIndex::with_byte_limit(byte_limit);
    let mut statement = conn.prepare("SELECT id,search_name FROM nodes WHERE parent IS NOT NULL ORDER BY id")?;
    let mut rows = statement.query([])?;
    let mut count = 0usize;
    while let Some(row) = rows.next().map_err(|e| schema::map_cancel(e, cancel))? {
        if count & 511 == 0 {
            check(cancel)?;
        }
        count += 1;
        let id: i64 = row.get(0)?;
        let name = match row.get_ref(1)? {
            rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => bytes,
            _ => return Err(CoreError::Invalid("A search name is missing.".into())),
        };
        if !index.push(id, name) {
            break;
        }
    }
    check(cancel)?;
    Ok(index)
}

/// Full path of a row: directories by their own `dirs` row, others by parent path + name.
fn row_path(conn: &Connection, row: &Row, parent: Option<i64>) -> Result<Option<Vec<u8>>> {
    let lookup = |id: i64| -> Result<Option<Vec<u8>>> {
        Ok(conn.prepare_cached("SELECT path FROM dirs WHERE id=?1")?.query_row([id], |r| r.get(0)).optional()?)
    };
    if row.meta.flags.is_dir
        && let Some(path) = lookup(row.id)?
    {
        return Ok(Some(path));
    }
    let Some(parent) = parent else { return Ok(None) };
    Ok(lookup(parent)?.map(|dir| crate::paths::join(&dir, &row.name)))
}

/// Searches `conn` (inside the caller's read transaction) for the folded query `needle`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn search_inventory(
    conn: &Connection,
    root: &[u8],
    needle: &str,
    limit: usize,
    prepared: Option<&PreparedIndex>,
    removals: &[ScanRemoval],
    stage: bool,
    cancel: Option<&Cancellation>,
) -> Result<SearchResults> {
    check(cancel)?;
    if needle.is_empty() {
        return Ok(SearchResults::default());
    }
    let limit = clamp_limit(limit);
    let index = prepared.map(|p| &p.index);
    let matches = match index {
        Some(index) => Some(index.query(needle.as_bytes(), QueryOptions::default(), || check(cancel))?),
        None => None,
    };
    let reader = Reader::new(conn, None, stage, cancel);
    let projection = Projection::new(&reader, root, removals, cancel)?;
    if !projection.is_empty() {
        return projected(conn, root, needle, limit, index, matches.as_ref(), &projection, stage, cancel);
    }
    let high_water = index.map_or(0, NameIndex::high_water);
    let select = format!("SELECT {},n.parent FROM nodes n", columns(stage));
    match (index, &matches) {
        (Some(_), Some(Matches { count, ids: Some(ids) })) => {
            let tail_count: i64 = conn
                .prepare_cached(
                    "SELECT COUNT(*) FROM nodes WHERE id>?1 AND parent IS NOT NULL AND instr(search_name,?2)>0",
                )?
                .query_row(params![high_water, needle], |r| r.get(0))
                .map_err(|e| schema::map_cancel(e, cancel))?;
            let total = count + tail_count.max(0) as usize;
            let mut top = TopK::new(limit);
            let mut by_id = conn.prepare_cached(&format!("{select} WHERE n.id=?1"))?;
            for (i, id) in ids.iter().enumerate() {
                if i & 255 == 0 {
                    check(cancel)?;
                }
                if let Some((row, parent)) = by_id
                    .query_row([id], |r| Ok((read_row(r, stage)?, r.get::<_, Option<i64>>(14)?)))
                    .optional()
                    .map_err(|e| schema::map_cancel(e, cancel))?
                {
                    top.push(size_id_key(row.meta.size, row.id), (row, parent));
                }
            }
            if tail_count > 0 {
                let mut tail = conn.prepare_cached(&format!(
                    "{select} WHERE n.id>?1 AND n.parent IS NOT NULL AND instr(n.search_name,?2)>0"
                ))?;
                let mut rows = tail.query(params![high_water, needle])?;
                while let Some(r) = rows.next().map_err(|e| schema::map_cancel(e, cancel))? {
                    let row = read_row(r, stage)?;
                    let parent = r.get::<_, Option<i64>>(14)?;
                    top.push(size_id_key(row.meta.size, row.id), (row, parent));
                }
            }
            finish(conn, top, total, cancel)
        }
        _ => {
            let total = match &matches {
                // Too many candidates: the index count is exact for its prefix.
                Some(Matches { count, .. }) => {
                    let tail: i64 = conn
                        .prepare_cached(
                            "SELECT COUNT(*) FROM nodes WHERE id>?1 AND parent IS NOT NULL AND instr(search_name,?2)>0",
                        )?
                        .query_row(params![high_water, needle], |r| r.get(0))
                        .map_err(|e| schema::map_cancel(e, cancel))?;
                    count + tail.max(0) as usize
                }
                None => {
                    let count: i64 = conn
                        .prepare_cached(
                            "SELECT COUNT(*) FROM nodes WHERE parent IS NOT NULL AND instr(search_name,?1)>0",
                        )?
                        .query_row([needle], |r| r.get(0))
                        .map_err(|e| schema::map_cancel(e, cancel))?;
                    count.max(0) as usize
                }
            };
            check(cancel)?;
            let mut top = TopK::new(limit);
            if total > 0 && limit > 0 {
                let mut statement = conn.prepare_cached(&format!(
                    "{select} WHERE n.parent IS NOT NULL AND instr(n.search_name,?1)>0 ORDER BY n.size DESC,n.id LIMIT ?2"
                ))?;
                let mut rows = statement.query(params![needle, limit as i64])?;
                while let Some(r) = rows.next().map_err(|e| schema::map_cancel(e, cancel))? {
                    let row = read_row(r, stage)?;
                    let parent = r.get::<_, Option<i64>>(14)?;
                    top.push(size_id_key(row.meta.size, row.id), (row, parent));
                }
            }
            finish(conn, top, total, cancel)
        }
    }
}

fn finish(
    conn: &Connection,
    top: TopK<(std::cmp::Reverse<u64>, i64), (Row, Option<i64>)>,
    total: usize,
    cancel: Option<&Cancellation>,
) -> Result<SearchResults> {
    let mut hits = Vec::new();
    for (_, (row, parent)) in top.into_sorted() {
        check(cancel)?;
        if let Some(path) = row_path(conn, &row, parent)? {
            hits.push(SearchHit { path, meta: row.meta });
        }
    }
    Ok(SearchResults { hits, total })
}

/// The removal-projected search: one pass over every match.
#[allow(clippy::too_many_arguments)]
fn projected(
    conn: &Connection,
    root: &[u8],
    needle: &str,
    limit: usize,
    index: Option<&NameIndex>,
    matches: Option<&Matches>,
    projection: &Projection,
    stage: bool,
    cancel: Option<&Cancellation>,
) -> Result<SearchResults> {
    let select = format!("SELECT {},p.path FROM nodes n JOIN dirs p ON p.id=n.parent", columns(stage));
    let mut top: TopK<(std::cmp::Reverse<u64>, i64), SearchHit> = TopK::new(limit);
    let mut total = 0usize;
    let mut visit = |row: Row, parent: Vec<u8>| {
        let path = crate::paths::join(&parent, &row.name);
        if projection.hides(&path, root) {
            return;
        }
        let mut meta = row.meta;
        if meta.flags.is_dir {
            projection.apply(&path, &mut meta);
        }
        total += 1;
        top.push(size_id_key(meta.size, row.id), SearchHit { path, meta });
    };
    let mut scan = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> Result<()> {
        let mut statement = conn.prepare_cached(sql)?;
        let mut rows = statement.query(params)?;
        let mut seen = 0usize;
        while let Some(r) = rows.next().map_err(|e| schema::map_cancel(e, cancel))? {
            seen += 1;
            if seen & 1023 == 0 {
                check(cancel)?;
            }
            visit(read_row(r, stage)?, r.get(14)?);
        }
        Ok(())
    };
    match (index, matches) {
        (Some(index), Some(Matches { ids: Some(ids), .. })) => {
            for id in ids {
                scan(&format!("{select} WHERE n.id=?1"), params![id])?;
            }
            let high_water = index.high_water();
            scan(&format!("{select} WHERE n.id>?1 AND instr(n.search_name,?2)>0"), params![high_water, needle])?;
        }
        _ => scan(&format!("{select} WHERE instr(n.search_name,?1)>0"), params![needle])?,
    }
    check(cancel)?;
    let hits = top.into_sorted().into_iter().map(|(_, hit)| hit).collect();
    Ok(SearchResults { hits, total })
}

/// Folds a user query (see [`crate::fold::fold_query`]).
pub fn fold(query: &str) -> String {
    fold_query(query)
}
