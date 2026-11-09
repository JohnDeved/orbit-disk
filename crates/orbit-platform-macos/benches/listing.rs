//! Single-threaded full-tree listing: `getattrlistbulk` vs the `readdir` + `fstatat` fallback.
//!
//! Fixtures are built once per run in a temp dir: a flat folder of 60,000 files and a mixed tree
//! of 90,000 files across nested folders (with hard links, symlinks, empty and sparse files).
//! Criterion reports entries/second; compare with `docs/benchmarks/parallel-discovery.json`.

#[cfg(target_os = "macos")]
mod bench {
    use std::os::fd::OwnedFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::time::Duration;

    use criterion::{Criterion, Throughput};
    use orbit_platform::{
        Cancellation, DirectoryLister, EntryRecord, FileIdentity, Issue, ListingSink, Platform, Stop, posix,
    };
    use orbit_platform_macos::{MacLister, MacPlatform};

    const FLAT_FILES: usize = 60_000;
    const MIXED_DIRS: usize = 300;
    const MIXED_FILES_PER_DIR: usize = 300;

    fn build_flat(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        for index in 0..FLAT_FILES {
            std::fs::write(root.join(format!("file-{index:06}.dat")), [index as u8; 3]).unwrap();
        }
    }

    fn build_mixed(root: &Path) {
        for dir_index in 0..MIXED_DIRS {
            // Three levels: group/sub/leaf so traversal opens many directories.
            let dir = root.join(format!("g{:02}/s{:02}/leaf-{dir_index:03}", dir_index % 10, dir_index % 7));
            std::fs::create_dir_all(&dir).unwrap();
            for index in 0..MIXED_FILES_PER_DIR {
                let name = format!("item {index:04} — ünïcode.bin");
                match index % 100 {
                    0 => std::os::unix::fs::symlink("../elsewhere", dir.join(&name)).unwrap(),
                    1 if index > 1 => {
                        std::fs::hard_link(dir.join(format!("item {:04} — ünïcode.bin", 2)), dir.join(&name))
                            .unwrap_or_else(|_| std::fs::write(dir.join(&name), b"x").unwrap())
                    }
                    2 => std::fs::File::create(dir.join(&name)).unwrap().set_len(1 << 20).unwrap(),
                    3 => std::fs::write(dir.join(&name), b"").unwrap(),
                    _ => std::fs::write(dir.join(&name), vec![1u8; index % 512]).unwrap(),
                }
            }
        }
    }

    struct Counter<'a> {
        entries: u64,
        subdirs: &'a mut Vec<(Vec<u8>, FileIdentity)>,
    }

    impl ListingSink for Counter<'_> {
        fn batch(&mut self, records: &[EntryRecord<'_>]) -> Result<(), Stop> {
            self.entries += records.len() as u64;
            for record in records {
                if record.is_dir() && !record.is_mount_point {
                    self.subdirs.push((record.name.to_vec(), record.id));
                }
            }
            Ok(())
        }

        fn issue(&mut self, _issue: Issue) {}
    }

    /// Lists every directory below `root` depth-first; returns the number of entries seen.
    fn walk(platform: &MacPlatform, lister: &mut MacLister, root: &Path) -> u64 {
        let cancel = Cancellation::new();
        let (dir, _) = platform.open_root(root).unwrap();
        let mut stack: Vec<(OwnedFd, Vec<u8>)> = vec![(dir, root.as_os_str().as_bytes().to_vec())];
        let mut subdirs = Vec::new();
        let mut entries = 0;
        while let Some((dir, path)) = stack.pop() {
            subdirs.clear();
            let mut sink = Counter { entries: 0, subdirs: &mut subdirs };
            lister.list(&dir, &path, &cancel, &mut sink).unwrap();
            entries += sink.entries;
            for (name, id) in subdirs.drain(..) {
                let child = platform.open_child(&dir, &name, id).unwrap();
                stack.push((child, posix::join(&path, &name)));
            }
        }
        entries
    }

    pub fn listing(c: &mut Criterion) {
        let temp = tempfile::tempdir().unwrap();
        let flat = temp.path().join("flat");
        let mixed = temp.path().join("mixed");
        build_flat(&flat);
        build_mixed(&mixed);
        let platform = MacPlatform::new();
        for (label, root) in [("flat-60k", &flat), ("mixed-90k", &mixed)] {
            let total = walk(&platform, &mut platform.new_lister(), root);
            assert_eq!(total, walk(&platform, &mut platform.new_fallback_lister(), root));
            let mut group = c.benchmark_group(format!("listing/{label}"));
            group.throughput(Throughput::Elements(total));
            group.sample_size(20).measurement_time(Duration::from_secs(8)).warm_up_time(Duration::from_secs(1));
            let mut bulk = platform.new_lister();
            group.bench_function("getattrlistbulk", |b| b.iter(|| walk(&platform, &mut bulk, root)));
            let mut fallback = platform.new_fallback_lister();
            group.bench_function("readdir+fstatat", |b| b.iter(|| walk(&platform, &mut fallback, root)));
            group.finish();
        }
    }
}

#[cfg(target_os = "macos")]
criterion::criterion_group!(benches, bench::listing);
#[cfg(target_os = "macos")]
criterion::criterion_main!(benches);

#[cfg(not(target_os = "macos"))]
fn main() {}
