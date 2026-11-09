//! Durable-indexing probe, ported from `Benchmarks/DurableIndexing/IndexingProbe.swift`.
//!
//! `indexing_probe output-dir shape files directories generation`
//!
//! Builds listing metadata in RAM before timing, then writes it through the production stage
//! writer with the same transaction shapes as the asynchronous writer: the first 128 records of a
//! listing, then 1,024-record batches, grouped fresh-leaf commits (≤32 small folders), fresh
//! small-folder completions, trim and link reconciliation, then publication. Shapes: `flat`
//! (one folder, `directories` = 0), `small` and `empty` (`directories` folders below the root);
//! generation 1 runs full-refresh marking before ingestion. Exact totals, drained queues and
//! `PRAGMA quick_check` are required. Prints one JSON object (sorted keys) and writes
//! `result.json`, compatible with `scripts/benchmarks/run-durable-indexing.py`.

#![allow(clippy::default_constructed_unit_structs)] // `Native` is a unit struct only on some OSes.

use orbit_bench::{Timings, create_output, file_bytes, rss_and_cpu, wal_state};
use orbit_core::publish::PublishOptions;
use orbit_core::records::RecordBatch;
use orbit_core::stage::{Completion, FreshLeaf, ListingCursor, Opened, Stage, StageCheckpoint, StageConfig, Watermark};
use orbit_core::store::IndexPaths;
use orbit_platform::{Cancellation, EntryKind, EntryRecord, FileIdentity, Platform};
use serde_json::json;
use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

struct Input {
    path: Vec<u8>,
    batches: Vec<RecordBatch>,
    records: usize,
}

fn chunks(records: Vec<(Vec<u8>, EntryKind, FileIdentity, u64, u64)>) -> Vec<RecordBatch> {
    let to_batch = |slice: &[(Vec<u8>, EntryKind, FileIdentity, u64, u64)]| {
        let mut batch = RecordBatch::with_capacity(slice.len(), slice.iter().map(|r| r.0.len()).sum());
        for (name, kind, id, size, allocated) in slice {
            batch.push(&EntryRecord {
                name,
                kind: *kind,
                id: *id,
                nlink: 1,
                size: *size,
                allocated: *allocated,
                is_mount_point: false,
            });
        }
        batch
    };
    if records.len() < 128 {
        return vec![to_batch(&records)];
    }
    let mut result = vec![to_batch(&records[..128])];
    let mut offset = 128;
    while offset < records.len() {
        let end = (offset + 1024).min(records.len());
        result.push(to_batch(&records[offset..end]));
        offset = end;
    }
    result
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("indexing_probe: {message}");
    std::process::exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        fail("usage: indexing_probe output-dir shape files directories generation");
    }
    let directory = PathBuf::from(&args[1]);
    let shape = args[2].as_str();
    let file_count: usize = args[3].parse().unwrap_or_else(|_| fail("files"));
    let directory_count: usize = args[4].parse().unwrap_or_else(|_| fail("directories"));
    let generation: i64 = args[5].parse().unwrap_or_else(|_| fail("generation"));
    let profiled = args[6..].iter().any(|a| a == "profile");
    if !(generation == 0 || generation == 1)
        || !["flat", "small", "empty"].contains(&shape)
        || (shape == "flat") != (directory_count == 0)
    {
        fail("invalid fixture");
    }
    create_output(&directory).unwrap_or_else(|e| fail(e));
    let directory = directory.canonicalize().unwrap_or_else(|e| fail(e));
    let root = directory.join("virtual-root");
    std::fs::create_dir(&root).unwrap_or_else(|e| fail(e));
    let root_bytes = root.as_os_str().as_bytes().to_vec();
    let platform = Arc::new(orbit_native::Native::default());
    let (_, root_identity) = platform.open_root(&root).unwrap_or_else(|e| fail(e));
    let device = root_identity.dev;

    let mut timings = Timings::default();
    let fixture_start = Instant::now();
    let (mut expected_logical, mut expected_allocated) = (0u64, 0u64);
    let mut files = |start: usize, count: usize| {
        (start..start + count)
            .map(|index| {
                let size = (1 + index as u64 % 32) * 1024;
                let allocated = if index % 31 == 0 { 0 } else { size.div_ceil(4096) * 4096 };
                expected_logical += size;
                expected_allocated += allocated;
                let name = if index % 19 == 0 { format!("résumé-{index}.bin") } else { format!("file-{index}.bin") };
                let id = FileIdentity { dev: device, ino: index as u64 + 1_000_000 };
                (name.into_bytes(), EntryKind::File, id, size, allocated)
            })
            .collect::<Vec<_>>()
    };
    let mut inputs = Vec::new();
    if shape == "flat" {
        inputs.push(Input { path: root_bytes.clone(), batches: chunks(files(0, file_count)), records: file_count });
    } else {
        let directories = (0..directory_count)
            .map(|index| {
                let id = FileIdentity { dev: device, ino: index as u64 + 100 };
                (format!("d-{index}").into_bytes(), EntryKind::Directory, id, 0, 0)
            })
            .collect();
        inputs.push(Input { path: root_bytes.clone(), batches: chunks(directories), records: directory_count });
        let mut offset = 0;
        for index in 0..directory_count {
            let count = file_count / directory_count + usize::from(index < file_count % directory_count);
            let mut path = root_bytes.clone();
            path.extend_from_slice(format!("/d-{index}").as_bytes());
            inputs.push(Input { path, batches: chunks(files(offset, count)), records: count });
            offset += count;
        }
    }
    let fixture_seconds = fixture_start.elapsed().as_secs_f64();

    let paths = IndexPaths {
        cache: directory.join("published"),
        locks: directory.join("locks"),
        staging: directory.join("staging"),
    };
    let checkpoint =
        StageCheckpoint { root_path: root_bytes.clone(), identity: "synthetic-indexing-benchmark".into(), event_id: 0 };
    let cancel = Cancellation::new();
    let opened = timings.measure("initialization", || {
        Stage::open(platform.clone(), paths, &root_bytes, checkpoint, cancel.clone(), StageConfig::default())
    });
    let mut stage = match opened {
        Ok(Opened::Stage(stage)) => stage,
        other => fail(format!("open: {other:?}")),
    };
    let database = stage.lease().database_path().to_path_buf();
    timings.measure("queue_indexes", || stage.writer().prepare_queue_indexes(&cancel)).unwrap_or_else(|e| fail(e));
    if generation != 0 {
        timings
            .measure("generation_setup", || -> orbit_core::Result<()> {
                stage.writer().mark_full_refresh()?;
                while stage.writer().mark_full_refresh_chunk()? {}
                Ok(())
            })
            .unwrap_or_else(|e| fail(e));
    }

    let resource_before = rss_and_cpu();
    let ingestion_start = Instant::now();
    let token = uuid::Uuid::new_v4();
    let (mut sequence, mut fresh_success, mut fresh_reject, mut finish_reject) = (0u64, 0u64, 0u64, 0u64);
    let (mut maximum_wal, mut maximum_wal_state) = (0i64, BTreeMap::new());
    let sample_wal = |maximum_wal: &mut i64, state_out: &mut BTreeMap<String, i64>| {
        let state = wal_state(&database);
        let bytes = state.get("file_bytes").copied().unwrap_or(0);
        if bytes > *maximum_wal {
            *maximum_wal = bytes;
            *state_out = state;
        }
    };
    let (mut index, mut grouped) = (0usize, 0u64);
    let writer_result = (|| -> orbit_core::Result<()> {
        while index < inputs.len() {
            let input = &inputs[index];
            if index > 0 {
                let mut group = Vec::new();
                let (mut group_records, mut group_bytes) = (0, 0);
                #[allow(clippy::needless_range_loop)] // the index is also the identity seed.
                for leaf_index in index..(index + 32).min(inputs.len()) {
                    let leaf = &inputs[leaf_index];
                    if leaf.batches.len() != 1
                        || leaf.records >= 128
                        || leaf.batches[0].metas().iter().any(|m| m.kind == EntryKind::Directory)
                        || group_records + leaf.records > 1024
                    {
                        break;
                    }
                    let listing = FreshLeaf {
                        path: &leaf.path,
                        identity: FileIdentity { dev: device, ino: (leaf_index - 1) as u64 + 100 },
                        records: &leaf.batches[0],
                        expected_revision: 0,
                        expected_generation: generation,
                        issue_count: 0,
                        issues: &[],
                        watermark: Watermark { generation: token, sequence: sequence + group.len() as u64 + 1 },
                    };
                    if group_bytes + listing.byte_count() > 256 * 1024 {
                        break;
                    }
                    group_records += leaf.records;
                    group_bytes += listing.byte_count();
                    group.push(listing);
                }
                if group.len() > 1 {
                    let committed = timings
                        .measure("fresh_group", || stage.writer().complete_fresh_leaf_listings(&group, &cancel))?;
                    if committed > 0 {
                        sequence += committed as u64;
                        fresh_success += committed as u64;
                        grouped += committed as u64;
                        timings.measure("links", || -> orbit_core::Result<()> {
                            while stage.writer().reconcile_links_chunk(Some(&cancel))? {}
                            Ok(())
                        })?;
                        if sequence / 128 > (sequence - committed as u64) / 128 || index + committed == inputs.len() {
                            sample_wal(&mut maximum_wal, &mut maximum_wal_state);
                        }
                        timings.measure("trim", || -> orbit_core::Result<()> {
                            while stage.writer().trim_one_chunk()? {}
                            Ok(())
                        })?;
                        index += committed;
                        continue;
                    }
                }
            }
            let mut cursor = ListingCursor::default();
            let mut completed = false;
            let batches = input.batches.len();
            for (batch_index, batch) in input.batches.iter().enumerate() {
                let last = batch_index == batches - 1;
                sequence += 1;
                let commit = Watermark { generation: token, sequence };
                if last && cursor.epoch.is_none() && batch.len() < 128 {
                    let completion = Completion { revision: 0, generation, issue_count: 0, issues: &[] };
                    completed = timings.measure("fresh_listing", || {
                        stage.writer().complete_fresh_listing(&input.path, &[batch], completion, Some(commit))
                    })?;
                    if completed {
                        fresh_success += 1;
                    } else {
                        fresh_reject += 1;
                    }
                }
                if !completed {
                    if !batch.is_empty() || !last {
                        timings.measure("append", || {
                            stage.writer().append(&input.path, &mut cursor, &[batch], Some(commit))
                        })?;
                    }
                    if last {
                        completed = timings.measure("finish", || {
                            stage.writer().finish(&input.path, &mut cursor, 0, Some(generation), 0, &[], Some(commit))
                        })?;
                        if !completed {
                            finish_reject += 1;
                        }
                    }
                }
                timings.measure("links", || -> orbit_core::Result<()> {
                    while stage.writer().reconcile_links_chunk(Some(&cancel))? {}
                    Ok(())
                })?;
                if sequence % 128 == 0 || (index == inputs.len() - 1 && last) {
                    sample_wal(&mut maximum_wal, &mut maximum_wal_state);
                }
                if last {
                    timings.measure("trim", || -> orbit_core::Result<()> {
                        while stage.writer().trim_one_chunk()? {}
                        Ok(())
                    })?;
                }
            }
            index += 1;
        }
        Ok(())
    })();
    writer_result.unwrap_or_else(|e| fail(e));
    let ingestion_seconds = ingestion_start.elapsed().as_secs_f64();
    let resource_after = rss_and_cpu();
    let wal_after_ingestion = wal_state(&database);

    let publication_start = Instant::now();
    let mut boundaries = BTreeMap::new();
    let published = timings
        .measure("publication", || {
            let mut boundary = |name: &str| {
                boundaries.insert(name.to_string(), publication_start.elapsed().as_secs_f64());
                orbit_core::publish::BoundaryAction::Continue
            };
            stage.publish(
                ingestion_seconds,
                PublishOptions { boundary: Some(&mut boundary), ..PublishOptions::default() },
            )
        })
        .unwrap_or_else(|e| fail(e));
    let validation = timings
        .measure("verification", || {
            validate(
                &published.database,
                file_count,
                file_count + directory_count + 1,
                expected_logical,
                expected_allocated,
            )
        })
        .unwrap_or_else(|e| fail(e));
    let result = json!({
        "shape": shape,
        "files": file_count,
        "directories": directory_count + 1,
        "generation": generation,
        "profiled": profiled,
        "fixture_seconds": fixture_seconds,
        "ingestion_seconds": ingestion_seconds,
        "records_per_second": (file_count + directory_count) as f64 / ingestion_seconds,
        "phase_seconds": timings.seconds,
        "phase_calls": timings.calls,
        "publication_boundaries_seconds": boundaries,
        "fresh_success": fresh_success,
        "fresh_reject": fresh_reject,
        "finish_reject": finish_reject,
        "grouped_listings": grouped,
        "async_commits": sequence,
        "wal_max_sampled_bytes": maximum_wal,
        "wal_at_maximum": maximum_wal_state,
        "wal_after_ingestion": wal_after_ingestion,
        "database_bytes": file_bytes(&published.database),
        "resource_before": resource_before,
        "resource_after": resource_after,
        "validation": validation,
        "pid": std::process::id(),
        "implementation": "rust",
    });
    let text = serde_json::to_string(&result).expect("json");
    std::fs::write(directory.join("result.json"), &text).unwrap_or_else(|e| fail(e));
    println!("{text}");
}

fn validate(
    database: &std::path::Path,
    files: usize,
    nodes: usize,
    logical: u64,
    allocated: u64,
) -> Result<serde_json::Value, String> {
    let conn = rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let values = |sql: &str, columns: usize| -> Result<Vec<i64>, String> {
        conn.query_row(sql, [], |row| (0..columns).map(|i| row.get::<_, i64>(i)).collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|e| e.to_string())
    };
    let root = values("SELECT file_count,size,allocated FROM nodes WHERE parent IS NULL", 3)?;
    let counts = values("SELECT COUNT(*),SUM(kind<>1) FROM nodes", 2)?;
    let queues = values(
        "SELECT (SELECT COUNT(*) FROM dirs WHERE pending=1)+(SELECT COUNT(*) FROM trim)+(SELECT COUNT(*) FROM dirty_links)",
        1,
    )?;
    if root != [files as i64, logical as i64, allocated as i64]
        || counts != [nodes as i64, files as i64]
        || queues != [0]
    {
        return Err(format!("result mismatch root={root:?} counts={counts:?} queues={queues:?}"));
    }
    let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0)).map_err(|e| e.to_string())?;
    if check != "ok" {
        return Err(format!("database integrity: {check}"));
    }
    Ok(json!({"root_totals": root, "node_counts": counts, "pending_work": 0, "quick_check": "ok"}))
}
