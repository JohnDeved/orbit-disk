//! Port of `Benchmarks/DurableIndexing/PipelineProbe.swift`: one full mapping session on real
//! files (discovery and the stage writer overlapping), then publication and verification.
//!
//! ```text
//! pipeline_probe ROOT CACHE FILES DIRECTORIES      (prints one JSON object)
//! ```
//!
//! Output keys match the Swift probe so `scripts/benchmarks/run-indexing-pipeline.py` can run
//! both interleaved. `CACHE` must not exist; the runner deletes it afterwards.

#![allow(clippy::default_constructed_unit_structs)] // `Native` is a unit struct only on some OSes.

use orbit_core::discovery::DiscoveryHooks;
use orbit_core::session::{MappingSession, SessionOptions, SessionOutcome};
use orbit_core::store::IndexPaths;
use orbit_native::Native;
use orbit_platform::ChangeCheckpoint;
use parking_lot::Mutex;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Counter(AtomicU64);

impl DiscoveryHooks for Counter {
    fn before_directory(&self, _path: &[u8], _ticket: u64) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn scalar(path: &std::path::Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: pipeline_probe ROOT CACHE FILES DIRECTORIES");
        std::process::exit(2);
    }
    let root = std::fs::canonicalize(&args[1]).expect("root");
    let cache = PathBuf::from(&args[2]);
    let files: i64 = args[3].parse().unwrap();
    let directories: i64 = args[4].parse().unwrap();
    assert!(!cache.exists(), "the cache must not exist");
    std::fs::create_dir_all(&cache).unwrap();
    let cache = std::fs::canonicalize(&cache).unwrap();
    let paths = IndexPaths {
        cache: cache.join("published"),
        locks: cache.join("support"),
        staging: cache.join("support"),
        journal: cache.join("support"),
    };
    let counter = Arc::new(Counter::default());
    let ready = Arc::new(Mutex::new(None::<Instant>));
    let ready_hook = ready.clone();
    let options = SessionOptions {
        checkpoint: Some(ChangeCheckpoint {
            root: root.clone(),
            identity: "pipeline-indexing-benchmark".into(),
            event_id: 0,
        }),
        reuse_saved_maps: false,
        // Swift keeps the stage after publication; so does this probe (cleanup is not timed).
        retire_after_publish: false,
        prepare_search: false,
        hooks: Some(counter.clone()),
        before_publish: Some(Arc::new(move || {
            ready_hook.lock().get_or_insert_with(Instant::now);
        })),
        ..SessionOptions::default()
    };
    let started = Instant::now();
    let session =
        MappingSession::start(Arc::new(Native::default()), paths, root.as_os_str().as_bytes(), options).expect("start");
    let initialized = Instant::now();
    let snapshot = match session.wait(Duration::from_secs(3600)) {
        SessionOutcome::Published(snapshot) => snapshot,
        other => panic!("mapping failed: {other:?}"),
    };
    let published = Instant::now();
    let report = session.discovery_report().expect("discovery ran");
    let ready = ready.lock().expect("publication hook ran");
    let database = &snapshot.database;
    let actual_files = scalar(database, "SELECT COUNT(*) FROM nodes WHERE kind<>1");
    let actual_directories = scalar(database, "SELECT COUNT(*) FROM nodes WHERE kind=1");
    let root_files = scalar(database, "SELECT file_count FROM nodes WHERE parent IS NULL");
    let pending = scalar(database, "SELECT COUNT(*) FROM dirs WHERE pending=1");
    assert!(
        actual_files == files && actual_directories == directories && root_files == files && pending == 0,
        "verification failed: {actual_files} files, {actual_directories} directories, root {root_files}, {pending} pending"
    );
    let producer = counter.0.load(Ordering::Relaxed);
    assert_eq!(producer as i64, directories);
    let since = |t: Instant| t.duration_since(initialized).as_secs_f64();
    let discovery = report.elapsed.as_secs_f64();
    let result = serde_json::json!({
        "files": files,
        "directories": directories,
        "advances": 0,
        "initialization_seconds": initialized.duration_since(started).as_secs_f64(),
        "discovery_seconds": discovery,
        "writer_loop_seconds": since(ready),
        "post_discovery_drain_seconds": (since(ready) - discovery).max(0.0),
        "mapping_ready_seconds": since(ready),
        "publication_seconds": published.duration_since(ready).as_secs_f64(),
        "total_seconds": published.duration_since(started).as_secs_f64(),
        "counters": { "producer_directories": producer },
        "verified": true,
    });
    println!("{result}");
}
