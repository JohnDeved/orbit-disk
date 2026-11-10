//! Full-refresh marking probe, ported from the Swift `MarkingProbe` (tag `v0.9.15`).
//!
//! `marking_probe output-dir directory-count rounds`
//!
//! Registers `directory-count` folders below a virtual root (setup, untimed), then per round
//! clears every pending bit and times only `mark_full_refresh` plus the chunked keyset marking.
//! Each round verifies the exact new generation and pending-directory count. Prints one JSON
//! object (sorted keys) and writes `result.json`.

#![allow(clippy::default_constructed_unit_structs)] // `Native` is a unit struct only on some OSes.

use orbit_bench::create_output;
use orbit_core::records::RecordBatch;
use orbit_core::stage::{ListingCursor, Opened, Stage, StageCheckpoint, StageConfig};
use orbit_core::store::IndexPaths;
use orbit_platform::{Cancellation, EntryKind, EntryRecord, FileIdentity, Platform};
use serde_json::json;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("marking_probe: {message}");
    std::process::exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        fail("usage: marking_probe output-dir directory-count rounds");
    }
    let directory = PathBuf::from(&args[1]);
    let count: usize = args[2].parse().unwrap_or_else(|_| fail("directory-count"));
    let rounds: usize = args[3].parse().unwrap_or_else(|_| fail("rounds"));
    create_output(&directory).unwrap_or_else(|e| fail(e));
    let directory = directory.canonicalize().unwrap_or_else(|e| fail(e));
    let root = directory.join("virtual-root");
    std::fs::create_dir(&root).unwrap_or_else(|e| fail(e));
    let root_bytes = root.as_os_str().as_bytes().to_vec();
    let platform = Arc::new(orbit_native::Native::default());
    let (_, root_identity) = platform.open_root(&root).unwrap_or_else(|e| fail(e));
    let paths = IndexPaths {
        cache: directory.join("published"),
        locks: directory.join("locks"),
        staging: directory.join("staging"),
        journal: directory.join("locks"),
    };
    let checkpoint =
        StageCheckpoint { root_path: root_bytes.clone(), identity: "synthetic-marking-benchmark".into(), event_id: 0 };
    let cancel = Cancellation::new();
    let mut stage = match Stage::open(platform, paths, &root_bytes, checkpoint, cancel.clone(), StageConfig::default())
    {
        Ok(Opened::Stage(stage)) => stage,
        other => fail(format!("open: {other:?}")),
    };
    let database = stage.database().to_path_buf();
    let writer = stage.writer();
    writer.prepare_queue_indexes(&cancel).unwrap_or_else(|e| fail(e));
    let setup_start = Instant::now();
    let mut cursor = ListingCursor::default();
    for start in (0..count).step_by(1024) {
        let mut batch = RecordBatch::default();
        for index in start..(start + 1024).min(count) {
            let name = format!("d-{index}");
            batch.push(&EntryRecord {
                name: name.as_bytes(),
                kind: EntryKind::Directory,
                id: FileIdentity { dev: root_identity.dev, ino: index as u64 + 1_000 },
                nlink: 1,
                size: 0,
                allocated: 0,
                is_mount_point: false,
            });
        }
        writer.append(&root_bytes, &mut cursor, &[&batch], None).unwrap_or_else(|e| fail(e));
    }
    let finished = writer.finish(&root_bytes, &mut cursor, 0, Some(0), 0, &[], None).unwrap_or_else(|e| fail(e));
    if !finished {
        fail("setup listing did not finish");
    }
    while writer.trim_one_chunk().unwrap_or_else(|e| fail(e)) {}
    let setup_seconds = setup_start.elapsed().as_secs_f64();
    let side = rusqlite::Connection::open(&database).unwrap_or_else(|e| fail(e));
    side.busy_timeout(std::time::Duration::from_secs(5)).unwrap_or_else(|e| fail(e));
    let mut observations = Vec::new();
    for round in 1..=rounds {
        side.execute("UPDATE dirs SET pending=0", []).unwrap_or_else(|e| fail(e));
        let started = Instant::now();
        writer.mark_full_refresh().unwrap_or_else(|e| fail(e));
        let mut chunks = 0;
        while writer.mark_full_refresh_chunk().unwrap_or_else(|e| fail(e)) {
            chunks += 1;
        }
        let elapsed = started.elapsed().as_secs_f64();
        let generation = writer.full_refresh_state().unwrap_or_else(|e| fail(e)).0;
        let verified: i64 = side
            .query_row("SELECT COUNT(*) FROM dirs WHERE pending=1 AND full_generation=?1", [generation], |row| {
                row.get(0)
            })
            .unwrap_or_else(|e| fail(e));
        if verified != count as i64 + 1 {
            fail(format!("round {round}: {verified} marked, expected {}", count + 1));
        }
        observations.push(json!({
            "round": round, "seconds": elapsed, "chunks": chunks,
            "generation": generation, "verified_directories": verified,
        }));
    }
    let result = json!({
        "directories": count + 1,
        "setup_seconds": setup_seconds,
        "marking": observations,
        "implementation": "rust",
    });
    let text = serde_json::to_string(&result).expect("json");
    std::fs::write(directory.join("result.json"), &text).unwrap_or_else(|e| fail(e));
    println!("{text}");
}
