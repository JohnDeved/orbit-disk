//! Recovery planning, saved-map reconciliation and stage replay (Swift `OrbitRemovalJournalChecks`
//! history/captured-tail parts, `OrbitStageRemovalChecks`, core rows of
//! `OrbitDurableRemovalChecks`).

use super::fake::{FakeClock, FakeMap, FakeStage};
use super::*;
use crate::journal::tests::support::*;
use crate::journal::{Hooks, Journal};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn cursor_of(journal: &Journal) -> RemovalCursor {
    journal
        .with_fence(&none(), |f| Ok(RemovalCursor { identity: f.identity().to_string(), sequence: f.high_water() }))
        .unwrap()
}

fn begin_complete(journal: &Journal, root: &Path, item: &Path) -> i64 {
    let op = journal.begin(&bytes(root), id(root), &[removal(item)], &none()).unwrap();
    let _ = std::fs::remove_file(item);
    op.complete(&none()).unwrap();
    op.sequence()
}

use std::path::Path;

// --- Planning -------------------------------------------------------------------------------

#[test]
fn live_and_completed_operations_beyond_the_history_window_are_recovered() {
    let fx = fixture();
    write(&fx.path("root/victim"), b"1");
    let journal = fx.journal();
    cursor_of(&journal);
    seed_unrelated(&fx.raw_db(), 260);
    let root = bytes(&fx.path("root"));
    let live = journal.begin(&root, id(&fx.path("root")), &[removal(&fx.path("root/victim"))], &none()).unwrap();
    let clock = FakeClock::default();
    let plan = journal.recover(&root, None, &none(), &clock).unwrap();
    assert!(plan.has_live_operation);
    assert_eq!(plan.removals, [removal(&fx.path("root/victim"))]);
    assert_eq!(plan.cursor.sequence, 261);
    live.complete(&none()).unwrap();
    let plan = journal.recover(&root, None, &none(), &clock).unwrap();
    assert!(!plan.has_live_operation);
    assert_eq!(plan.affected_parents.iter().collect::<Vec<_>>(), [&root]);
    // From the captured cursor nothing is probed.
    Hooks::set(&journal.hooks.after_recovery_probe, || panic!("idle recovery probed an operation"));
    let idle = journal.recover(&root, Some(&plan.cursor), &none(), &clock).unwrap();
    assert!(idle.is_empty() && idle.cursor == plan.cursor);
}

#[test]
fn probes_respect_the_injected_budget() {
    let fx = fixture();
    std::fs::create_dir_all(fx.path("root")).unwrap();
    let journal = fx.journal();
    cursor_of(&journal);
    seed_unrelated(&fx.raw_db(), 300);
    let probes = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(FakeClock::default());
    {
        let (probes, clock) = (Arc::clone(&probes), Arc::clone(&clock));
        // Every probe "takes" 2 ms: the 5 ms budget allows three probes per fence.
        Hooks::set(&journal.hooks.after_recovery_probe, move || {
            probes.fetch_add(1, Ordering::SeqCst);
            clock.advance(Duration::from_millis(2));
        });
    }
    let plan = journal.recover(&bytes(&fx.path("root")), None, &none(), clock.as_ref()).unwrap();
    assert!(plan.is_empty());
    assert_eq!(plan.cursor.sequence, 300);
    assert_eq!(probes.load(Ordering::SeqCst), 300, "every operation is probed exactly once");
}

#[test]
fn captured_tail_is_required_and_later_appends_wait_for_the_next_recovery() {
    for removes_tail in [true, false] {
        let fx = fixture();
        std::fs::create_dir_all(fx.path("root")).unwrap();
        let journal = fx.journal();
        cursor_of(&journal);
        seed_unrelated(&fx.raw_db(), 256);
        let db = fx.db_path();
        let count = Arc::new(AtomicUsize::new(0));
        {
            let count = Arc::clone(&count);
            Hooks::set(&journal.hooks.after_recovery_probe, move || {
                if count.fetch_add(1, Ordering::SeqCst) > 0 {
                    return;
                }
                // Damage the captured tail and append, without taking the fence.
                let conn = rusqlite::Connection::open(&db).unwrap();
                let delete = if removes_tail {
                    "DELETE FROM items WHERE op>128; DELETE FROM operations WHERE seq>128;"
                } else {
                    ""
                };
                conn.execute_batch(&format!(
                    "BEGIN IMMEDIATE; {delete}
                     INSERT INTO operations VALUES(257, CAST('/unrelated' AS BLOB), 1, 1,
                        '00000101-0000-0000-0000-000000000000.operation', 1);
                     INSERT INTO items VALUES(257, CAST('/unrelated/new' AS BLOB), 1, 2, CAST('/unrelated/new' AS BLOB), 1, 1);
                     COMMIT;"
                ))
                .unwrap();
            });
        }
        let root = bytes(&fx.path("root"));
        let result = journal.recover(&root, None, &none(), &FakeClock::default());
        if removes_tail {
            assert!(
                matches!(result, Err(JournalError::Invalid(ref m)) if m == "Missing removal journal entries"),
                "{result:?}"
            );
            assert_eq!(count.load(Ordering::SeqCst), 128, "only the retained prefix is probed");
        } else {
            let plan = result.unwrap();
            assert_eq!((plan.cursor.sequence, count.load(Ordering::SeqCst)), (256, 256));
            let next = journal.recover(&root, Some(&plan.cursor), &none(), &FakeClock::default()).unwrap();
            assert_eq!(next.cursor.sequence, 257);
        }
    }
}

#[test]
fn root_replaced_by_symlink_during_recovery_is_a_journal_change() {
    let fx = fixture();
    std::fs::create_dir_all(fx.path("A/root")).unwrap();
    write(&fx.path("other/x"), b"x");
    let journal = fx.journal();
    begin_complete(&journal, &fx.path("other"), &fx.path("other/x"));
    let (a, b) = (fx.path("A"), fx.path("B"));
    Hooks::set(&journal.hooks.after_recovery_probe, move || {
        if !b.exists() {
            std::fs::rename(&a, &b).unwrap();
            std::os::unix::fs::symlink(&b, &a).unwrap();
        }
    });
    let result = journal.recover(&bytes(&fx.path("A/root")), None, &none(), &FakeClock::default());
    assert!(matches!(result, Err(JournalError::JournalChanged)), "{result:?}");
}

#[test]
fn coverage_and_limits_decide_full_refresh_and_overflow() {
    let fx = fixture();
    std::fs::create_dir_all(fx.path("root/sub")).unwrap();
    let journal = fx.journal();
    let root = bytes(&fx.path("root"));
    // A folder that contains the recovery root.
    let covering = journal.begin(&root, id(&fx.path("root")), &[removal(&fx.path("root/sub"))], &none()).unwrap();
    covering.complete(&none()).unwrap();
    let plan = journal.recover(&bytes(&fx.path("root/sub")), None, &none(), &FakeClock::default()).unwrap();
    assert!(plan.requires_full_refresh);

    // Raw rows: 10,001 removals in one parent overflow the overlay only; 10,001 parents force a
    // full refresh.
    let fresh = journal.recover(&root, None, &none(), &FakeClock::default()).unwrap().cursor;
    let conn = fx.raw_db();
    let insert = |seq: i64, parent_of: &dyn Fn(i64) -> String| {
        conn.execute(
            "INSERT INTO operations VALUES(?1, ?2, 1, 1, printf('%08X-0000-0000-0000-000000000000.operation', ?1), 1)",
            rusqlite::params![seq, root],
        )
        .unwrap();
        let mut stmt = conn.prepare("INSERT INTO items VALUES(?1, ?2, 1, ?3, ?2, 1, 1)").unwrap();
        for i in 0..=10_000i64 {
            let path = format!("{}/{}/f{i}", fx.path("root").display(), parent_of(i));
            stmt.execute(rusqlite::params![seq, path.as_bytes(), i + 1]).unwrap();
        }
    };
    conn.execute_batch("BEGIN").unwrap();
    insert(fresh.sequence + 1, &|_| "one".into());
    conn.execute_batch("COMMIT").unwrap();
    let plan = journal.recover(&root, Some(&fresh), &none(), &FakeClock::default()).unwrap();
    assert!(plan.overlay_overflow && !plan.requires_full_refresh);
    assert_eq!((plan.removals.len(), plan.affected_parents.len()), (OVERLAY_LIMIT, 1));
    conn.execute_batch("BEGIN").unwrap();
    insert(fresh.sequence + 2, &|i| format!("p{i}"));
    conn.execute_batch("COMMIT").unwrap();
    let plan = journal.recover(&root, Some(&fresh), &none(), &FakeClock::default()).unwrap();
    assert!(plan.requires_full_refresh);
    assert_eq!(plan.affected_parents.len(), PARENT_LIMIT);
    let mut map = FakeMap::scan(&fx.path("root"));
    map.cursor = Some(fresh);
    let result = reconcile_saved_map(&journal, &mut map, &none(), &FakeClock::default()).unwrap();
    assert!(matches!(result, SavedRecovery::FullRefresh(_)));
    assert_eq!(map.full_refresh.as_ref().map(|c| c.sequence), Some(plan.cursor.sequence));
    assert_eq!(map.publishes, 0);
}

// --- Saved maps -----------------------------------------------------------------------------

#[test]
fn live_operation_is_never_retired_by_a_saved_map() {
    let fx = fixture();
    write(&fx.path("root/a/victim"), b"v");
    write(&fx.path("root/a/keep"), b"k");
    let journal = fx.journal();
    let mut map = FakeMap::scan(&fx.path("root"));
    map.cursor = Some(cursor_of(&journal));
    let op = journal
        .begin(&bytes(&fx.path("root")), id(&fx.path("root")), &[removal(&fx.path("root/a/victim"))], &none())
        .unwrap();
    std::fs::remove_file(fx.path("root/a/victim")).unwrap();
    let clock = FakeClock::default();
    let result = reconcile_saved_map(&journal, &mut map, &none(), &clock).unwrap();
    let SavedRecovery::Live(plan) = result else { panic!("{result:?}") };
    assert_eq!(plan.removals.len(), 1);
    assert_eq!(plan.removals[0].path, bytes(&fx.path("root/a/victim")));
    assert_eq!(map.cursor.as_ref().unwrap().sequence, 0, "a live operation keeps the cursor");
    assert_eq!(map.publishes, 0);
    op.complete(&none()).unwrap();
    let result = reconcile_saved_map(&journal, &mut map, &none(), &clock).unwrap();
    assert_eq!(result, SavedRecovery::Published(cursor_of(&journal)));
    assert!(!map.published.contains_key(&bytes(&fx.path("root/a/victim"))));
    assert!(map.published.contains_key(&bytes(&fx.path("root/a/keep"))));
    let again = reconcile_saved_map(&journal, &mut map, &none(), &clock).unwrap();
    assert!(matches!(again, SavedRecovery::UpToDate(_)));
}

#[test]
fn high_water_fence_refuses_a_stale_publication() {
    let fx = fixture();
    write(&fx.path("root/a/first"), b"1");
    write(&fx.path("root/b/second"), b"2");
    write(&fx.path("root/b/keep"), b"3");
    let journal = Arc::new(fx.journal());
    let mut map = FakeMap::scan(&fx.path("root"));
    map.cursor = Some(cursor_of(&journal));
    begin_complete(&journal, &fx.path("root"), &fx.path("root/a/first"));
    let reconciles = Arc::new(AtomicUsize::new(0));
    {
        let (journal, reconciles) = (Arc::clone(&journal), Arc::clone(&reconciles));
        let (root, second) = (fx.path("root"), fx.path("root/b/second"));
        // A newer removal lands while the first plan is being applied.
        map.on_reconcile = Some(Box::new(move || {
            if reconciles.fetch_add(1, Ordering::SeqCst) == 0 {
                begin_complete(&journal, &root, &second);
            }
        }));
    }
    let result = reconcile_saved_map(&journal, &mut map, &none(), &FakeClock::default()).unwrap();
    assert_eq!(result, SavedRecovery::Published(RemovalCursor { identity: cursor_of(&journal).identity, sequence: 2 }));
    assert_eq!(map.publishes, 1, "the stale plan was never published");
    assert!(reconciles.load(Ordering::SeqCst) >= 2);
    for gone in ["root/a/first", "root/b/second"] {
        assert!(!map.published.contains_key(&bytes(&fx.path(gone))), "{gone}");
    }
    assert!(map.published.contains_key(&bytes(&fx.path("root/b/keep"))));
}

#[test]
fn replaced_item_and_parent_keep_the_new_versions() {
    let fx = fixture();
    write(&fx.path("root/file"), b"old");
    write(&fx.path("root/dir/inner"), b"old");
    let journal = fx.journal();
    let mut map = FakeMap::scan(&fx.path("root"));
    map.cursor = Some(cursor_of(&journal));
    let root = bytes(&fx.path("root"));
    let items = [removal(&fx.path("root/file")), removal(&fx.path("root/dir/inner"))];
    let op = journal.begin(&root, id(&fx.path("root")), &items, &none()).unwrap();
    std::fs::remove_file(fx.path("root/file")).unwrap();
    std::fs::remove_dir_all(fx.path("root/dir")).unwrap();
    op.complete(&none()).unwrap();
    // Something else now lives at both paths.
    write(&fx.path("root/file"), b"new");
    write(&fx.path("root/dir/inner"), b"new");
    let result = reconcile_saved_map(&journal, &mut map, &none(), &FakeClock::default()).unwrap();
    assert!(matches!(result, SavedRecovery::Published(_)), "{result:?}");
    for rel in ["root/file", "root/dir", "root/dir/inner"] {
        assert_eq!(map.published.get(&bytes(&fx.path(rel))), Some(&id(&fx.path(rel))), "{rel}");
    }
}

#[test]
fn replaced_journal_forces_full_reconciliation() {
    let fx = fixture();
    write(&fx.path("root/victim"), b"v");
    let journal = fx.journal();
    let mut map = FakeMap::scan(&fx.path("root"));
    map.cursor = Some(cursor_of(&journal));
    std::fs::remove_file(fx.db_path()).unwrap();
    let result = reconcile_saved_map(&journal, &mut map, &none(), &FakeClock::default()).unwrap();
    assert!(matches!(result, SavedRecovery::FullRefresh(ref p) if p.requires_full_refresh), "{result:?}");
    assert_eq!(map.full_refresh, Some(cursor_of(&journal)));
}

#[test]
fn cancelled_reconciliation_keeps_the_published_map() {
    let fx = fixture();
    write(&fx.path("root/a/victim"), b"v");
    let journal = fx.journal();
    let mut map = FakeMap::scan(&fx.path("root"));
    let start = cursor_of(&journal);
    map.cursor = Some(start.clone());
    begin_complete(&journal, &fx.path("root"), &fx.path("root/a/victim"));
    let cancel = Cancellation::new();
    let token = cancel.clone();
    map.on_reconcile = Some(Box::new(move || token.cancel()));
    let error = reconcile_saved_map(&journal, &mut map, &cancel, &FakeClock::default()).unwrap_err();
    assert!(error.is_cancelled(), "{error:?}");
    assert_eq!((map.publishes, map.cursor.clone()), (0, Some(start)));
    assert!(map.published.contains_key(&bytes(&fx.path("root/a/victim"))));
    map.on_reconcile = None;
    let retried = reconcile_saved_map(&journal, &mut map, &none(), &FakeClock::default()).unwrap();
    assert!(matches!(retried, SavedRecovery::Published(_)));
    assert!(!map.published.contains_key(&bytes(&fx.path("root/a/victim"))));
}

// --- Stage replay ---------------------------------------------------------------------------

fn replay(journal: &Arc<Journal>, stage: &mut FakeStage, clock: &Arc<FakeClock>) -> StageReplay {
    StageReplay::open(Arc::clone(journal), stage, none(), Arc::clone(clock) as Arc<dyn RecoveryClock>, true).unwrap()
}

/// Runs the writer until replay settles; returns the settled cursor sequence.
fn settle(replay: &mut StageReplay, stage: &mut FakeStage) -> i64 {
    for _ in 0..8 {
        stage.drain();
        if replay.settle_if_possible(stage).unwrap() {
            return replay.view().get().cursor.unwrap().sequence;
        }
    }
    panic!("replay did not settle");
}

#[test]
fn fresh_stage_anchors_before_the_first_live_operation() {
    let fx = fixture();
    write(&fx.path("root/a"), b"a");
    write(&fx.path("root/b"), b"b");
    let journal = Arc::new(fx.journal());
    begin_complete(&journal, &fx.path("root"), &fx.path("root/a"));
    // An abandoned operation (owner gone) before the live one is safe to skip.
    drop(
        journal.begin(&bytes(&fx.path("root")), id(&fx.path("root")), &[removal(&fx.path("root/b"))], &none()).unwrap(),
    );
    let live =
        journal.begin(&bytes(&fx.path("root")), id(&fx.path("root")), &[removal(&fx.path("root/b"))], &none()).unwrap();
    let clock = Arc::new(FakeClock::default());
    let mut stage = FakeStage::new(&fx.path("root"), true);
    let mut replay = replay(&journal, &mut stage, &clock);
    assert_eq!(stage.cursor.as_ref().unwrap().sequence, 2, "anchored just before the live operation");
    replay.prepare(&mut stage).unwrap();
    let view = replay.view().get();
    assert!(!view.blocked, "a live intent must not hide the unrelated staged map");
    assert_eq!(view.overlay.len(), 1);
    assert!(stage.dirty.is_empty(), "parents are dirtied only after the owner finishes");
    stage.drain();
    assert!(!replay.settle_if_possible(&mut stage).unwrap());
    assert_eq!(stage.cursor.as_ref().unwrap().sequence, 2);

    std::fs::remove_file(fx.path("root/b")).unwrap();
    live.complete(&none()).unwrap();
    replay.refresh_if_needed(&mut stage).unwrap();
    assert_eq!(stage.cursor.as_ref().unwrap().sequence, 2, "probes wait for the interval");
    clock.advance(replay::PROBE_INTERVAL);
    replay.refresh_if_needed(&mut stage).unwrap();
    assert!(stage.dirty.contains(&bytes(&fx.path("root"))));
    assert_eq!(settle(&mut replay, &mut stage), 3);
    assert_eq!(stage.cursor.as_ref().unwrap().sequence, 3);
}

#[test]
fn anchor_capture_race_and_unanchored_stage_replay_everything() {
    let fx = fixture();
    write(&fx.path("root/item"), b"i");
    let journal = Arc::new(fx.journal());
    let clock = Arc::new(FakeClock::default());
    let mut stage = FakeStage::new(&fx.path("root"), true);
    let mut replay = StageReplay::new(Arc::clone(&journal), none(), Arc::clone(&clock) as Arc<dyn RecoveryClock>);
    {
        let (journal, root, item) = (Arc::clone(&journal), fx.path("root"), fx.path("root/item"));
        replay.after_fresh_capture = Some(Box::new(move || {
            begin_complete(&journal, &root, &item);
        }));
    }
    replay.anchor(&mut stage, true).unwrap();
    assert_eq!(stage.cursor.as_ref().unwrap().sequence, 0, "the anchor is the captured high-water");
    replay.prepare(&mut stage).unwrap();
    assert!(stage.dirty.contains(&bytes(&fx.path("root"))), "the racing removal is replayed");
    assert_eq!(settle(&mut replay, &mut stage), 1);

    // A stage that crashed before storing its anchor is not fresh: it replays from zero.
    let mut reopened = FakeStage::new(&fx.path("root"), false);
    let mut replay = self::replay(&journal, &mut reopened, &clock);
    assert_eq!(reopened.cursor, None);
    replay.prepare(&mut reopened).unwrap();
    assert!(reopened.dirty.contains(&bytes(&fx.path("root"))));
    assert_eq!(settle(&mut replay, &mut reopened), 1);
}

#[test]
fn replay_never_consumes_beyond_its_captured_high_water() {
    let fx = fixture();
    write(&fx.path("root/victim"), b"v");
    let journal = Arc::new(fx.journal());
    cursor_of(&journal);
    seed_unrelated(&fx.raw_db(), 3);
    let clock = Arc::new(FakeClock::default());
    let mut stage = FakeStage::new(&fx.path("root"), false);
    stage.cursor = Some(cursor_of(&journal)).map(|c| RemovalCursor { sequence: 0, ..c });
    let mut replay = replay(&journal, &mut stage, &clock);
    {
        let (journal, root, victim) = (Arc::clone(&journal), fx.path("root"), fx.path("root/victim"));
        let mut once = false;
        replay.after_replay_head = Some(Box::new(move |_| {
            if !std::mem::replace(&mut once, true) {
                begin_complete(&journal, &root, &victim);
            }
        }));
    }
    replay.prepare(&mut stage).unwrap();
    replay.after_replay_head = None;
    assert!(stage.dirty.is_empty() && replay.view().get().overlay.is_empty(), "applied beyond the high-water");
    // Settling notices the journal moved, re-prepares and then settles at the new head.
    assert_eq!(settle(&mut replay, &mut stage), 4);
    // Publication is fenced at the settled cursor.
    replay
        .fence_publication(|f| {
            assert_eq!(f.high_water(), 4);
            Ok(())
        })
        .unwrap();
    write(&fx.path("root/later"), b"l");
    begin_complete(&journal, &fx.path("root"), &fx.path("root/later"));
    assert!(matches!(replay.fence_publication(|_| Ok(())), Err(JournalError::JournalChanged)));
}

#[test]
fn replay_cancellation_and_root_replacement_keep_the_cursor() {
    for replace_root in [false, true] {
        let fx = fixture();
        std::fs::create_dir_all(fx.path("holder/root")).unwrap();
        let journal = Arc::new(fx.journal());
        cursor_of(&journal);
        seed_unrelated(&fx.raw_db(), 4);
        let clock = Arc::new(FakeClock::default());
        let mut stage = FakeStage::new(&fx.path("holder/root"), true);
        stage.fresh = false;
        let cancel = Cancellation::new();
        let mut replay = StageReplay::open(
            Arc::clone(&journal),
            &mut stage,
            cancel.clone(),
            Arc::clone(&clock) as Arc<dyn RecoveryClock>,
            false,
        )
        .unwrap();
        let (root, moved) = (fx.path("holder/root"), fx.path("holder/moved"));
        let mut once = false;
        Hooks::set(&journal.hooks.after_recovery_probe, move || {
            if std::mem::replace(&mut once, true) {
                return;
            }
            if replace_root {
                std::fs::rename(&root, &moved).unwrap();
                std::fs::create_dir(&root).unwrap();
            } else {
                cancel.cancel();
            }
        });
        let error = replay.prepare(&mut stage).unwrap_err();
        if replace_root {
            assert!(matches!(error, JournalError::JournalChanged), "{error:?}");
        } else {
            assert!(error.is_cancelled(), "{error:?}");
        }
        assert_eq!(stage.cursor_writes, 0);
        assert!(replay.view().get().blocked);
    }
}

#[test]
fn replaced_journal_blocks_the_stage_and_requests_full_reconciliation() {
    let fx = fixture();
    write(&fx.path("root/item"), b"i");
    let journal = Arc::new(fx.journal());
    let clock = Arc::new(FakeClock::default());
    let mut stage = FakeStage::new(&fx.path("root"), true);
    let mut first = replay(&journal, &mut stage, &clock);
    first.prepare(&mut stage).unwrap();
    assert_eq!(settle(&mut first, &mut stage), 0);
    std::fs::remove_file(fx.path("root/item")).unwrap();
    std::fs::remove_file(fx.db_path()).unwrap();
    stage.fresh = false;
    let mut second = replay(&journal, &mut stage, &clock);
    second.prepare(&mut stage).unwrap();
    assert!(second.view().get().blocked && stage.full_refresh);
    assert!(!second.settle_if_possible(&mut stage).unwrap(), "waits for the full refresh");
    stage.drain();
    assert!(second.settle_if_possible(&mut stage).unwrap());
    assert_eq!(stage.cursor, Some(cursor_of(&journal)));
    assert!(!second.view().get().blocked);
}
