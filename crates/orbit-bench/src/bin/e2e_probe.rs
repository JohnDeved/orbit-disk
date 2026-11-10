//! A whole mapping session on a generated tree shaped like a user's Documents folder, measured
//! like `docs/benchmarks/async-inventory-hot-paths.json` (first visible view, discovery, total
//! to publication, peak RSS, CPU, view latency while mapping).
//!
//! ```text
//! e2e_probe fixture --dir DIR [--files 180000]   (creates DIR/root; sparse/empty files only)
//! e2e_probe rescan  --dir DIR [--workers N] [--fresh]
//!                                                (maps DIR/root once, then times a forced rescan
//!                                                seeded from that saved map, or with --fresh an
//!                                                unseeded one as before; prints JSON)
//! e2e_probe run     --dir DIR [--workers N] [--keep]
//!                                                (one measured session; prints JSON; --keep
//!                                                leaves its cache for inspection)
//! ```
//!
//! Files are empty or sparse, so the fixture costs almost no disk blocks; logical sizes vary.
//! Each run uses a fresh cache under DIR and deletes it afterwards.

#![allow(clippy::default_constructed_unit_structs)] // `Native` is a unit struct only on some OSes.

use orbit_core::session::{MappingSession, SessionOptions, SessionOutcome};
use orbit_core::store::IndexPaths;
use orbit_core::view::FolderTree;
use orbit_native::Native;
use orbit_platform::ChangeCheckpoint;
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
            let files: usize = arg(&args, "--files").map_or(180_000, |n| n.parse().unwrap());
            fixture(&dir.join("root"), files);
        }
        Some("run") => run(&dir, arg(&args, "--workers").map(|n| n.parse().expect("--workers N"))),
        Some("rescan") => rescan(
            &dir,
            arg(&args, "--workers").map(|n| n.parse().expect("--workers N")),
            args.iter().any(|a| a == "--fresh"),
        ),
        _ => {
            eprintln!("usage: e2e_probe fixture|run|rescan --dir DIR");
            std::process::exit(2);
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Mixed depth: project-like folders (deep, many small folders), media-like folders (wide),
/// and archives (one huge folder).
fn fixture(root: &Path, total: usize) {
    std::fs::create_dir_all(root).unwrap();
    let mut rng = Rng(0x0b17_d15c);
    let mut made = 0usize;
    let mut folders = 0usize;
    let started = Instant::now();
    let mut stack: Vec<(PathBuf, u32)> = (0..24).map(|i| (root.join(format!("Area {i:02}")), 0)).collect();
    while made < total {
        let Some((folder, depth)) = stack.pop() else {
            stack.push((root.join(format!("More {folders}")), 0));
            continue;
        };
        std::fs::create_dir_all(&folder).unwrap();
        folders += 1;
        let wide = rng.below(40) == 0;
        let count = if wide { 400 + rng.below(1600) } else { rng.below(24) } as usize;
        for i in 0..count.min(total - made) {
            let name = match rng.below(5) {
                0 => format!("Résumé {i}.pdf"),
                1 => format!("IMG_{:05}.HEIC", rng.below(99_999)),
                2 => format!("notes-{i}.md"),
                3 => format!("data_{i}.bin"),
                _ => format!("file {i}.txt"),
            };
            let file = std::fs::File::create(folder.join(name)).unwrap();
            let size = match rng.below(4) {
                0 => 0,
                1 => rng.below(4_096),
                2 => rng.below(1 << 20),
                _ => rng.below(1 << 27),
            };
            file.set_len(size).unwrap();
            made += 1;
        }
        let children = if depth >= 7 { 0 } else { rng.below(if depth < 2 { 8 } else { 4 }) };
        for c in 0..children {
            stack.push((folder.join(format!("folder {depth}-{c}")), depth + 1));
        }
    }
    eprintln!("fixture: {made} files in {folders} folders, {:.1}s", started.elapsed().as_secs_f64());
}

fn rusage() -> (f64, f64, u64) {
    // SAFETY: zeroed rusage is valid POD; getrusage fills it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    // macOS reports bytes, Linux kibibytes.
    let rss = if cfg!(target_os = "macos") { usage.ru_maxrss as u64 } else { usage.ru_maxrss as u64 * 1024 };
    (seconds(usage.ru_utime), seconds(usage.ru_stime), rss)
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values[((values.len() - 1) as f64 * p).round() as usize]
}

fn run(dir: &Path, workers: Option<usize>) {
    let root = std::fs::canonicalize(dir.join("root")).expect("run `fixture` first");
    let cache = dir.join(format!("cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();
    let paths = IndexPaths::under(&std::fs::canonicalize(&cache).unwrap());
    let root_bytes = root.as_os_str().as_bytes().to_vec();
    // A folder two levels down that the UI could be looking at.
    let first_dir = |dir: &Path| {
        // Readable folders only (a real home has some that are not, such as `.Trash`).
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && std::fs::read_dir(p).is_ok())
            .collect();
        dirs.sort();
        dirs.into_iter().next().unwrap_or_else(|| dir.to_path_buf())
    };
    let deep = first_dir(&first_dir(&root)).as_os_str().as_bytes().to_vec();
    // Like the app: cloud storage under the user's home is excluded (it matters when the
    // measured root is a real home folder).
    let options = SessionOptions {
        checkpoint: Some(ChangeCheckpoint { root: root.clone(), identity: "e2e-benchmark".into(), event_id: 0 }),
        prepare_search: false,
        home: std::env::var_os("HOME").map(std::path::PathBuf::from),
        workers: workers.unwrap_or(orbit_core::discovery::DEFAULT_WORKERS),
        ..SessionOptions::default()
    };
    let started = Instant::now();
    let session =
        MappingSession::start(Arc::new(Native::default()), paths, &root_bytes, options).expect("start mapping");
    let started_ms = started.elapsed().as_secs_f64() * 1e3;
    let mut first_view = None;
    let mut first_full_view = None;
    let mut latencies = Vec::new();
    let mut seen = session.changes().take();
    while matches!(session.outcome(), SessionOutcome::Running) {
        for path in [&root_bytes, &deep] {
            let t = Instant::now();
            let view = session.view(path, &[]).unwrap();
            latencies.push(t.elapsed().as_secs_f64() * 1e3);
            if path == &root_bytes
                && let Some(view) = view
            {
                if first_view.is_none() && !view.children(FolderTree::FOLDER).is_empty() {
                    first_view = Some(started.elapsed().as_secs_f64() * 1e3);
                }
                if first_full_view.is_none() && !view.is_listing && session.progress().discovery_complete {
                    first_full_view = Some(started.elapsed().as_secs_f64() * 1e3);
                }
            }
        }
        // A UI repaints at most once per frame.
        seen = session.changes().wait(seen, Duration::from_millis(16));
        session.changes().take();
    }
    let snapshot = match session.wait(Duration::from_secs(3600)) {
        SessionOutcome::Published(snapshot) => snapshot,
        other => panic!("mapping failed: {other:?}"),
    };
    let total_ms = started.elapsed().as_secs_f64() * 1e3;
    let report = session.discovery_report().expect("discovery ran");
    let files = session.discovered_file_count(&[]).unwrap().unwrap_or(0);
    let (user, system, rss) = rusage();
    let mut sorted = latencies.clone();
    let result = serde_json::json!({
        "files": files,
        "directories": report.directories,
        "snapshot": snapshot.id,
        "start_ms": started_ms,
        "first_visible_view_ms": first_view,
        "first_full_view_ms": first_full_view,
        "discovery_ms": report.elapsed.as_secs_f64() * 1e3,
        "discovery_cancelled": report.cancelled,
        "cache_hits": report.cache_hits,
        "total_to_publish_ms": total_ms,
        "peak_rss_mib": rss as f64 / (1024.0 * 1024.0),
        "user_cpu_s": user,
        "system_cpu_s": system,
        "views_while_mapping": latencies.len(),
        "view_p50_ms": percentile(&mut sorted, 0.5),
        "view_p95_ms": percentile(&mut sorted, 0.95),
        "view_max_ms": percentile(&mut sorted, 1.0),
    });
    println!("{result}");
    drop(session);
    if std::env::args().all(|a| a != "--keep") {
        let _ = std::fs::remove_dir_all(&cache);
    } else {
        eprintln!("kept cache {}", cache.display());
    }
}

/// Maps `DIR/root`, then times a forced rescan of it (seeded from the saved map unless `fresh`).
fn rescan(dir: &Path, workers: Option<usize>, fresh: bool) {
    let root = std::fs::canonicalize(dir.join("root")).expect("DIR/root");
    let cache = dir.join(format!("cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();
    let paths = IndexPaths::under(&std::fs::canonicalize(&cache).unwrap());
    let root_bytes = root.as_os_str().as_bytes().to_vec();
    let options = |event_id: u64| SessionOptions {
        checkpoint: Some(ChangeCheckpoint { root: root.clone(), identity: "e2e-benchmark".into(), event_id }),
        prepare_search: false,
        reuse_saved_maps: false,
        home: std::env::var_os("HOME").map(std::path::PathBuf::from),
        workers: workers.unwrap_or(orbit_core::discovery::DEFAULT_WORKERS),
        ..SessionOptions::default()
    };
    let platform = Arc::new(Native::default());
    let publish = |options: SessionOptions| {
        let started = Instant::now();
        let session = MappingSession::start(Arc::clone(&platform), paths.clone(), &root_bytes, options).expect("start");
        let mut discovery = None;
        loop {
            match session.outcome() {
                SessionOutcome::Running => {
                    if discovery.is_none() && session.progress().discovery_complete {
                        discovery = Some(started.elapsed());
                    }
                    session.changes().wait(session.revision(), Duration::from_millis(20));
                }
                SessionOutcome::Published(snapshot) => {
                    return (snapshot, discovery.unwrap_or_else(|| started.elapsed()), started.elapsed());
                }
                other => panic!("mapping failed: {other:?}"),
            }
        }
    };
    let (saved, _, first) = publish(options(1));
    let seed = (!fresh).then(|| saved.clone());
    let (cpu_user, cpu_system) = cpu_times();
    let (rescanned, discovery, total) = publish(SessionOptions { seed, ..options(2) });
    let (user, system) = cpu_times();
    let result = serde_json::json!({
        "seeded": !fresh,
        "first_map_ms": first.as_secs_f64() * 1e3,
        "rescan_discovery_ms": discovery.as_secs_f64() * 1e3,
        "rescan_total_ms": total.as_secs_f64() * 1e3,
        "rescan_user_cpu_s": user - cpu_user,
        "rescan_system_cpu_s": system - cpu_system,
        "map_bytes": std::fs::metadata(&rescanned.database).map_or(0, |m| m.len()),
        "usage": orbit_bench::rss_and_cpu(),
    });
    println!("{result}");
    let _ = std::fs::remove_dir_all(&cache);
}

/// User and system CPU seconds of this process.
fn cpu_times() -> (f64, f64) {
    // SAFETY: zeroed rusage is valid POD; getrusage writes it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    (seconds(usage.ru_utime), seconds(usage.ru_stime))
}
