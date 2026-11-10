//! Background-refresh cost of a saved map: one small changed folder in a large map.
//!
//! ```text
//! refresh_probe fixture --dir DIR [--files 93000]     creates DIR/root (empty/sparse files) and
//!                                                     prints the deep folder a writer can touch
//! refresh_probe run     --dir DIR [--iterations 20]   maps DIR/root into a fresh cache, then
//!                                                     times SavedMaps::refresh of the deep folder
//!                                                     after a real change (a file grows) and
//!                                                     after no change (same listing), then a
//!                                                     file in `Area 00` and one in the root, then
//!                                                     reopening (journal recovery); prints JSON
//! refresh_probe writer  --dir DIR [--interval-ms 200] [--seconds 0]
//!                                                     appends to a file in the deep folder every
//!                                                     interval (forever with --seconds 0)
//! ```
//!
//! The deep folder is `DIR/root/Area 00/deep 1/deep 2/deep 3/deep 4/deep 5` (12 files). Each run
//! uses a fresh cache under DIR and deletes it afterwards.

#![allow(clippy::default_constructed_unit_structs)] // `Native` is a unit struct only on some OSes.

use orbit_core::saved::SavedMaps;
use orbit_core::schema::SnapshotInfo;
use orbit_core::session::{MappingSession, SessionOptions, SessionOutcome};
use orbit_core::store::IndexPaths;
use orbit_native::Native;
use orbit_platform::{Cancellation, ChangeCheckpoint};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(arg(&args, "--dir").expect("--dir"));
    match args.get(1).map(String::as_str) {
        Some("fixture") => {
            let files: usize = arg(&args, "--files").map_or(93_000, |n| n.parse().unwrap());
            fixture(&dir.join("root"), files);
        }
        Some("run") => {
            let iterations: usize = arg(&args, "--iterations").map_or(20, |n| n.parse().unwrap());
            run(&dir, iterations);
        }
        Some("writer") => {
            let interval: u64 = arg(&args, "--interval-ms").map_or(200, |n| n.parse().unwrap());
            let seconds: u64 = arg(&args, "--seconds").map_or(0, |n| n.parse().unwrap());
            writer(&dir, Duration::from_millis(interval), seconds);
        }
        _ => {
            eprintln!("usage: refresh_probe fixture|run|writer --dir DIR");
            std::process::exit(2);
        }
    }
}

fn deep_folder(root: &Path) -> PathBuf {
    let mut path = root.join("Area 00");
    for level in 1..=5 {
        path = path.join(format!("deep {level}"));
    }
    path
}

/// `files` files in up to 24 areas of nested folders (30 files per folder, 341 folders per area),
/// plus the deep folder with 12 files.
fn fixture(root: &Path, total: usize) {
    std::fs::create_dir_all(root).unwrap();
    let started = Instant::now();
    let deep = deep_folder(root);
    std::fs::create_dir_all(&deep).unwrap();
    for i in 0..12 {
        std::fs::File::create(deep.join(format!("item {i}.log"))).unwrap().set_len(1_000 + i).unwrap();
    }
    let mut made = 12usize;
    let mut folders = 0usize;
    let mut state = 0x5eed_u64;
    let mut next = || {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        state >> 33
    };
    'outer: for area in 0..24 {
        let mut stack = vec![(root.join(format!("Area {area:02}")), 0u32)];
        while let Some((folder, depth)) = stack.pop() {
            std::fs::create_dir_all(&folder).unwrap();
            folders += 1;
            for i in 0..30 {
                if made >= total {
                    break 'outer;
                }
                let file = std::fs::File::create(folder.join(format!("file {i}.dat"))).unwrap();
                file.set_len(next() % (1 << 22)).unwrap();
                made += 1;
            }
            if depth < 4 {
                for c in 0..4 {
                    stack.push((folder.join(format!("folder {depth}-{c}")), depth + 1));
                }
            }
        }
    }
    eprintln!("fixture: {made} files in {folders} folders, {:.1}s", started.elapsed().as_secs_f64());
    println!("{}", deep.display());
}

fn writer(dir: &Path, interval: Duration, seconds: u64) {
    let target = deep_folder(&dir.join("root")).join("item 0.log");
    let deadline = (seconds > 0).then(|| Instant::now() + Duration::from_secs(seconds));
    loop {
        let mut file = std::fs::OpenOptions::new().append(true).create(true).open(&target).unwrap();
        file.write_all(b"orbit refresh probe line\n").unwrap();
        drop(file);
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return;
        }
        std::thread::sleep(interval);
    }
}

fn append(target: &Path) {
    let mut file = std::fs::OpenOptions::new().append(true).create(true).open(target).unwrap();
    file.write_all(b"orbit refresh probe line\n").unwrap();
}

/// User + system CPU seconds of this process.
fn cpu_seconds() -> f64 {
    // SAFETY: zeroed rusage is valid POD; getrusage writes it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

/// Wall and CPU milliseconds of `body`.
fn timed<T>(wall: &mut Vec<f64>, cpu: &mut Vec<f64>, body: impl FnOnce() -> T) -> T {
    let (started, cpu_started) = (Instant::now(), cpu_seconds());
    let value = body();
    wall.push(started.elapsed().as_secs_f64() * 1e3);
    cpu.push((cpu_seconds() - cpu_started) * 1e3);
    value
}

fn percentile(values: &[f64], p: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sorted.get(((sorted.len().max(1) - 1) as f64 * p).round() as usize).copied().unwrap_or(0.0)
}

fn summary(values: &[f64]) -> serde_json::Value {
    serde_json::json!({
        "p50_ms": percentile(values, 0.5),
        "p90_ms": percentile(values, 0.9),
        "max_ms": percentile(values, 1.0),
    })
}

fn run(dir: &Path, iterations: usize) {
    let root = std::fs::canonicalize(dir.join("root")).expect("run `fixture` first");
    let cache = dir.join(format!("cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();
    let paths = IndexPaths::under(&std::fs::canonicalize(&cache).unwrap());
    let root_bytes = root.as_os_str().as_bytes().to_vec();
    let platform = Arc::new(Native::default());
    let options = SessionOptions {
        checkpoint: Some(ChangeCheckpoint { root: root.clone(), identity: "refresh-probe".into(), event_id: 0 }),
        prepare_search: false,
        ..SessionOptions::default()
    };
    let mapped = Instant::now();
    let session =
        MappingSession::start(Arc::clone(&platform), paths.clone(), &root_bytes, options).expect("start mapping");
    let mut snapshot: SnapshotInfo = match session.wait(Duration::from_secs(3600)) {
        SessionOutcome::Published(snapshot) => snapshot,
        other => panic!("mapping failed: {other:?}"),
    };
    let files = session.discovered_file_count(&[]).unwrap().unwrap_or(0);
    drop(session);
    let mapped_ms = mapped.elapsed().as_secs_f64() * 1e3;
    let maps = SavedMaps::new(platform, paths);
    let cancel = Cancellation::new();
    let deep = deep_folder(&root);
    let deep_bytes = deep.as_os_str().as_bytes().to_vec();
    let target = deep.join("item 0.log");
    let (mut changed, mut changed_cpu) = (Vec::new(), Vec::new());
    let (mut unchanged, mut unchanged_cpu) = (Vec::new(), Vec::new());
    let (mut search, mut search_cpu) = (Vec::new(), Vec::new());
    let mut kept = 0usize;
    let mut event_id = 0u64;
    let mut checkpoint = || {
        event_id += 1;
        ChangeCheckpoint { root: root.clone(), identity: "refresh-probe".into(), event_id }
    };
    for _ in 0..iterations {
        append(&target);
        let next = checkpoint();
        snapshot = timed(&mut changed, &mut changed_cpu, || {
            maps.refresh(&snapshot, std::slice::from_ref(&deep_bytes), &[], Some(&next), &cancel).unwrap()
        });
        let next = checkpoint();
        let before = snapshot.id.clone();
        snapshot = timed(&mut unchanged, &mut unchanged_cpu, || {
            maps.refresh(&snapshot, std::slice::from_ref(&deep_bytes), &[], Some(&next), &cancel).unwrap()
        });
        kept += usize::from(snapshot.id == before);
        timed(&mut search, &mut search_cpu, || maps.prepare_search(&snapshot, &cancel).unwrap());
    }
    // A file changed in a shallow folder with a large subtree (like a home folder), and one changed
    // directly in the mapped root. The monitor reports the changed file's parent folder.
    let mut shallow = |folder: &Path, wall: &mut Vec<f64>, cpu: &mut Vec<f64>| {
        let target = folder.join("probe change.log");
        let folder = folder.as_os_str().as_bytes().to_vec();
        for _ in 0..iterations {
            append(&target);
            let next = checkpoint();
            snapshot = timed(wall, cpu, || {
                maps.refresh(&snapshot, std::slice::from_ref(&folder), &[], Some(&next), &cancel).unwrap()
            });
        }
    };
    let (mut area, mut area_cpu) = (Vec::new(), Vec::new());
    shallow(&root.join("Area 00"), &mut area, &mut area_cpu);
    let (mut top, mut top_cpu) = (Vec::new(), Vec::new());
    shallow(&root, &mut top, &mut top_cpu);
    let _ = std::fs::remove_file(root.join("Area 00/probe change.log"));
    let _ = std::fs::remove_file(root.join("probe change.log"));
    // Reopening a saved map whose removal journal is already current.
    let (mut reopen, mut reopen_cpu) = (Vec::new(), Vec::new());
    for _ in 0..iterations {
        let (_, published) = timed(&mut reopen, &mut reopen_cpu, || maps.recover(&snapshot, &cancel).unwrap());
        assert!(published.is_none(), "a current journal must not republish the map");
    }
    let size = std::fs::metadata(&snapshot.database).map_or(0, |m| m.len());
    let result = serde_json::json!({
        "files": files,
        "map_ms": mapped_ms,
        "database_bytes": size,
        "iterations": iterations,
        "refresh_changed": summary(&changed),
        "refresh_changed_cpu": summary(&changed_cpu),
        "refresh_unchanged": summary(&unchanged),
        "refresh_unchanged_cpu": summary(&unchanged_cpu),
        "unchanged_refreshes_kept_the_map": kept,
        "prepare_search_after_refresh": summary(&search),
        "prepare_search_after_refresh_cpu": summary(&search_cpu),
        "refresh_area_file": summary(&area),
        "refresh_area_file_cpu": summary(&area_cpu),
        "refresh_root_file": summary(&top),
        "refresh_root_file_cpu": summary(&top_cpu),
        "reopen_current_journal": summary(&reopen),
        "reopen_current_journal_cpu": summary(&reopen_cpu),
        "usage": orbit_bench::rss_and_cpu(),
    });
    println!("{result}");
    drop(maps);
    let _ = std::fs::remove_dir_all(&cache);
}
