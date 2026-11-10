//! Stage protocol tests: transaction boundaries, crash points (simulated by dropping the writer
//! mid-way and reopening), epochs/trim/prune with revision and generation rejection, refresh
//! marking, hard-link ownership, mailbox consumption, views across the watermark and
//! publication crash safety. Real-file tests use `orbit_native::Native`.

use super::writer::busy_readers;
use super::*;
use crate::lease::SnapshotExpectation;
use crate::mailbox::{ListingCompletion, Mailbox};
use crate::publish::{BoundaryAction, PublishOptions};
use crate::store::Store;
use orbit_platform::EntryKind;
use std::panic::{AssertUnwindSafe, catch_unwind};

const ROOT: FileIdentity = FileIdentity { dev: 1, ino: 2 };

#[derive(Clone)]
struct Rec {
    name: Vec<u8>,
    kind: EntryKind,
    id: FileIdentity,
    nlink: u32,
    size: u64,
    allocated: u64,
}

fn file(name: &str, ino: u64, size: u64) -> Rec {
    Rec {
        name: name.as_bytes().to_vec(),
        kind: EntryKind::File,
        id: FileIdentity { dev: 1, ino },
        nlink: 1,
        size,
        allocated: size.div_ceil(4096) * 4096,
    }
}

fn link(name: &str, ino: u64, size: u64) -> Rec {
    Rec { nlink: 2, ..file(name, ino, size) }
}

fn dir(name: &str, ino: u64) -> Rec {
    Rec {
        name: name.as_bytes().to_vec(),
        kind: EntryKind::Directory,
        id: FileIdentity { dev: 1, ino },
        nlink: 1,
        size: 0,
        allocated: 0,
    }
}

fn batch(records: &[Rec]) -> RecordBatch {
    let mut batch = RecordBatch::default();
    for r in records {
        batch.push(&EntryRecord {
            name: &r.name,
            kind: r.kind,
            id: r.id,
            nlink: r.nlink,
            size: r.size,
            allocated: r.allocated,
            is_mount_point: false,
        });
    }
    batch
}

fn open_writer(path: &Path) -> Writer {
    let mut writer = Writer::open(path, b"/r", ROOT, Traversal::SameDevice).unwrap();
    writer.preserve_scan_start("history", 7, Some(&SnapshotExpectation::Absent), Some("0")).unwrap();
    writer.register_root().unwrap();
    writer
}

fn completion(revision: i64, generation: i64) -> Completion<'static> {
    Completion { revision, generation, issue_count: 0, issues: &[] }
}

/// Lists `parent` as one complete (non-fresh path) listing: append + finish.
fn relist(w: &mut Writer, parent: &[u8], records: &[Rec]) -> bool {
    let mut cursor = ListingCursor::default();
    let b = batch(records);
    if !records.is_empty() {
        assert_eq!(w.append(parent, &mut cursor, &[&b], None).unwrap(), AppendOutcome::Committed);
    }
    let revision = w.dirty_revision(parent).unwrap();
    let generation = w.full_refresh_state().unwrap().0;
    w.finish(parent, &mut cursor, revision, Some(generation), 0, &[], None).unwrap()
}

fn drain(w: &mut Writer) {
    loop {
        if w.mark_full_refresh_chunk().unwrap()
            || w.prune_one_chunk().unwrap()
            || w.trim_one_chunk().unwrap()
            || w.reconcile_links_chunk(None).unwrap()
        {
            continue;
        }
        break;
    }
}

fn totals(w: &Writer, path: &[u8]) -> (i64, i64, i64) {
    w.folder_totals(path).unwrap().unwrap()
}

fn quick_check(conn: &rusqlite::Connection) {
    let result: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0)).unwrap();
    assert_eq!(result, "ok");
}

/// Recomputes every directory total from file rows and compares with the stored totals.
fn assert_consistent(w: &Writer) {
    let conn = w.connection();
    let mismatches: i64 = conn
        .query_row(
            "WITH RECURSIVE up(file, dir) AS (SELECT id, parent FROM nodes WHERE kind<>1
               UNION ALL SELECT up.file, n.parent FROM up JOIN nodes n ON n.id=up.dir WHERE n.parent IS NOT NULL)
             SELECT COUNT(*) FROM nodes d WHERE d.kind=1 AND (d.size, d.allocated, d.file_count) IS NOT
               (SELECT COALESCE(SUM(f.size),0), COALESCE(SUM(f.allocated),0), COUNT(f.id) FROM up JOIN nodes f ON f.id=up.file WHERE up.dir=d.id)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mismatches, 0, "directory totals diverge from file rows");
    quick_check(conn);
}

// --- Writer protocol --------------------------------------------------------------------------

#[test]
fn fresh_folder_commits_records_queue_and_completion_in_one_transaction() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    let root = batch(&[dir("a", 10), file("f", 11, 100)]);
    assert!(w.complete_fresh_listing(b"/r", &[&root], completion(0, 0), None).unwrap());
    // Children are queued, the folder is complete, totals are exact.
    assert!(w.directory(b"/r/a").unwrap().is_some());
    assert!(!w.can_complete_fresh_listing(b"/r").unwrap());
    assert_eq!(w.next_directory(None).unwrap().unwrap().path, b"/r/a");
    assert_eq!(totals(&w, b"/r"), (100, 4096, 1));
    // A second fresh completion of the same folder is refused without mutation.
    assert!(!w.complete_fresh_listing(b"/r", &[&root], completion(0, 0), None).unwrap());
    assert_eq!(w.count(b"/r").unwrap(), 2);

    // A crash before COMMIT (hook panics inside the transaction) leaves nothing behind.
    w.set_commit_hook(Some(Box::new(|event, _| assert_ne!(event, "commit", "simulated crash"))));
    let leaf = batch(&[file("x", 20, 5)]);
    let crashed =
        catch_unwind(AssertUnwindSafe(|| w.complete_fresh_listing(b"/r/a", &[&leaf], completion(0, 0), None)));
    assert!(crashed.is_err());
    drop(w);
    let w = open_writer(&tmp.path().join("s.sqlite"));
    assert_eq!(w.count(b"/r/a").unwrap(), 0);
    assert!(w.can_complete_fresh_listing(b"/r/a").unwrap());
    assert_consistent(&w);
}

#[test]
fn stale_revision_or_generation_rejects_completion_without_minting_an_epoch() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    assert!(w.complete_fresh_listing(b"/r", &[&batch(&[dir("a", 10)])], completion(0, 0), None).unwrap());
    let epoch = |w: &Writer| -> i64 {
        w.connection().query_row("SELECT epoch FROM dirs WHERE path=?1", [&b"/r/a"[..]], |r| r.get(0)).unwrap()
    };
    // A change arrives while /r/a is being listed: the listing's revision is stale.
    w.mark_dirty(b"/r/a").unwrap();
    let watermark = Watermark { generation: uuid::Uuid::new_v4(), sequence: 9 };
    let mut cursor = ListingCursor::default();
    assert!(!w.finish(b"/r/a", &mut cursor, 0, Some(0), 0, &[], Some(watermark)).unwrap());
    assert_eq!(epoch(&w), 0, "rejected completion must not mint an epoch");
    assert_eq!(w.watermark().unwrap(), Some(watermark), "consumed observations are still acknowledged");
    assert!(w.is_dirty(b"/r/a").unwrap());
    // Fresh completion is rejected for the stale revision too.
    assert!(!w.complete_fresh_listing(b"/r/a", &[&batch(&[file("f", 11, 1)])], completion(0, 0), None).unwrap());
    // A full refresh started: listings of the old generation are rejected until marking ends.
    w.mark_full_refresh().unwrap();
    assert!(!w.complete_fresh_listing(b"/r/a", &[&batch(&[file("f", 11, 1)])], completion(1, 1), None).unwrap());
    while w.mark_full_refresh_chunk().unwrap() {}
    assert!(!w.complete_fresh_listing(b"/r/a", &[&batch(&[file("f", 11, 1)])], completion(1, 0), None).unwrap());
    let mut cursor = ListingCursor::default();
    assert!(w.finish(b"/r/a", &mut cursor, 1, Some(1), 0, &[], None).unwrap());
    drain(&mut w);
    assert!(!w.is_dirty(b"/r/a").unwrap());
    // Only the root still waits for its generation-1 listing.
    assert_eq!(w.next_directory(None).unwrap().unwrap().path, b"/r");
    assert_eq!(epoch(&w), 1);
}

#[test]
fn relisting_trims_stale_files_and_prunes_stale_subtrees() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    let many: Vec<Rec> = (0..300).map(|i| file(&format!("f{i:03}"), 100 + i, 10)).collect();
    let mut root = vec![dir("d", 10), file("keep", 11, 7)];
    root.extend(many.iter().cloned());
    assert!(relist(&mut w, b"/r", &root));
    drain(&mut w);
    assert!(relist(&mut w, b"/r/d", &[dir("e", 12), file("x", 13, 1000)]));
    drain(&mut w);
    assert!(relist(&mut w, b"/r/d/e", &[file("y", 14, 2000)]));
    drain(&mut w);
    assert_eq!(totals(&w, b"/r"), (7 + 3000 + 3000, 4096 * 303, 303));

    // The folder lost 300 files and the whole subtree: trim needs several 128-row chunks.
    w.mark_dirty(b"/r").unwrap();
    assert!(relist(&mut w, b"/r", &[file("keep", 11, 7)]));
    assert!(w.has_trim().unwrap());
    drain(&mut w);
    assert_eq!(totals(&w, b"/r"), (7, 4096, 1));
    assert_eq!(w.count(b"/r").unwrap(), 1);
    assert!(w.directory(b"/r/d").unwrap().is_none() && w.directory(b"/r/d/e").unwrap().is_none());
    assert!(!w.has_remaining_work().unwrap());
    assert_consistent(&w);
}

/// Counts every insert, update and delete of `nodes` from now on (read with [`node_writes`]).
fn count_node_writes(w: &Writer) {
    w.connection()
        .execute_batch(
            "CREATE TEMP TABLE node_writes(n INTEGER); INSERT INTO node_writes VALUES(0);
             CREATE TEMP TRIGGER node_insert AFTER INSERT ON main.nodes BEGIN UPDATE node_writes SET n=n+1; END;
             CREATE TEMP TRIGGER node_update AFTER UPDATE ON main.nodes BEGIN UPDATE node_writes SET n=n+1; END;
             CREATE TEMP TRIGGER node_delete AFTER DELETE ON main.nodes BEGIN UPDATE node_writes SET n=n+1; END;",
        )
        .unwrap();
}

fn node_writes(w: &Writer) -> i64 {
    w.connection().query_row("SELECT n FROM node_writes", [], |row| row.get(0)).unwrap()
}

/// Rescanning unchanged data writes nothing: every relisted row matches and stays as it is, and
/// a relisting that saw all its rows needs no trim.
#[test]
fn an_unchanged_relisting_writes_no_rows_and_needs_no_trim() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    let root = vec![dir("a", 10), file("f", 11, 1000), file("g", 12, 2000)];
    assert!(relist(&mut w, b"/r", &root));
    assert!(relist(&mut w, b"/r/a", &[file("x", 20, 300)]));
    drain(&mut w);
    let before = totals(&w, b"/r");
    w.mark_dirty(b"/r").unwrap();
    w.mark_dirty(b"/r/a").unwrap();
    count_node_writes(&w);
    assert!(relist(&mut w, b"/r", &root));
    assert!(relist(&mut w, b"/r/a", &[file("x", 20, 300)]));
    assert_eq!(node_writes(&w), 0, "an unchanged relisting rewrote rows");
    assert!(!w.has_trim().unwrap(), "nothing is stale");
    assert!(!w.is_dirty(b"/r").unwrap() && !w.is_dirty(b"/r/a").unwrap());
    assert_eq!(totals(&w, b"/r"), before);
    assert_consistent(&w);
}

/// Only what changed is written; rows left untouched survive the trim of the stale ones.
#[test]
fn a_changed_relisting_writes_changes_and_keeps_untouched_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    let kept: Vec<Rec> = (0..20).map(|i| file(&format!("keep{i}"), 100 + i, 10)).collect();
    let mut root = kept.clone();
    root.push(file("grow", 200, 1000));
    root.push(file("gone", 201, 5000));
    assert!(relist(&mut w, b"/r", &root));
    drain(&mut w);
    w.mark_dirty(b"/r").unwrap();
    count_node_writes(&w);
    let mut now = kept.clone();
    now.push(file("grow", 200, 3000));
    now.push(file("new", 202, 7));
    assert!(relist(&mut w, b"/r", &now));
    assert!(w.has_trim().unwrap(), "\"gone\" is stale");
    drain(&mut w);
    // grow (update) + new (insert) + 20 untouched rows stamped for the trim + gone (delete), and
    // the folder's own totals once for the listing and once for the trim.
    assert_eq!(node_writes(&w), 1 + 1 + 20 + 1 + 2);
    assert_eq!(w.count(b"/r").unwrap(), 22);
    let files: i64 = 20 + 2;
    assert_eq!(totals(&w, b"/r").2, files);
    assert_eq!(totals(&w, b"/r").0, 20 * 10 + 3000 + 7);
    assert_consistent(&w);
}

#[test]
fn trim_without_stale_rows_acknowledges_and_guards_hold() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    assert!(relist(&mut w, b"/r", &[dir("a", 10), file("f", 11, 1)]));
    drain(&mut w);
    w.mark_dirty(b"/r").unwrap();
    // A relisting that saw every row has nothing to trim: it is acknowledged without a marker.
    assert!(relist(&mut w, b"/r", &[dir("a", 10), file("f", 11, 1)]));
    assert!(!w.has_trim().unwrap());
    assert!(!w.is_dirty(b"/r").unwrap());
    // A trim marker without stale rows is consumed in one empty transaction.
    assert!(relist(&mut w, b"/r/a", &[file("old", 12, 1)]));
    drain(&mut w);
    w.mark_dirty(b"/r/a").unwrap();
    let mut cursor = ListingCursor::default();
    assert!(w.finish(b"/r/a", &mut cursor, 1, Some(0), 0, &[], None).unwrap());
    assert!(w.has_trim().unwrap(), "the unseen row is stale");
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    w.set_commit_hook(Some(Box::new(move |event, _| seen.lock().push(event.to_string()))));
    while w.trim_one_chunk().unwrap() {}
    assert!(events.lock().iter().any(|e| e == "trim-empty-committed"));
    assert!(!w.is_dirty(b"/r/a").unwrap());
    // A newer revision keeps the directory pending even though its old trim ran.
    assert!(relist(&mut w, b"/r/a", &[file("old", 12, 1)]));
    drain(&mut w);
    w.mark_dirty(b"/r/a").unwrap();
    let revision = w.dirty_revision(b"/r/a").unwrap();
    let mut cursor = ListingCursor::default();
    assert!(w.finish(b"/r/a", &mut cursor, revision, Some(0), 0, &[], None).unwrap());
    w.mark_dirty(b"/r/a").unwrap();
    assert!(w.trim_one_chunk().unwrap());
    assert!(w.is_dirty(b"/r/a").unwrap());
    assert!(w.has_pending_directories().unwrap());
    assert!(!w.has_trim().unwrap());
}

#[test]
fn replaced_directory_requires_prune_before_new_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    assert!(relist(&mut w, b"/r", &[dir("a", 10)]));
    assert!(relist(&mut w, b"/r/a", &[file("x", 20, 4096)]));
    drain(&mut w);
    // /r/a was replaced by another directory: the batch is refused and a prune is queued.
    let mut cursor = ListingCursor::default();
    let replaced = batch(&[dir("a", 99), file("g", 21, 1)]);
    assert_eq!(w.append(b"/r", &mut cursor, &[&replaced], None).unwrap(), AppendOutcome::PruneRequired);
    assert!(cursor.epoch.is_none(), "no epoch for a refused batch");
    assert_eq!(w.count(b"/r").unwrap(), 1);
    assert!(w.has_prune().unwrap());
    drain(&mut w);
    assert!(w.directory(b"/r/a").unwrap().is_none());
    // A directory that became a file is refused the same way.
    assert!(relist(&mut w, b"/r", &[dir("a", 99)]));
    let mut cursor = ListingCursor::default();
    assert_eq!(
        w.append(b"/r", &mut cursor, &[&batch(&[file("a", 99, 1)])], None).unwrap(),
        AppendOutcome::PruneRequired
    );
    drain(&mut w);
    w.mark_dirty(b"/r").unwrap();
    assert!(relist(&mut w, b"/r", &[file("a", 99, 1)]));
    drain(&mut w);
    assert_eq!(totals(&w, b"/r"), (1, 4096, 1));
    assert_consistent(&w);
}

#[test]
fn interrupted_prune_resumes_after_reopen_with_paths_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.sqlite");
    let mut w = open_writer(&path);
    assert!(relist(&mut w, b"/r", &[dir("p", 10), dir("p-x", 11), file("z", 12, 1)]));
    let wide: Vec<Rec> = (0..200).map(|i| file(&format!("w{i}"), 1000 + i, 1)).collect();
    let mut p = vec![dir("q", 13)];
    p.extend(wide);
    assert!(relist(&mut w, b"/r/p", &p));
    assert!(relist(&mut w, b"/r/p/q", &(0..150).map(|i| file(&format!("v{i}"), 2000 + i, 1)).collect::<Vec<_>>()));
    assert!(relist(&mut w, b"/r/p-x", &[file("sibling", 30, 5)]));
    drain(&mut w);
    w.invalidate_directory(b"/r/p").unwrap();
    // Two chunks, then a crash (drop) and reopen.
    assert!(w.prune_one_chunk().unwrap());
    assert!(w.prune_one_chunk().unwrap());
    // Deepest parent first: /r/p/q lost rows; /r/p still keeps its path for survivors.
    assert!(w.directory(b"/r/p").unwrap().is_some());
    drop(w);
    let mut w = open_writer(&path);
    assert_consistent(&w);
    drain(&mut w);
    assert!(w.directory(b"/r/p").unwrap().is_none());
    assert_eq!(w.count(b"/r/p-x").unwrap(), 1, "the sibling with a byte-prefix name is untouched");
    assert!(w.is_dirty(b"/r").unwrap());
    assert!(relist(&mut w, b"/r", &[dir("p-x", 11), file("z", 12, 1)]));
    drain(&mut w);
    assert_eq!(totals(&w, b"/r"), (6, 8192, 2));
    assert_consistent(&w);
}

#[test]
fn hard_links_charge_the_first_path_and_move_on_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    assert!(relist(&mut w, b"/r", &[dir("b", 10), dir("B", 11), dir("a-x", 12)]));
    // Same identity (ino 50) three times: "/r/B/l" sorts first (component "B" < "a-x" < "b").
    assert!(relist(&mut w, b"/r/b", &[link("l", 50, 8192)]));
    assert!(relist(&mut w, b"/r/a-x", &[link("l", 50, 8192)]));
    drain(&mut w);
    assert_eq!(w.charge(b"/r/a-x/l").unwrap(), Some(8192));
    assert_eq!(w.charge(b"/r/b/l").unwrap(), Some(0));
    assert!(relist(&mut w, b"/r/B", &[link("l", 50, 8192)]));
    drain(&mut w);
    assert_eq!(w.charge(b"/r/B/l").unwrap(), Some(8192));
    assert_eq!(w.charge(b"/r/a-x/l").unwrap(), Some(0));
    assert_eq!(totals(&w, b"/r"), (3 * 8192, 8192, 3));
    // Removing the owner moves the charge to the next path.
    w.mark_dirty(b"/r/B").unwrap();
    assert!(relist(&mut w, b"/r/B", &[]));
    drain(&mut w);
    assert_eq!(w.charge(b"/r/a-x/l").unwrap(), Some(8192));
    assert_eq!(totals(&w, b"/r"), (2 * 8192, 8192, 2));
    // A single-link file whose identity is already indexed elsewhere is charged once.
    w.mark_dirty(b"/r/B").unwrap();
    assert!(relist(&mut w, b"/r/B", &[file("dup", 50, 8192)]));
    drain(&mut w);
    assert_eq!(totals(&w, b"/r").1, 8192);
    assert_eq!(w.charge(b"/r/B/dup").unwrap(), Some(8192));
    assert_consistent(&w);
}

#[test]
fn fresh_leaf_groups_commit_a_timed_prefix_or_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.sqlite");
    let mut w = open_writer(&path);
    let names = ["a", "b", "c", "d"];
    let root: Vec<Rec> = names.iter().enumerate().map(|(i, n)| dir(n, 10 + i as u64)).collect();
    assert!(w.complete_fresh_listing(b"/r", &[&batch(&root)], completion(0, 0), None).unwrap());
    let generation = uuid::Uuid::new_v4();
    let leaves: Vec<(Vec<u8>, RecordBatch)> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (format!("/r/{n}").into_bytes(), batch(&[file("f", 100 + i as u64, 10), link("l", 500, 10)])))
        .collect();
    let group = |range: std::ops::Range<usize>, first_sequence: u64| -> Vec<FreshLeaf<'_>> {
        leaves[range]
            .iter()
            .enumerate()
            .map(|(i, (path, records))| FreshLeaf {
                path,
                identity: FileIdentity {
                    dev: 1,
                    ino: 10 + names.iter().position(|n| path.ends_with(n.as_bytes())).unwrap() as u64,
                },
                records,
                expected_revision: 0,
                expected_generation: 0,
                issue_count: 0,
                issues: &[],
                watermark: Watermark { generation, sequence: first_sequence + i as u64 },
            })
            .collect()
    };
    let cancel = Cancellation::new();
    // A sequence gap or a wrong identity rejects the whole group without writing.
    let mut gap = group(0..2, 1);
    gap[1].watermark.sequence = 5;
    assert_eq!(w.complete_fresh_leaf_listings(&gap, &cancel).unwrap(), 0);
    let mut wrong = group(0..2, 1);
    wrong[1].identity.ino = 999;
    assert_eq!(w.complete_fresh_leaf_listings(&wrong, &cancel).unwrap(), 0);
    assert_eq!(w.count(b"/r/a").unwrap(), 0);
    // Cancellation rolls back too.
    let cancelled = Cancellation::new();
    cancelled.cancel();
    assert!(w.complete_fresh_leaf_listings(&group(0..2, 1), &cancelled).unwrap_err().is_cancelled());
    assert_eq!(w.watermark().unwrap(), None);
    // The budget runs out after two folders (injected clock): a prefix commits with its watermark.
    let ticks = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let base = std::time::Instant::now();
    let clock = ticks.clone();
    w.set_clock(Arc::new(move || {
        base + std::time::Duration::from_millis(3 * clock.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }));
    assert_eq!(w.complete_fresh_leaf_listings(&group(0..3, 1), &cancel).unwrap(), 2);
    assert_eq!(w.watermark().unwrap().unwrap().sequence, 2);
    assert!(w.can_complete_fresh_listing(b"/r/c").unwrap());
    // A crash after the group is written but before COMMIT keeps the previous state.
    w.set_commit_hook(Some(Box::new(|event, _| assert_ne!(event, "fresh-group-written", "simulated crash"))));
    let crashed = catch_unwind(AssertUnwindSafe(|| w.complete_fresh_leaf_listings(&group(2..4, 3), &cancel)));
    assert!(crashed.is_err());
    drop(w);
    let mut w = open_writer(&path);
    assert_eq!(w.watermark().unwrap().unwrap().sequence, 2);
    assert_eq!(w.count(b"/r/c").unwrap(), 0);
    assert_eq!(w.complete_fresh_leaf_listings(&group(2..4, 3), &cancel).unwrap(), 2);
    drain(&mut w);
    // Four folders, each with one file and one link of identity 500: one link is charged.
    assert_eq!(totals(&w, b"/r"), (80, 4 * 4096 + 4096, 8));
    assert_eq!(w.charge(b"/r/a/l").unwrap(), Some(4096));
    assert!(!w.has_remaining_work().unwrap());
    assert_consistent(&w);
}

#[test]
fn writer_reads_are_reset_before_every_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    let violations = Arc::new(Mutex::new(Vec::new()));
    let seen = violations.clone();
    w.set_commit_hook(Some(Box::new(move |event, conn| {
        if event == "commit" {
            seen.lock().extend(busy_readers(conn));
        }
    })));
    let wide: Vec<Rec> = (0..400).map(|i| file(&format!("n{i}"), 100 + i, 1)).collect();
    let mut root = vec![dir("a", 10), dir("b", 11), link("l", 77, 9)];
    root.extend(wide);
    assert!(relist(&mut w, b"/r", &root));
    assert!(w.complete_fresh_listing(b"/r/a", &[&batch(&[link("l", 77, 9)])], completion(0, 0), None).unwrap());
    drain(&mut w);
    w.mark_dirty(b"/r").unwrap();
    assert!(relist(&mut w, b"/r", &[dir("a", 10)]));
    w.mark_full_refresh().unwrap();
    drain(&mut w);
    assert!(violations.lock().is_empty(), "readers left open at COMMIT: {:?}", violations.lock());
}

#[test]
fn full_refresh_marking_resumes_and_restarts_per_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.sqlite");
    let mut w = open_writer(&path);
    let dirs: Vec<Rec> = (0..300).map(|i| dir(&format!("d{i:03}"), 100 + i)).collect();
    assert!(relist(&mut w, b"/r", &dirs));
    w.connection().execute("UPDATE dirs SET pending=0", []).unwrap();
    w.mark_full_refresh().unwrap();
    assert!(w.mark_full_refresh_chunk().unwrap());
    let marked = |w: &Writer, generation: i64| -> i64 {
        w.connection()
            .query_row("SELECT COUNT(*) FROM dirs WHERE pending=1 AND full_generation=?1", [generation], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(marked(&w, 1), 128);
    // A failed chunk rolls back without advancing.
    w.set_commit_hook(Some(Box::new(|event, _| assert_ne!(event, "commit", "simulated crash"))));
    assert!(catch_unwind(AssertUnwindSafe(|| w.mark_full_refresh_chunk())).is_err());
    drop(w);
    let mut w = open_writer(&path);
    assert_eq!(marked(&w, 1), 128);
    // After reopening, marking resumes from the committed prefix and covers every directory.
    while w.mark_full_refresh_chunk().unwrap() {}
    assert_eq!(marked(&w, 1), 301);
    assert_eq!(w.full_refresh_state().unwrap(), (1, false));
    // A newer generation starts from the beginning, not from the old cursor.
    w.connection().execute("UPDATE dirs SET pending=0", []).unwrap();
    w.mark_full_refresh().unwrap();
    while w.mark_full_refresh_chunk().unwrap() {}
    assert_eq!(marked(&w, 2), 301);
}

#[test]
fn raw_names_large_values_and_mount_issues_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = open_writer(&tmp.path().join("s.sqlite"));
    let nfc = "é".as_bytes().to_vec();
    let nfd = "e\u{301}".as_bytes().to_vec();
    let names_in: Vec<Vec<u8>> = vec![nfc.clone(), nfd.clone(), b"\xff\xfe".to_vec(), b"A".to_vec(), b"a".to_vec()];
    let mut records: Vec<Rec> =
        names_in.iter().enumerate().map(|(i, n)| Rec { name: n.clone(), ..file("x", 10 + i as u64, 1) }).collect();
    let huge = (1u64 << 53) + 1;
    records.push(Rec { id: FileIdentity { dev: 1, ino: u64::MAX }, size: huge, allocated: huge, ..file("big", 0, 0) });
    // A directory on another device is shown but not traversed.
    records.push(Rec { id: FileIdentity { dev: u64::MAX - 1, ino: 5 }, ..dir("vol", 0) });
    assert!(relist(&mut w, b"/r", &records));
    drain(&mut w);
    assert_eq!(w.count(b"/r").unwrap(), 7);
    let total = totals(&w, b"/r");
    assert_eq!(total.0 as u64, 5 + huge);
    let (search, stored): (String, Vec<u8>) = w
        .connection()
        .query_row("SELECT search_name,name FROM nodes WHERE name=?1", [&nfd], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((search.as_str(), stored), ("e", nfd.clone()));
    let (dev, inode): (i64, i64) = w
        .connection()
        .query_row("SELECT dev,inode FROM nodes WHERE name=CAST('big' AS BLOB)", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(schema::identity_from_sql(dev, inode).ino, u64::MAX);
    assert!(w.directory(b"/r/vol").unwrap().is_none());
    let issue: String = w.connection().query_row("SELECT message FROM stage_issues", [], |r| r.get(0)).unwrap();
    assert!(issue.contains("nested volume skipped"));
    // Once the folder is on the root device again, the issue clears and it is queued.
    w.mark_dirty(b"/r").unwrap();
    records.pop();
    records.push(dir("vol", 6));
    assert!(relist(&mut w, b"/r", &records));
    let issues: i64 = w.connection().query_row("SELECT COUNT(*) FROM stage_issues", [], |r| r.get(0)).unwrap();
    assert_eq!(issues, 0);
    assert!(w.directory(b"/r/vol").unwrap().is_some());
    assert_consistent(&w);
    // Views return the stored bytes unchanged, including invalid UTF-8.
    let reader = crate::store::Reader::new(w.connection(), None, true, None);
    let projection = crate::store::Projection::default();
    let tree = build_tree(&reader, b"/r", b"/r", &projection, ViewSource::Stage, 0, None).unwrap();
    let mut names: Vec<Vec<u8>> = tree.children(FolderTree::FOLDER).iter().map(|ix| tree.name(*ix).to_vec()).collect();
    names.sort();
    let mut expected = names_in;
    expected.extend([b"big".to_vec(), b"vol".to_vec()]);
    expected.sort();
    assert_eq!(names, expected);
    let raw = tree.children(FolderTree::FOLDER).iter().copied().find(|ix| tree.name(*ix) == b"\xff\xfe").unwrap();
    assert_eq!(tree.path(raw), b"/r/\xff\xfe");
}

#[test]
fn queue_index_creation_is_cancellable_and_retryable() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.sqlite");
    let mut w = open_writer(&path);
    assert!(relist(&mut w, b"/r", &(0..50).map(|i| dir(&format!("d{i}"), 100 + i)).collect::<Vec<_>>()));
    let cancel = Cancellation::new();
    let token = cancel.clone();
    w.set_commit_hook(Some(Box::new(move |event, _| {
        if event == "queue-index-first" {
            token.cancel();
        }
    })));
    assert!(w.prepare_queue_indexes(&cancel).unwrap_err().is_cancelled());
    let indexes: i64 = w
        .connection()
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('pending_dirs','pending_dirty')", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(indexes, 0, "a cancelled migration commits nothing");
    w.set_commit_hook(None);
    w.mark_dirty(b"/r/d7").unwrap();
    w.prepare_queue_indexes(&Cancellation::new()).unwrap();
    assert_eq!(w.next_directory(None).unwrap().unwrap().path, b"/r/d7", "dirty directories keep priority");
}

// --- Real-file stages ---------------------------------------------------------------------------

type Native = orbit_native::Native;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    paths: IndexPaths,
    platform: Arc<Native>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("root");
        std::fs::create_dir(&root).unwrap();
        let paths = IndexPaths::under(&base);
        Self { _dir: tmp, root, paths, platform: Arc::new(Native::default()) }
    }

    fn root_bytes(&self) -> Vec<u8> {
        self.root.as_os_str().as_bytes().to_vec()
    }

    fn checkpoint(&self, event_id: u64) -> StageCheckpoint {
        StageCheckpoint { root_path: self.root_bytes(), identity: "history".into(), event_id }
    }

    fn try_open(&self, event_id: u64) -> Result<Opened<Native>> {
        Stage::open(
            self.platform.clone(),
            self.paths.clone(),
            &self.root_bytes(),
            self.checkpoint(event_id),
            Cancellation::new(),
            StageConfig::default(),
        )
    }

    fn open(&self) -> Box<Stage<Native>> {
        match self.try_open(1).unwrap() {
            Opened::Stage(stage) => stage,
            other => panic!("expected a stage, got {other:?}"),
        }
    }

    fn write(&self, relative: &str, bytes: usize) {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7u8; bytes]).unwrap();
    }

    fn identity(&self, relative: &str) -> FileIdentity {
        self.platform.open_root(&self.root.join(relative)).unwrap().1
    }

    fn store(&self) -> Store {
        Store::new(self.paths.clone())
    }

    fn orphans(&self) -> usize {
        std::fs::read_dir(&self.paths.cache)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(".orbit-stage-"))
            .count()
    }
}

fn run(stage: &mut Stage<Native>, mailbox: Option<&Mailbox>) {
    for _ in 0..100_000 {
        if stage.step(mailbox).unwrap() == StepOutcome::Done {
            return;
        }
    }
    panic!("stage did not finish");
}

/// Expected `(logical, allocated, files)` of the real tree under `root`, deduplicating links.
fn scan_totals(root: &Path) -> (u64, u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let mut seen = HashSet::new();
    let (mut logical, mut allocated, mut files) = (0, 0, 0);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.path().symlink_metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
                continue;
            }
            files += 1;
            logical += meta.len();
            if seen.insert((meta.dev(), meta.ino())) {
                allocated += meta.blocks() * 512;
            }
        }
    }
    (logical, allocated, files)
}

fn saved_totals(store: &Store, root: &[u8]) -> (u64, u64, u64) {
    let snapshot = store.saved_snapshot(root).expect("saved map");
    let tree = store.folder(&snapshot, root, &[], None).unwrap();
    let meta = tree.node(FolderTree::FOLDER).meta;
    (meta.size, meta.allocated, meta.file_count)
}

fn build_tree_on_disk(f: &Fixture) {
    f.write("a/one.txt", 5000);
    f.write("a/deep/er/x.bin", 9000);
    f.write("Résumé/é.txt", 10);
    f.write("b/two.txt", 1);
    std::fs::create_dir_all(f.root.join("empty")).unwrap();
    std::fs::hard_link(f.root.join("a/one.txt"), f.root.join("b/link.txt")).unwrap();
    std::os::unix::fs::symlink("a/one.txt", f.root.join("sym")).unwrap();
    for i in 0..300 {
        f.write(&format!("wide/f{i}"), i);
    }
}

#[test]
fn local_pass_maps_real_files_and_publishes_exact_totals() {
    let f = Fixture::new();
    build_tree_on_disk(&f);
    let mut stage = f.open();
    assert!(!stage.is_resumed());
    run(&mut stage, None);
    let snapshot = stage.publish(1.5, PublishOptions::default()).unwrap();
    assert_eq!(saved_totals(&f.store(), &f.root_bytes()), scan_totals(&f.root));
    let conn = schema::open_reader(&snapshot.database, None, true).unwrap();
    quick_check(&conn);
    // A second session is refused while the first holds the stage.
    let second = f.try_open(1);
    assert!(matches!(second, Err(CoreError::InUse(_))), "{second:?}");
    drop(stage);
    // Afterwards the published stage is recognised and can be retired.
    match f.try_open(1).unwrap() {
        Opened::AlreadyPublished(id) => {
            assert_eq!(id, snapshot.id);
            Stage::<Native>::retire_published(&f.paths, &f.root_bytes(), f.identity(""), &id, &Cancellation::new())
                .unwrap();
        }
        other => panic!("expected AlreadyPublished, got {other:?}"),
    }
    assert!(!crate::lease::has_stage(&f.paths.staging, &f.root_bytes(), f.identity("")));
}

#[test]
fn interrupted_stage_resumes_with_its_original_checkpoint() {
    let f = Fixture::new();
    build_tree_on_disk(&f);
    let mut stage = f.open();
    for _ in 0..4 {
        stage.step(None).unwrap();
    }
    let reader = stage.reader();
    let partial = reader.view(&f.root_bytes(), &[], None, 1, None).unwrap();
    assert_eq!(partial.source, ViewSource::Stage);
    drop(stage); // crash between commit points
    // A later open with a newer checkpoint keeps the stage's original scan-start event.
    let mut stage = match f.try_open(99).unwrap() {
        Opened::Stage(stage) => stage,
        other => panic!("{other:?}"),
    };
    assert!(stage.is_resumed());
    assert_eq!(stage.checkpoint().event_id, 1);
    run(&mut stage, None);
    stage.publish(0.0, PublishOptions::default()).unwrap();
    assert_eq!(saved_totals(&f.store(), &f.root_bytes()), scan_totals(&f.root));
}

#[test]
fn unreadable_and_vanished_folders_are_blocked_or_requeued() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.write("locked/secret", 100);
    f.write("gone/file", 100);
    f.write("kept/file", 100);
    let mut stage = f.open();
    stage.step(None).unwrap(); // lists the root
    std::fs::set_permissions(f.root.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::remove_dir_all(f.root.join("gone")).unwrap();
    run(&mut stage, None);
    std::fs::set_permissions(f.root.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let tree = stage.reader().view(&f.root_bytes(), &[], None, 1, None).unwrap();
    let names: Vec<&[u8]> = tree.children(FolderTree::FOLDER).iter().map(|ix| tree.name(*ix)).collect();
    assert!(!names.contains(&&b"gone"[..]), "a vanished folder is pruned and its parent relisted");
    let locked = tree.children(FolderTree::FOLDER).iter().find(|ix| tree.name(**ix) == b"locked").unwrap();
    assert!(!tree.node(*locked).meta.flags.accessible);
    let snapshot = stage.publish(0.0, PublishOptions::default()).unwrap();
    assert_eq!(snapshot.skipped, 1);
    assert_eq!(tree.node(FolderTree::FOLDER).meta.file_count, 1);
}

fn mailbox_for(f: &Fixture) -> Arc<Mailbox> {
    Mailbox::new(Arc::from(f.root_bytes()), f.identity(""), Cancellation::new(), || {}, |_| {})
}

/// A discovery batch for the real directory `relative` (components carry real identities).
fn listing(f: &Fixture, relative: &str, records: &[Rec], complete: bool) -> ListingBatch {
    let mut components = Vec::new();
    let mut walked = String::new();
    for part in relative.split('/').filter(|p| !p.is_empty()) {
        if !walked.is_empty() {
            walked.push('/');
        }
        walked.push_str(part);
        components.push(DirComponent { name: part.as_bytes().into(), identity: f.identity(&walked) });
    }
    let path =
        if relative.is_empty() { f.root_bytes() } else { crate::paths::join(&f.root_bytes(), relative.as_bytes()) };
    ListingBatch::new(
        Arc::from(path),
        f.identity(relative),
        Arc::from(components),
        batch(records),
        complete.then(ListingCompletion::default),
        None,
    )
}

/// Records of a real directory with the given synthetic files (real identities for folders).
fn with_dirs(f: &Fixture, dirs: &[&str], files: &[Rec]) -> Vec<Rec> {
    let mut records: Vec<Rec> =
        dirs.iter().map(|d| Rec { id: f.identity(d), ..dir(d.rsplit('/').next().unwrap(), 0) }).collect();
    records.extend(files.iter().map(|r| Rec { id: FileIdentity { dev: f.identity("").dev, ..r.id }, ..r.clone() }));
    records
}

#[test]
fn mailbox_batches_are_committed_with_their_watermark() {
    let f = Fixture::new();
    for d in ["a", "b", "c", "wide"] {
        std::fs::create_dir(f.root.join(d)).unwrap();
    }
    let mut stage = f.open();
    // A frozen clock keeps the grouped-leaf budget from splitting the group under load.
    let frozen = std::time::Instant::now();
    stage.writer().set_clock(Arc::new(move || frozen));
    let mailbox = mailbox_for(&f);
    let root = with_dirs(&f, &["a", "b", "c", "wide"], &[file("top", 1, 10)]);
    mailbox.enqueue(listing(&f, "", &root, true)).unwrap();
    for (i, d) in ["a", "b", "c"].iter().enumerate() {
        let records = with_dirs(&f, &[], &[file("f", 100 + i as u64, 1000), link("l", 900, 4096)]);
        mailbox.enqueue(listing(&f, d, &records, true)).unwrap();
    }
    let wide: Vec<Rec> =
        with_dirs(&f, &[], &(0..1500).map(|i| file(&format!("w{i}"), 2000 + i, 1)).collect::<Vec<_>>());
    for (index, chunk) in wide.chunks(128).enumerate() {
        mailbox.enqueue(listing(&f, "wide", chunk, index == wide.len().div_ceil(128) - 1)).unwrap();
    }
    mailbox.finish();
    let mut outcomes = Vec::new();
    loop {
        let outcome = stage.step(Some(&mailbox)).unwrap();
        if outcome == StepOutcome::Done {
            break;
        }
        outcomes.push(outcome);
    }
    assert!(mailbox.is_drained());
    let watermark = stage.writer_ref().watermark().unwrap().unwrap();
    assert_eq!((watermark.generation, watermark.sequence), (mailbox.generation(), mailbox.stats().sequence));
    // Steps share transaction groups: every record was written exactly once, in few groups.
    let written: usize =
        outcomes.iter().map(|o| if let StepOutcome::Progressed { records, .. } = o { *records } else { 0 }).sum();
    assert_eq!(written, 5 + 3 * 2 + 1500);
    assert!(outcomes.len() < 6, "{} steps for one small tree", outcomes.len());
    let tree = stage.reader().view(&f.root_bytes(), &[], None, 1, None).unwrap();
    let meta = tree.node(FolderTree::FOLDER).meta;
    assert_eq!(
        (meta.size, meta.allocated, meta.file_count),
        (10 + 3000 + 3 * 4096 + 1500, 4096 + 3 * 4096 + 4096 + 1500 * 4096, 1 + 6 + 1500)
    );
    assert_consistent(stage.writer_ref());
}

/// Steps share one transaction group, and only its commit makes them durable: a failure in a
/// later folder of the group discards the earlier ones too, so nothing half-written or claimed
/// as written survives; the next session lists those folders again.
#[test]
fn a_failed_transaction_group_writes_nothing() {
    let f = Fixture::new();
    for d in ["a", "b"] {
        std::fs::create_dir(f.root.join(d)).unwrap();
    }
    let mut stage = f.open();
    let mailbox = mailbox_for(&f);
    mailbox.enqueue(listing(&f, "", &with_dirs(&f, &["a", "b"], &[file("top", 1, 10)]), true)).unwrap();
    mailbox.enqueue(listing(&f, "a", &with_dirs(&f, &[], &[file("one", 2, 20)]), true)).unwrap();
    mailbox.enqueue(listing(&f, "b", &with_dirs(&f, &[], &[file("two", 3, 30)]), true)).unwrap();
    mailbox.finish();
    let settled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&settled);
    stage.set_before_acknowledge(Some(Box::new(move |_| {
        // The second folder of the group fails after the first was written into it.
        if counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            return Err(CoreError::Invalid("injected".into()));
        }
        Ok(())
    })));
    assert!(stage.step(Some(&mailbox)).is_err());
    assert!(settled.load(std::sync::atomic::Ordering::SeqCst) >= 2, "the group spanned several folders");
    // As for any writer failure, the session stops; resuming lists from what is durable.
    assert!(mailbox.is_cancelled(), "the failed session stopped consuming");
    assert_eq!(stage.writer_ref().watermark().unwrap(), None, "nothing was made durable");
    let rows: i64 = stage.writer_ref().connection().query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 1, "only the root's own row remains: {rows}");
    assert!(stage.writer_ref().connection().is_autocommit(), "the group was closed");
}

#[test]
fn listings_of_a_replaced_folder_are_skipped_and_relisted_locally() {
    let f = Fixture::new();
    f.write("a/real.txt", 300);
    let mut stage = f.open();
    let mailbox = mailbox_for(&f);
    mailbox.enqueue(listing(&f, "", &with_dirs(&f, &["a"], &[]), true)).unwrap();
    // Discovery listed a folder at "a" whose identity no longer matches the registered one.
    let mut stale = listing(&f, "a", &with_dirs(&f, &[], &[file("ghost", 5, 10)]), true);
    stale.parent_identity = FileIdentity { dev: stale.parent_identity.dev, ino: stale.parent_identity.ino + 1 };
    mailbox.enqueue(stale).unwrap();
    mailbox.finish();
    run(&mut stage, Some(&mailbox));
    assert!(mailbox.is_drained());
    assert!(mailbox.progress().uncertain, "skipped observations require reconciliation");
    let tree = stage.reader().view(&crate::paths::join(&f.root_bytes(), b"a"), &[], None, 1, None).unwrap();
    let names: Vec<&[u8]> = tree.children(FolderTree::FOLDER).iter().map(|ix| tree.name(*ix)).collect();
    assert_eq!(names, [&b"real.txt"[..]], "the local pass relisted the folder from disk");
}

#[test]
fn cached_ancestors_are_revalidated_before_accepting_a_listing() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.root.join("a/b")).unwrap();
    std::fs::create_dir_all(f.root.join("a/c")).unwrap();
    let mut stage = f.open();
    let mailbox = mailbox_for(&f);
    mailbox.enqueue(listing(&f, "", &with_dirs(&f, &["a"], &[]), true)).unwrap();
    mailbox.enqueue(listing(&f, "a", &with_dirs(&f, &["a/b", "a/c"], &[]), true)).unwrap();
    mailbox.enqueue(listing(&f, "a/b", &with_dirs(&f, &[], &[file("x", 1, 10)]), true)).unwrap();
    let stale = listing(&f, "a/c", &with_dirs(&f, &[], &[file("y", 2, 10)]), true);
    for _ in 0..3 {
        stage.step(Some(&mailbox)).unwrap(); // the writer's cursor now holds "a" open
    }
    // "a" is replaced (renamed away, recreated) before the writer reaches "a/c".
    std::fs::rename(f.root.join("a"), f.root.join("old")).unwrap();
    std::fs::create_dir_all(f.root.join("a/c")).unwrap();
    mailbox.enqueue(stale).unwrap();
    mailbox.finish();
    run(&mut stage, Some(&mailbox));
    assert!(mailbox.progress().uncertain, "the stale listing was skipped, not attached to the new folder");
    // The local pass converges on the disk contents: no synthetic file survives.
    let tree = stage.reader().view(&f.root_bytes(), &[], None, 1, None).unwrap();
    assert_eq!(tree.node(FolderTree::FOLDER).meta.file_count, 0);
    let mut names: Vec<&[u8]> = tree.children(FolderTree::FOLDER).iter().map(|ix| tree.name(*ix)).collect();
    names.sort();
    assert_eq!(names, [&b"a"[..], b"old"]);
    assert_consistent(stage.writer_ref());
}

#[test]
fn stage_view_counts_each_batch_once_while_the_writer_races() {
    let f = Fixture::new();
    for d in ["a", "b", "c"] {
        std::fs::create_dir(f.root.join(d)).unwrap();
    }
    let stage = Arc::new(Mutex::new(f.open()));
    let mailbox = mailbox_for(&f);
    mailbox.enqueue(listing(&f, "", &with_dirs(&f, &["a", "b", "c"], &[]), true)).unwrap();
    // Two links of identity 900: "/a/l" (committed early) owns it over "/c/l" (pending).
    mailbox.enqueue(listing(&f, "a", &with_dirs(&f, &[], &[link("l", 900, 8192), file("x", 1, 100)]), true)).unwrap();
    let wide: Vec<Rec> = (0..200).map(|i| file(&format!("y{i}"), 10 + i, 1)).collect();
    mailbox.enqueue(listing(&f, "b", &with_dirs(&f, &[], &wide), true)).unwrap();
    mailbox.enqueue(listing(&f, "c", &with_dirs(&f, &[], &[link("l", 900, 8192)]), true)).unwrap();
    let expected = (8192 + 100 + 200 + 8192, 8192 + 4096 + 200 * 4096, 2 + 200 + 1);
    let reader = stage.lock().reader();
    // Commit the root listing only.
    stage.lock().step(Some(&mailbox)).unwrap();
    let check = |tree: &FolderTree| {
        let meta = tree.node(FolderTree::FOLDER).meta;
        assert_eq!((meta.size, meta.allocated, meta.file_count), expected);
        let a = tree.children(FolderTree::FOLDER).iter().copied().find(|ix| tree.name(*ix) == b"a").unwrap();
        assert_eq!(tree.node(a).meta.allocated, 8192 + 4096, "the first link path owns the allocation");
    };
    // The snapshot is captured before the view's transaction; inside it (after the durable
    // watermark was read) the writer commits and acknowledges everything else.
    let snapshot = mailbox.capture();
    let racer = stage.clone();
    let racing_mailbox = mailbox.clone();
    reader.set_after_watermark(Some(Box::new(move || {
        let mut stage = racer.lock();
        while matches!(stage.step(Some(&racing_mailbox)).unwrap(), StepOutcome::Progressed { .. }) {}
    })));
    let tree = reader.view(&f.root_bytes(), &[], Some(&snapshot), 1, None).unwrap();
    reader.set_after_watermark(None);
    check(&tree);
    assert_eq!(mailbox.stats().pending_batches, 0, "the racing writer committed everything");
    // A view taken afterwards (all durable) agrees, with or without the old snapshot.
    check(&reader.view(&f.root_bytes(), &[], Some(&snapshot), 2, None).unwrap());
    check(&reader.view(&f.root_bytes(), &[], None, 3, None).unwrap());
    // A pending folder and an uncommitted batch show their scan states.
    let b = reader.view(&crate::paths::join(&f.root_bytes(), b"b"), &[], None, 4, None).unwrap();
    assert_eq!(b.node(FolderTree::FOLDER).meta.state, crate::types::ScanState::Complete);
}

#[test]
fn removals_hide_matching_identities_and_subtract_totals() {
    let f = Fixture::new();
    f.write("a/big", 100_000);
    f.write("a/small", 10);
    f.write("b/other", 20);
    let mut stage = f.open();
    run(&mut stage, None);
    let snapshot = stage.publish(0.0, PublishOptions::default()).unwrap();
    let store = f.store();
    let big = crate::paths::join(&f.root_bytes(), b"a/big");
    let identity = store.node(&snapshot, &big, None).unwrap().unwrap().meta.identity.unwrap();
    let removals = [
        ScanRemoval { path: big.clone(), identity },
        // A stale identity (the path was reused) is not hidden.
        ScanRemoval {
            path: crate::paths::join(&f.root_bytes(), b"b/other"),
            identity: FileIdentity { dev: 0, ino: 1 },
        },
    ];
    let tree = store.folder(&snapshot, &f.root_bytes(), &removals, None).unwrap();
    assert_eq!(tree.node(FolderTree::FOLDER).meta.size, 30);
    assert_eq!(tree.node(FolderTree::FOLDER).meta.file_count, 2);
    let a = tree.children(FolderTree::FOLDER).iter().copied().find(|ix| tree.name(*ix) == b"a").unwrap();
    assert_eq!(tree.children(a).len(), 1);
    assert_eq!(tree.path(tree.children(a)[0]), crate::paths::join(&f.root_bytes(), b"a/small"));
    let ancestors = store.ancestors(&snapshot, &big, None).unwrap();
    assert_eq!(ancestors.first().unwrap().path, f.root_bytes());
    assert_eq!(ancestors.last().unwrap().path, big);
}

#[test]
fn saved_views_are_bounded_sorted_and_keep_raw_names() {
    let f = Fixture::new();
    for i in 0..30 {
        for j in 0..40 {
            f.write(&format!("d{i:02}/s{j:02}/file"), 1 + i * 100 + j);
        }
    }
    // Decomposed (NFD) bytes are kept exactly; APFS refuses invalid UTF-8, which the writer
    // tests cover instead.
    let nfd = "cafe\u{301}";
    std::fs::write(f.root.join(nfd), b"x").unwrap();
    let mut stage = f.open();
    run(&mut stage, None);
    let snapshot = stage.publish(0.0, PublishOptions::default()).unwrap();
    let tree = f.store().folder(&snapshot, &f.root_bytes(), &[], None).unwrap();
    let direct = tree.children(FolderTree::FOLDER);
    assert_eq!(direct.len(), 31, "all direct children");
    assert!(tree.len() - 1 - direct.len() <= crate::view::VIEW_DESCENDANT_BUDGET);
    let sizes: Vec<u64> = direct.iter().map(|ix| tree.node(*ix).meta.size).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "largest first");
    let first = direct[0];
    assert_eq!(tree.node(first).total_children, 40);
    assert!(direct.iter().any(|ix| tree.name(*ix) == nfd.as_bytes()));
    for ix in 1..tree.len() as u32 {
        let depth = std::iter::successors(Some(ix), |i| Some(tree.node(*i).parent))
            .take_while(|i| *i != crate::view::NO_PARENT && *i != FolderTree::FOLDER)
            .count();
        assert!(depth as u32 <= crate::view::VIEW_DEPTH, "descendants stay within the view depth");
    }
}

// --- Publication ---------------------------------------------------------------------------------

#[test]
fn crash_after_clone_keeps_the_old_map_and_the_orphan() {
    let f = Fixture::new();
    f.write("a/file", 100);
    let mut stage = f.open();
    run(&mut stage, None);
    let first = stage.publish(0.0, PublishOptions::default()).unwrap();
    drop(stage);
    Stage::<Native>::retire_published(&f.paths, &f.root_bytes(), f.identity(""), &first.id, &Cancellation::new())
        .unwrap();
    f.write("a/second", 5000);
    let mut stage = f.open();
    assert_eq!(stage.expectation(), &SnapshotExpectation::Matching(first.id.clone()));
    run(&mut stage, None);
    let mut crash = |b: &str| if b == "cloned" { BoundaryAction::Crash } else { BoundaryAction::Continue };
    assert!(stage.publish(0.0, PublishOptions { boundary: Some(&mut crash), ..Default::default() }).is_err());
    drop(stage); // the process died after cloning
    let store = f.store();
    assert_eq!(store.saved_snapshot(&f.root_bytes()).unwrap().id, first.id);
    assert_eq!(saved_totals(&store, &f.root_bytes()).0, 100);
    assert_eq!(f.orphans(), 1, "a crashed publication leaves its hidden clone for cache cleanup");
    // The sealed stage resumes and publishes; the orphan is never deleted automatically.
    let mut stage = f.open();
    assert!(stage.is_resumed());
    run(&mut stage, None);
    let second = stage.publish(0.0, PublishOptions::default()).unwrap();
    assert_ne!(second.id, first.id);
    assert_eq!(saved_totals(&store, &f.root_bytes()).0, 5100);
    assert_eq!(f.orphans(), 1);
}

#[test]
fn crash_after_rename_publishes_the_complete_map_once() {
    let f = Fixture::new();
    f.write("a/file", 100);
    let mut stage = f.open();
    run(&mut stage, None);
    let mut crash = |b: &str| if b == "renamed" { BoundaryAction::Crash } else { BoundaryAction::Continue };
    assert!(stage.publish(2.0, PublishOptions { boundary: Some(&mut crash), ..Default::default() }).is_err());
    drop(stage);
    let saved = f.store().saved_snapshot(&f.root_bytes()).unwrap();
    assert_eq!((saved.event_id, saved.event_identity.as_str()), (1, "history"));
    assert_eq!(saved_totals(&f.store(), &f.root_bytes()).0, 100);
    match f.try_open(5).unwrap() {
        Opened::AlreadyPublished(id) => assert_eq!(id, saved.id),
        other => panic!("{other:?}"),
    }
}

#[test]
fn failed_publication_removes_its_clone_and_resumes_wal() {
    let f = Fixture::new();
    f.write("a/file", 100);
    let mut stage = f.open();
    run(&mut stage, None);
    let mut refuse = |_: &mut dyn FnMut() -> Result<()>| -> Result<()> { Err(CoreError::SnapshotChanged) };
    let error = stage.publish(0.0, PublishOptions { fence: Some(&mut refuse), ..Default::default() }).unwrap_err();
    assert!(matches!(error, CoreError::SnapshotChanged));
    assert_eq!(f.orphans(), 0);
    let mode: String = stage.writer_ref().connection().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "wal");
    assert!(f.store().saved_snapshot(&f.root_bytes()).is_none());
    stage.publish(0.0, PublishOptions::default()).unwrap();
}

#[test]
fn invalidation_makes_running_stages_unpublishable() {
    let f = Fixture::new();
    f.write("a/file", 100);
    let mut stage = f.open();
    run(&mut stage, None);
    f.store().invalidate(&f.root_bytes(), &Cancellation::new()).unwrap();
    assert!(matches!(stage.publish(0.0, PublishOptions::default()), Err(CoreError::SnapshotChanged)));
    assert!(f.store().saved_snapshot(&f.root_bytes()).is_none());
    drop(stage);
    assert!(matches!(f.try_open(1).unwrap(), Opened::Invalidated));
    Stage::<Native>::retire_invalidated(&f.paths, &f.root_bytes(), f.identity(""), &Cancellation::new()).unwrap();
    let mut stage = f.open();
    assert!(!stage.is_resumed());
    run(&mut stage, None);
    stage.publish(0.0, PublishOptions::default()).unwrap();
    assert!(f.store().saved_snapshot(&f.root_bytes()).is_some());
}

#[test]
fn recovery_copy_replaces_the_map_only_through_the_fence() {
    let f = Fixture::new();
    f.write("a/file", 100);
    let mut stage = f.open();
    run(&mut stage, None);
    let original = stage.publish(0.0, PublishOptions::default()).unwrap();
    let store = f.store();
    let cancel = Cancellation::new();
    let copy = store.recovery_working_copy(f.platform.as_ref(), &original, &cancel).unwrap();
    {
        let conn = rusqlite::Connection::open(&copy.database).unwrap();
        crate::stage::writer::write_removal_cursor(&conn, &uuid::Uuid::new_v4().to_string(), 3).unwrap();
    }
    // A failing fence keeps the original published.
    let mut refuse = |_: &mut dyn FnMut() -> Result<()>| -> Result<()> { Err(CoreError::SnapshotChanged) };
    assert!(store.publish_recovery_copy(&copy, &original, &mut refuse, &cancel).is_err());
    assert_eq!(store.removal_cursor(&original, None).unwrap(), None);
    let mut pass = |commit: &mut dyn FnMut() -> Result<()>| commit();
    let published = store.publish_recovery_copy(&copy, &original, &mut pass, &cancel).unwrap();
    assert_eq!(store.removal_cursor(&published, None).unwrap().unwrap().1, 3);
}
