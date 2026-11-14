//! Discovery into the live inventory only (no index writer), in fresh processes.
//!
//! ```text
//! discovery_probe fixtures --dir DIR
//! discovery_probe bench    --dir DIR [--workers 4,6,8,10] [--samples 5] [--fixtures flat,mixed]
//! discovery_probe sample   --dir DIR --fixture flat --workers 6      (one measured child run)
//! discovery_probe profile  --dir DIR --fixture mixed --workers 4 [--seconds 10]  (repeat, for `sample`)
//! discovery_probe micro                                              (view and search benches)
//! discovery_probe scaling  --dir DIR [--spread same-parent]          (kernel calls alone, 1-8 threads)
//! ```
//!
//! Fixture shapes follow `docs/benchmarks/parallel-discovery.json` (the Swift generator is not in
//! the repository; counts match): `flat` has 60,008 records (root, one folder of 60,000 files,
//! two symlinks, a hard-link pair and two sparse files); `mixed` has 90,068 records (3,662
//! folders, 86,406 files including long, Unicode (NFD) and hard-linked names, symlinks and
//! sparse files). The mailbox has no consumer, like Swift's stopped writer, so pending batches
//! stay resident as they would while the writer catches up.

use orbit_core::discovery::{self, DiscoveryOptions};
use orbit_core::live::LiveInventory;
use orbit_core::mailbox::{DirComponent, ListingBatch, ListingCompletion, Mailbox};
use orbit_core::records::RecordBatch;
use orbit_core::scope::TraversalScope;
use orbit_native::Native;
use orbit_platform::{Cancellation, EntryKind, EntryRecord, FileIdentity, Platform};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("fixtures") => {
            let dir = PathBuf::from(arg(&args, "--dir").expect("--dir"));
            make_fixtures(&dir);
        }
        Some("sample") => {
            let dir = PathBuf::from(arg(&args, "--dir").expect("--dir"));
            let fixture = arg(&args, "--fixture").expect("--fixture");
            let workers: usize = arg(&args, "--workers").map_or(6, |w| w.parse().unwrap());
            sample(&dir, &fixture, workers);
        }
        Some("bench") => bench(&args),
        Some("micro") => micro(),
        Some("profile") => {
            // Repeats discovery in one process for `sample <pid>` / profilers.
            let dir = PathBuf::from(arg(&args, "--dir").expect("--dir"));
            let fixture = arg(&args, "--fixture").expect("--fixture");
            let workers: usize = arg(&args, "--workers").map_or(4, |w| w.parse().unwrap());
            let seconds: f64 = arg(&args, "--seconds").map_or(10.0, |s| s.parse().unwrap());
            let started = Instant::now();
            while started.elapsed().as_secs_f64() < seconds {
                sample(&dir, &fixture, workers);
            }
        }
        Some("scaling") => scaling(&args),
        Some("walk") => {
            let root = PathBuf::from(arg(&args, "--root").expect("--root"));
            let workers: usize = arg(&args, "--workers").map_or(discovery::DEFAULT_WORKERS, |w| w.parse().unwrap());
            walk(&root, workers);
        }
        _ => {
            eprintln!("usage: discovery_probe fixtures|bench|sample|micro --dir DIR ...");
            std::process::exit(2);
        }
    }
}

// --- Fixtures ----------------------------------------------------------------------------

fn write(path: &Path, len: usize) {
    if len == 0 {
        fs::File::create(path).unwrap();
    } else {
        fs::write(path, vec![b'x'; len]).unwrap();
    }
}

fn extras(dir: &Path) {
    write(&dir.join("hl-a"), 4096);
    fs::hard_link(dir.join("hl-a"), dir.join("hl-b")).unwrap();
    fs::File::create(dir.join("sparse-1")).unwrap().set_len(1 << 30).unwrap();
    fs::File::create(dir.join("sparse-2")).unwrap().set_len(4 << 30).unwrap();
    std::os::unix::fs::symlink("hl-a", dir.join("sym-1")).unwrap();
    std::os::unix::fs::symlink("/nonexistent", dir.join("sym-2")).unwrap();
}

fn make_fixtures(dir: &Path) {
    let flat = dir.join("flat");
    if !dir.join("flat.ok").exists() {
        let _ = fs::remove_dir_all(&flat);
        fs::create_dir_all(flat.join("flat")).unwrap();
        for i in 0..60_000 {
            write(&flat.join(format!("flat/f{i:05}.dat")), if i % 20 == 0 { 1 + i % 4096 } else { 0 });
        }
        extras(&flat);
        fs::write(dir.join("flat.manifest"), "records=60008 files=60006\n").unwrap();
        fs::write(dir.join("flat.ok"), "").unwrap();
    }
    let mixed = dir.join("mixed");
    if !dir.join("mixed.ok").exists() {
        let _ = fs::remove_dir_all(&mixed);
        fs::create_dir_all(&mixed).unwrap();
        let long = "l".repeat(200);
        let nfd = "nai\u{308}ve-cafe\u{301}";
        for t in 0..60 {
            for s in 0..60 {
                let leaf = mixed.join(format!("t{t:02}/s{s:02}"));
                fs::create_dir_all(&leaf).unwrap();
                for f in 0..22 {
                    write(&leaf.join(format!("file-{f:02}.bin")), if f == 0 { 100 + t * 60 + s } else { 0 });
                }
                write(&leaf.join(format!("{long}-{s}")), 0);
                write(&leaf.join(format!("{nfd}-{s}")), 0);
            }
        }
        let unicode = mixed.join("unicode-\u{e9}");
        fs::create_dir_all(&unicode).unwrap();
        extras(&unicode);
        fs::write(dir.join("mixed.manifest"), "records=90068 files=86406\n").unwrap();
        fs::write(dir.join("mixed.ok"), "").unwrap();
    }
}

fn manifest(dir: &Path, fixture: &str) -> (u64, u64) {
    let text = fs::read_to_string(dir.join(format!("{fixture}.manifest"))).expect("run `fixtures` first");
    let value = |key: &str| -> u64 {
        text.split_whitespace().find_map(|kv| kv.strip_prefix(key)).and_then(|v| v.parse().ok()).unwrap()
    };
    (value("records="), value("files="))
}

// --- One measured run ----------------------------------------------------------------------

struct Usage {
    user_ms: f64,
    system_ms: f64,
    peak_rss: u64,
}

fn usage() -> Usage {
    // SAFETY: zeroed rusage is valid POD; getrusage writes it.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let ms = |tv: libc::timeval| tv.tv_sec as f64 * 1000.0 + tv.tv_usec as f64 / 1000.0;
    // ru_maxrss is bytes on macOS and KiB on Linux.
    let scale = if cfg!(target_os = "macos") { 1 } else { 1024 };
    Usage { user_ms: ms(ru.ru_utime), system_ms: ms(ru.ru_stime), peak_rss: ru.ru_maxrss as u64 * scale }
}

fn sample(dir: &Path, fixture: &str, workers: usize) {
    let (records, files) = manifest(dir, fixture);
    let root = fs::canonicalize(dir.join(fixture)).unwrap();
    let root_bytes = root.as_os_str().as_bytes().to_vec();
    let setup = Instant::now();
    let platform = Arc::new(Native::new());
    let (_, identity) = platform.open_root(&root).unwrap();
    let scope = TraversalScope::new(&root_bytes, identity, platform.volume_layout(&root, identity));
    let live = Arc::new(LiveInventory::new(scope));
    let cancel = Cancellation::new();
    let mailbox = Mailbox::new(Arc::from(root_bytes.clone()), identity, cancel.clone(), || {}, live.hook());
    let setup_ms = setup.elapsed().as_secs_f64() * 1000.0;
    let before = usage();
    let started = Instant::now();
    let options = DiscoveryOptions { workers, ..DiscoveryOptions::default() };
    let handle = discovery::start(platform, mailbox.clone(), live.clone(), options, cancel).unwrap();
    let report = handle.join();
    let discovery_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after = usage();
    let view_started = Instant::now();
    let view = live.view(&root_bytes, &[]).unwrap();
    let view_ms = view_started.elapsed().as_secs_f64() * 1000.0;
    let totals = live.totals();
    let progress = mailbox.progress();
    let correct = totals.files == files && progress.records == records - 1 && report.failures == 0 && !view.is_listing;
    println!(
        "fixture={fixture} workers={workers} discovery_ms={discovery_ms:.3} setup_ms={setup_ms:.3} view_ms={view_ms:.3} \
         user_ms={:.3} system_ms={:.3} peak_rss={} live_heap={} pending_bytes={} files={} records={} opens={} \
         revalidations={} correct={correct}",
        after.user_ms - before.user_ms,
        after.system_ms - before.system_ms,
        after.peak_rss,
        totals.heap_bytes,
        mailbox.stats().pending_bytes,
        totals.files,
        progress.records,
        report.cursor.opens,
        report.cursor.revalidations,
    );
}

/// Discovery alone (live inventory, no index writer) over a real folder such as `/` or `~`.
fn walk(root: &Path, workers: usize) {
    let root = fs::canonicalize(root).unwrap();
    let root_bytes = root.as_os_str().as_bytes().to_vec();
    let platform = Arc::new(Native::new());
    let (_, identity) = platform.open_root(&root).unwrap();
    let scope = TraversalScope::new(&root_bytes, identity, platform.volume_layout(&root, identity));
    let live = Arc::new(LiveInventory::new(scope));
    let cancel = Cancellation::new();
    let mailbox = Mailbox::new(Arc::from(root_bytes.clone()), identity, cancel.clone(), || {}, live.hook());
    let before = usage();
    let started = Instant::now();
    let options = DiscoveryOptions { workers, ..DiscoveryOptions::default() };
    let handle = discovery::start(platform, mailbox.clone(), live.clone(), options, cancel).unwrap();
    let report = handle.join();
    let discovery_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after = usage();
    let totals = live.totals();
    println!(
        "root={} workers={workers} discovery_ms={discovery_ms:.1} files={} directories={} failures={} \
         files_per_s={:.0} user_ms={:.0} system_ms={:.0} peak_rss_mib={:.0}",
        root.display(),
        totals.files,
        report.directories,
        report.failures,
        totals.files as f64 / (discovery_ms / 1000.0),
        after.user_ms - before.user_ms,
        after.system_ms - before.system_ms,
        after.peak_rss as f64 / 1_048_576.0,
    );
}

// --- Interleaved fresh-process sweep -------------------------------------------------------

fn field(line: &str, key: &str) -> Option<f64> {
    line.split_whitespace().find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')).and_then(|v| v.parse().ok())
}

fn spawn(dir: &Path, fixture: &str, workers: usize) -> String {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["sample", "--dir"])
        .arg(dir)
        .args(["--fixture", fixture, "--workers", &workers.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let line = String::from_utf8(output.stdout).unwrap().trim().to_string();
    assert!(line.contains("correct=true"), "incorrect inventory: {line}");
    line
}

fn stats(values: &mut [f64]) -> (f64, f64, f64) {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let median = if n % 2 == 1 { values[n / 2] } else { (values[n / 2 - 1] + values[n / 2]) / 2.0 };
    (median, values[0], values[n - 1])
}

fn bench(args: &[String]) {
    let dir = PathBuf::from(arg(args, "--dir").expect("--dir"));
    make_fixtures(&dir);
    let workers: Vec<usize> =
        arg(args, "--workers").unwrap_or_else(|| "4,6,8,10".into()).split(',').map(|w| w.parse().unwrap()).collect();
    let samples: usize = arg(args, "--samples").map_or(5, |s| s.parse().unwrap());
    let fixtures: Vec<String> =
        arg(args, "--fixtures").unwrap_or_else(|| "flat,mixed".into()).split(',').map(str::to_string).collect();
    for fixture in &fixtures {
        for &w in &workers {
            spawn(&dir, fixture, w); // warm-up, unmeasured
        }
    }
    let mut lines: BTreeMap<(String, usize), Vec<String>> = BTreeMap::new();
    for round in 0..samples {
        for fixture in &fixtures {
            // Rotate the worker order per round so no count always runs first.
            for i in 0..workers.len() {
                let w = workers[(i + round) % workers.len()];
                lines.entry((fixture.clone(), w)).or_default().push(spawn(&dir, fixture, w));
            }
        }
    }
    println!(
        "| fixture | workers | discovery ms median (min–max) | user ms | system ms | peak RSS MB | live heap MB | pending MB |"
    );
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    for ((fixture, w), runs) in &lines {
        let collect = |key: &str| -> Vec<f64> { runs.iter().map(|l| field(l, key).unwrap()).collect() };
        let (ms, lo, hi) = stats(&mut collect("discovery_ms"));
        let (user, _, _) = stats(&mut collect("user_ms"));
        let (system, _, _) = stats(&mut collect("system_ms"));
        let (rss, _, rss_max) = stats(&mut collect("peak_rss"));
        let (heap, _, _) = stats(&mut collect("live_heap"));
        let (pending, _, _) = stats(&mut collect("pending_bytes"));
        let mb = |b: f64| b / 1_048_576.0;
        println!(
            "| {fixture} | {w} | {ms:.1} ({lo:.1}–{hi:.1}) | {user:.1} | {system:.1} | {:.1} (max {:.1}) | {:.1} | {:.1} |",
            mb(rss),
            mb(rss_max),
            mb(heap),
            mb(pending)
        );
    }
}

// --- In-memory view and search ---------------------------------------------------------------

fn id(ino: u64) -> FileIdentity {
    FileIdentity { dev: 1, ino }
}

fn files_batch(
    root: &[u8],
    chain: &[(&str, u64)],
    names: impl Iterator<Item = (String, u64, u64)>,
    complete: bool,
) -> ListingBatch {
    let mut path = root.to_vec();
    let mut components = Vec::new();
    for (name, ino) in chain {
        path.push(b'/');
        path.extend_from_slice(name.as_bytes());
        components.push(DirComponent { name: name.as_bytes().into(), identity: id(*ino) });
    }
    let parent = chain.last().map_or(id(1), |(_, ino)| id(*ino));
    let mut records = RecordBatch::default();
    for (name, ino, size) in names {
        records.push(&EntryRecord {
            name: name.as_bytes(),
            kind: EntryKind::File,
            id: id(ino),
            nlink: 1,
            size,
            allocated: size.div_ceil(4096) * 4096,
            is_mount_point: false,
        });
    }
    ListingBatch::new(
        Arc::from(path),
        parent,
        Arc::from(components),
        records,
        complete.then(ListingCompletion::default),
        None,
    )
}

fn dirs_batch(root: &[u8], names: &[(String, u64)]) -> ListingBatch {
    let mut records = RecordBatch::default();
    for (name, ino) in names {
        records.push(&EntryRecord {
            name: name.as_bytes(),
            kind: EntryKind::Directory,
            id: id(*ino),
            nlink: 1,
            size: 0,
            allocated: 0,
            is_mount_point: false,
        });
    }
    ListingBatch::new(Arc::from(root), id(1), Arc::from(Vec::new()), records, Some(ListingCompletion::default()), None)
}

fn median_ms(runs: usize, mut f: impl FnMut()) -> (f64, f64, f64) {
    let mut times: Vec<f64> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    stats(&mut times)
}

fn micro() {
    let root: &[u8] = b"/bench";
    // A folder with 100,000 direct children.
    let live = LiveInventory::new(TraversalScope::single_volume(root, id(1)));
    live.append(&dirs_batch(root, &[("big".into(), 2)]));
    let started = Instant::now();
    let mut next = 10u64;
    for chunk in 0..100 {
        let names = (0..1000).map(|i| {
            next += 1;
            (format!("child-{chunk:03}-{i:04}.dat"), next, (next * 2_654_435_761) % 1_000_000)
        });
        live.append(&files_batch(root, &[("big", 2)], names, chunk == 99));
    }
    let append_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (view_ms, view_lo, view_hi) = median_ms(21, || {
        let tree = live.view(b"/bench/big", &[]).unwrap();
        assert_eq!(tree.children(0).len(), 100_000);
    });
    println!(
        "view 100k-child folder: median {view_ms:.2} ms ({view_lo:.2}–{view_hi:.2}); append 100k: {append_ms:.1} ms"
    );

    // One million names across 1,000 folders.
    let live = LiveInventory::new(TraversalScope::single_volume(root, id(1)));
    let dirs: Vec<(String, u64)> = (0..1000).map(|d| (format!("dir-{d:04}"), 100 + d)).collect();
    live.append(&dirs_batch(root, &dirs));
    let started = Instant::now();
    let mut next = 10_000u64;
    for (name, ino) in &dirs {
        let names = (0..1000).map(|i| {
            next += 1;
            let label = if i % 1000 == 7 { "R\u{e9}sum\u{e9}" } else { "report" };
            (format!("{label}-{ino}-{i:04}.txt"), next, (next * 2_654_435_761) % 10_000_000)
        });
        live.append(&files_batch(root, &[(name.as_str(), *ino)], names, true));
    }
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;
    let heap = live.totals().heap_bytes as f64 / 1_048_576.0;
    println!("build 1M-name inventory: {build_ms:.1} ms, live heap {heap:.1} MiB");
    for query in ["resume", "report-500-0999", "txt", "no-such-name"] {
        let folded = orbit_core::fold::fold_query(query);
        let mut total = 0;
        let (ms, lo, hi) = median_ms(7, || {
            let (_, count) = live.search(&folded, 100, &[]);
            total = count;
        });
        println!("search {query:>16}: {total:>8} matches, median {ms:.1} ms ({lo:.1}–{hi:.1})");
    }
}

struct Count;

impl orbit_platform::ListingSink for Count {
    fn batch(&mut self, _records: &[EntryRecord<'_>]) -> Result<(), orbit_platform::Stop> {
        Ok(())
    }

    fn issue(&mut self, _issue: orbit_platform::Issue) {}
}

/// Raw platform scaling on `mixed`: each kernel operation discovery performs, alone, on T threads
/// over disjoint slices of the 3,600 leaf folders (best of three passes). Parents and leaves are
/// opened before timing, so `list` measures enumeration only and `open_child` measures one
/// relative open + identity check + close. `--spread same-parent` interleaves the slices so
/// concurrent threads work in the same parent folder.
fn scaling(args: &[String]) {
    use orbit_platform::DirectoryLister;
    let dir = PathBuf::from(arg(args, "--dir").expect("--dir"));
    let interleave = arg(args, "--spread").as_deref() == Some("same-parent");
    let root = fs::canonicalize(dir.join("mixed")).unwrap();
    let platform = Native::new();
    let mut parents = Vec::new();
    let mut leaves = Vec::new();
    for t in 0..60 {
        let parent = root.join(format!("t{t:02}"));
        parents.push(platform.open_root(&parent).unwrap().0);
        for s in 0..60 {
            let path = parent.join(format!("s{s:02}"));
            let (handle, identity) = platform.open_root(&path).unwrap();
            let name = format!("s{s:02}").into_bytes();
            leaves.push((t, name, identity, handle, path.as_os_str().as_bytes().to_vec()));
        }
    }
    let threads_list = [1, 2, 4, 8];
    println!(
        "operation (3,600 leaf folders, {}) | {}",
        if interleave { "same parent" } else { "split parents" },
        threads_list.map(|t| format!("{t} thread(s)")).join(" | ")
    );
    for mode in ["open_child", "child_identity", "list", "open_child+list", "open_root (path)"] {
        let mut cells = Vec::new();
        for threads in threads_list {
            let mut best = f64::MAX;
            for _ in 0..3 {
                let started = Instant::now();
                std::thread::scope(|scope| {
                    for t in 0..threads {
                        let (leaves, parents, platform) = (&leaves, &parents, &platform);
                        scope.spawn(move || {
                            let mut lister = platform.new_lister();
                            let cancel = Cancellation::new();
                            let n = leaves.len();
                            let mine: Vec<usize> = if interleave {
                                (t..n).step_by(threads).collect()
                            } else {
                                (t * n / threads..(t + 1) * n / threads).collect()
                            };
                            for index in mine {
                                let (parent, name, identity, handle, path) = &leaves[index];
                                let parent = &parents[*parent];
                                match mode {
                                    "open_child" => drop(platform.open_child(parent, name, *identity).unwrap()),
                                    "child_identity" => {
                                        assert_eq!(platform.child_identity(parent, name).unwrap(), *identity)
                                    }
                                    "list" => drop(lister.list(handle, path, &cancel, &mut Count).unwrap()),
                                    "open_child+list" => {
                                        let opened = platform.open_child(parent, name, *identity).unwrap();
                                        lister.list(&opened, path, &cancel, &mut Count).unwrap();
                                    }
                                    _ => {
                                        drop(platform.open_root(Path::new(std::ffi::OsStr::from_bytes(path))).unwrap())
                                    }
                                }
                            }
                        });
                    }
                });
                best = best.min(started.elapsed().as_secs_f64() * 1000.0);
            }
            cells.push(format!("{best:.1} ms"));
        }
        println!("{mode} | {}", cells.join(" | "));
    }
}
