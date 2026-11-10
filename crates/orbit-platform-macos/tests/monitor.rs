//! Real FSEvents integration. Waits are bounded channel receives (FSEvents batches arrive after
//! its 0.6 s latency); nothing sleeps.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use orbit_platform::{ChangeBatch, ChangeMonitor, MonitorStart, Platform};
use orbit_platform_macos::MacPlatform;

const TIMEOUT: Duration = Duration::from_secs(20);

fn wait_for(rx: &mpsc::Receiver<ChangeBatch>, what: &str, matches: impl Fn(&ChangeBatch) -> bool) -> ChangeBatch {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(batch) if matches(&batch) => return batch,
            Ok(_) => continue,
            Err(error) => panic!("timed out waiting for {what}: {error}"),
        }
    }
}

fn standardized(path: &Path) -> PathBuf {
    path.components().collect()
}

#[test]
fn live_changes_reach_the_sink_and_stop_ends_delivery() {
    let temp = tempfile::tempdir().unwrap();
    let root = standardized(&temp.path().join("root"));
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let platform = MacPlatform::new();
    let mut monitor = platform.new_monitor();
    assert!(monitor.has_history());
    let checkpoint = monitor.capture(&root).unwrap();

    let (tx, rx) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let late_calls = Arc::new(AtomicUsize::new(0));
    let alive = Arc::new(());
    let sink = {
        let (stopped, late_calls, alive) = (Arc::clone(&stopped), Arc::clone(&late_calls), Arc::clone(&alive));
        Box::new(move |batch: ChangeBatch| {
            let _ = &alive;
            if stopped.load(Ordering::SeqCst) {
                late_calls.fetch_add(1, Ordering::SeqCst);
            }
            let _ = tx.send(batch);
        })
    };
    assert_eq!(monitor.start(&root, &[], Some(&checkpoint), sink).unwrap(), MonitorStart::Replaying);
    wait_for(&rx, "history replay", |b| b.history_finished);

    let file = root.join("event-file");
    std::fs::write(&file, b"1").unwrap();
    let created = wait_for(&rx, "create", |b| b.directories.contains(&root));
    assert!(!created.full_refresh && created.checkpoint.identity == checkpoint.identity, "{created:?}");
    assert!(created.checkpoint.event_id >= checkpoint.event_id);
    std::fs::write(root.join("sub/nested"), b"2").unwrap();
    wait_for(&rx, "nested create", |b| b.directories.contains(&root.join("sub")));
    std::fs::remove_file(&file).unwrap();
    wait_for(&rx, "delete", |b| b.directories.contains(&root));

    monitor.stop();
    stopped.store(true, Ordering::SeqCst);
    assert_eq!(Arc::strong_count(&alive), 1, "the sink must be dropped once stop returns");

    // Changes after stop: a second monitor observing them proves FSEvents delivered them.
    let mut witness = platform.new_monitor();
    let since = witness.capture(&root).unwrap();
    let (witness_tx, witness_rx) = mpsc::channel();
    witness
        .start(
            &root,
            &[],
            Some(&since),
            Box::new(move |batch| {
                let _ = witness_tx.send(batch);
            }),
        )
        .unwrap();
    std::fs::write(root.join("after-stop"), b"3").unwrap();
    wait_for(&witness_rx, "witness create", |b| b.directories.contains(&root));
    witness.stop();
    assert_eq!(late_calls.load(Ordering::SeqCst), 0, "sink called after stop returned");
}

#[test]
fn foreign_checkpoint_requests_a_full_refresh_first() {
    let temp = tempfile::tempdir().unwrap();
    let root = standardized(&temp.path().join("root"));
    let cache = root.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let platform = MacPlatform::new();
    let mut monitor = platform.new_monitor();
    let mut stale = monitor.capture(&root).unwrap();
    let current_identity = stale.identity.clone();
    stale.identity.push_str("-from-another-volume");

    let (tx, rx) = mpsc::channel();
    let start = monitor
        .start(
            &root,
            std::slice::from_ref(&cache),
            Some(&stale),
            Box::new(move |batch| {
                let _ = tx.send(batch);
            }),
        )
        .unwrap();
    assert_eq!(start, MonitorStart::FullRefresh);
    let first = rx.recv_timeout(TIMEOUT).unwrap();
    assert!(first.full_refresh && first.directories.is_empty() && first.subtrees == vec![root.clone()], "{first:?}");
    assert_eq!(first.checkpoint.identity, current_identity);
    wait_for(&rx, "history", |b| b.history_finished);

    // Writes into the excluded cache produce nothing; a write next to it does.
    for index in 0..20 {
        std::fs::write(cache.join(format!("index-{index}")), b"x").unwrap();
    }
    std::fs::write(root.join("outside-cache"), b"x").unwrap();
    let batch = wait_for(&rx, "non-excluded write", |b| !b.directories.is_empty() || b.full_refresh);
    assert!(!batch.full_refresh && batch.directories == vec![root.clone()], "{batch:?}");
    monitor.stop();
}

#[test]
fn history_probe_rejects_a_changed_subtree() {
    let temp = tempfile::tempdir().unwrap();
    let root = standardized(&temp.path().join("root"));
    std::fs::create_dir_all(&root).unwrap();
    let platform = MacPlatform::new();
    let monitor = platform.new_monitor();
    let checkpoint = monitor.capture(&root).unwrap();
    std::fs::write(root.join("changed"), b"x").unwrap();
    assert!(!monitor.history_unchanged_since(&checkpoint, Duration::from_secs(5)), "a recorded change was ignored");
    let mut foreign = checkpoint.clone();
    foreign.identity.push('x');
    assert!(!monitor.history_unchanged_since(&foreign, Duration::from_secs(5)), "foreign history accepted");
}
