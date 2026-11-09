//! Discovery against real files through the native platform layer.

use super::cache::{CachedListing, ListingCache, ListingCacheReader};
use super::*;
use crate::live::LiveInventory;
use crate::mailbox::Mailbox;
use crate::view::FolderTree;
use orbit_native::Native;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Barrier;
use unicode_normalization::UnicodeNormalization;

fn bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

fn write(path: &Path, len: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7u8; len]).unwrap();
}

struct Run {
    live: Arc<LiveInventory>,
    mailbox: Arc<Mailbox>,
    report: DiscoveryReport,
}

fn setup(root: &Path) -> (Arc<Native>, Arc<LiveInventory>, Arc<Mailbox>, Cancellation) {
    let platform = Arc::new(Native::new());
    let (_, identity) = platform.open_root(root).unwrap();
    let layout = platform.volume_layout(root, identity);
    let scope = TraversalScope::new(&bytes(root), identity, layout);
    let live = Arc::new(LiveInventory::new(scope));
    let cancel = Cancellation::new();
    let mailbox = Mailbox::new(Arc::from(bytes(root)), identity, cancel.clone(), || {}, live.hook());
    (platform, live, mailbox, cancel)
}

fn run(root: &Path, options: DiscoveryOptions) -> Run {
    let (platform, live, mailbox, cancel) = setup(root);
    let handle = start(platform, mailbox.clone(), live.clone(), options, cancel).unwrap();
    let report = handle.join();
    Run { live, mailbox, report }
}

fn workers(n: usize) -> DiscoveryOptions {
    DiscoveryOptions { workers: n, ..DiscoveryOptions::default() }
}

fn tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap().join("r");
    fs::create_dir(&root).unwrap();
    (dir, root)
}

/// Every node of the live inventory, gathered through folder views.
fn live_nodes(live: &LiveInventory) -> BTreeMap<Vec<u8>, crate::view::NodeMeta> {
    let mut out = BTreeMap::new();
    let mut stack = vec![live.root_path().to_vec()];
    while let Some(path) = stack.pop() {
        let tree = live.view(&path, &[]).unwrap();
        for &c in tree.children(FolderTree::FOLDER) {
            let child = tree.path(c);
            let meta = tree.node(c).meta;
            if meta.flags.is_dir {
                stack.push(child.clone());
            }
            out.insert(child, meta);
        }
    }
    out
}

#[derive(Debug)]
struct Reference {
    entries: BTreeMap<Vec<u8>, fs::Metadata>,
    logical: u64,
    allocated: u64,
    files: u64,
}

/// Independent `lstat` walk; hard-link allocation counted once.
fn reference(root: &Path) -> Reference {
    let mut entries = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path.clone());
            }
            entries.insert(bytes(&path), meta);
        }
    }
    let (mut logical, mut allocated, mut files) = (0, 0, 0);
    let mut seen = std::collections::HashSet::new();
    for meta in entries.values().filter(|m| !m.is_dir()) {
        logical += meta.len();
        files += 1;
        if meta.nlink() < 2 || seen.insert((meta.dev(), meta.ino())) {
            allocated += meta.blocks() * 512;
        }
    }
    Reference { entries, logical, allocated, files }
}

fn build_fixture(root: &Path) {
    write(&root.join("a/small.txt"), 10);
    for i in 0..300 {
        write(&root.join(format!("a/wide/f{i:04}")), i % 7);
    }
    write(&root.join("a/hl-source"), 10_000);
    fs::create_dir_all(root.join("b")).unwrap();
    fs::create_dir_all(root.join("c/deeper")).unwrap();
    fs::hard_link(root.join("a/hl-source"), root.join("b/hl-second")).unwrap();
    fs::hard_link(root.join("a/hl-source"), root.join("c/deeper/hl-third")).unwrap();
    std::os::unix::fs::symlink(root.join("a"), root.join("sym-dir")).unwrap();
    std::os::unix::fs::symlink(root.join("a/small.txt"), root.join("sym-file")).unwrap();
    std::os::unix::fs::symlink("sym-loop", root.join("sym-loop")).unwrap();
    std::os::unix::fs::symlink("/nonexistent/target", root.join("broken")).unwrap();
    write(&root.join("nested/x/y/z/deep.txt"), 3);
    fs::create_dir_all(root.join("empty")).unwrap();
    let sparse = fs::File::create(root.join("sparse")).unwrap();
    sparse.set_len(10 << 20).unwrap();
    let nfd: String = "café-ñ".nfd().collect();
    write(&root.join("unicode").join(&nfd), 4);
    write(&root.join("unicode/Ωmega ✓"), 5);
    if cfg!(target_os = "linux") {
        // Linux allows both spellings as siblings; APFS treats them as one name.
        let nfc: String = "café-ñ".nfc().collect();
        write(&root.join("unicode").join(&nfc), 6);
        write(&root.join("unicode").join(std::ffi::OsStr::from_bytes(b"bad-\xff")), 1);
    }
}

#[test]
fn discovered_tree_matches_reference_walk() {
    let (_guard, root) = tempdir();
    build_fixture(&root);
    let expected = reference(&root);
    for n in [1, 8] {
        let run = run(&root, workers(n));
        assert_eq!(run.report.failures, 0);
        let nodes = live_nodes(&run.live);
        assert_eq!(
            nodes.keys().map(|k| String::from_utf8_lossy(k).into_owned()).collect::<Vec<_>>(),
            expected.entries.keys().map(|k| String::from_utf8_lossy(k).into_owned()).collect::<Vec<_>>()
        );
        for (path, meta) in &nodes {
            let reference = &expected.entries[path];
            assert_eq!(meta.flags.is_dir, reference.is_dir(), "{}", String::from_utf8_lossy(path));
            assert_eq!(meta.flags.is_symlink, reference.file_type().is_symlink());
            assert_eq!(meta.identity.unwrap().ino, reference.ino());
            if !reference.is_dir() {
                assert_eq!(meta.size, reference.len(), "{}", String::from_utf8_lossy(path));
                assert_eq!(meta.raw_allocated, reference.blocks() * 512, "{}", String::from_utf8_lossy(path));
            }
        }
        // Symlinked directories are leaves: nothing below sym-dir was discovered.
        assert!(nodes.keys().all(|k| !k.starts_with(&bytes(&root.join("sym-dir/")))));
        let totals = run.live.totals();
        assert_eq!(
            (totals.logical, totals.allocated, totals.files),
            (expected.logical, expected.allocated, expected.files)
        );
        assert_eq!(totals.open_directories, 0);
        // Exactly the first link in depth-first path order owns the allocation.
        let links = ["a/hl-source", "b/hl-second", "c/deeper/hl-third"];
        let owners: Vec<bool> = links.iter().map(|l| nodes[&bytes(&root.join(l))].allocated > 0).collect();
        assert_eq!(owners, [true, false, false]);
        assert!(run.mailbox.is_finished());
        let progress = run.mailbox.progress();
        assert_eq!(progress.files, expected.files);
        assert!(!progress.uncertain);
    }
}

#[test]
fn mailbox_receives_parents_before_children_in_contiguous_runs() {
    let (_guard, root) = tempdir();
    build_fixture(&root);
    for i in 0..40 {
        write(&root.join(format!("tree/{i}/{}/leaf", i % 3)), 1);
    }
    let run = run(&root, workers(8));
    let snapshot = run.mailbox.capture();
    let mut ranges: HashMap<Vec<u8>, (u64, u64, bool)> = HashMap::new();
    let mut previous: Option<Vec<u8>> = None;
    for batch in snapshot.batches_after(0) {
        let path = batch.parent_path.to_vec();
        let entry = ranges.entry(path.clone()).or_insert((batch.sequence, batch.sequence, false));
        assert!(!entry.2, "batches of {} are not contiguous", String::from_utf8_lossy(&path));
        entry.1 = batch.sequence;
        if batch.completion.is_some() {
            entry.2 = true;
        }
        if let Some(prev) = previous.replace(path.clone())
            && prev != path
        {
            assert!(ranges[&prev].2, "{} was interrupted", String::from_utf8_lossy(&prev));
        }
    }
    let root_bytes = bytes(&root);
    for (path, (first, _, complete)) in &ranges {
        assert!(complete);
        if *path != root_bytes {
            let parent = crate::paths::parent(path).unwrap();
            assert!(ranges[parent].1 < *first, "{} before its parent", String::from_utf8_lossy(path));
        }
    }
    let directories = reference(&root).entries.values().filter(|m| m.is_dir()).count() + 1;
    assert_eq!(ranges.len(), directories);
}

#[derive(Default)]
struct Recorder {
    order: Mutex<Vec<Vec<u8>>>,
}

impl DiscoveryHooks for Recorder {
    fn before_directory(&self, path: &[u8], _ticket: u64) {
        self.order.lock().push(path.to_vec());
    }
}

#[test]
fn preferred_path_is_listed_first() {
    let (_guard, root) = tempdir();
    for top in ["a", "b", "c", "d", "e", "f"] {
        for i in 0..3 {
            write(&root.join(format!("{top}/sub{i}/sub{i}/file")), 1);
        }
    }
    let recorder = Arc::new(Recorder::default());
    let preferred = bytes(&root.join("e/sub1/sub1"));
    let options = DiscoveryOptions {
        workers: 1,
        preferred: Some(preferred.clone()),
        hooks: Some(recorder.clone()),
        ..DiscoveryOptions::default()
    };
    let run = run(&root, options);
    let order = recorder.order.lock().clone();
    let expected: Vec<Vec<u8>> = ["", "e", "e/sub1", "e/sub1/sub1"]
        .iter()
        .map(|p| if p.is_empty() { bytes(&root) } else { bytes(&root.join(p)) })
        .collect();
    assert_eq!(order[..4], expected[..]);
    assert_eq!(order.len(), 1 + 6 * 7);
    assert_eq!(run.report.skipped, 4, "queued copies of the claimed way are skipped");
    assert_eq!(run.live.totals().files, 18);
}

struct Replace {
    target: Vec<u8>,
    action: Box<dyn Fn() + Send + Sync>,
    done: AtomicBool,
}

impl DiscoveryHooks for Replace {
    fn before_directory(&self, path: &[u8], _ticket: u64) {
        if path == self.target && !self.done.swap(true, Ordering::SeqCst) {
            (self.action)();
        }
    }
}

#[test]
fn directory_replaced_between_parent_and_child_listing_is_dropped() {
    let (_guard, root) = tempdir();
    write(&root.join("a/b/old-leaf"), 47);
    write(&root.join("a/c/kept"), 5);
    let (a, b) = (root.join("a"), root.join("a/b"));
    let hook = Arc::new(Replace {
        target: bytes(&b),
        action: Box::new(move || {
            fs::rename(a.join("b"), a.join("b-moved")).unwrap();
            write(&a.join("b/new-leaf"), 59);
        }),
        done: AtomicBool::new(false),
    });
    let run = run(&root, DiscoveryOptions { workers: 1, hooks: Some(hook), ..DiscoveryOptions::default() });
    assert_eq!(run.report.failures, 1);
    assert!(run.live.node(&bytes(&b)).is_none(), "the replaced directory is removed, not shown stale");
    assert_eq!(run.live.search("old-leaf", 10, &[]).1, 0);
    assert_eq!(run.live.search("new-leaf", 10, &[]).1, 0, "the replacement is left to the retry pass");
    assert_eq!(run.live.search("kept", 10, &[]).1, 1);
    assert_eq!(run.live.totals().files, 1);
    let progress = run.mailbox.progress();
    assert!(progress.uncertain);
    assert_eq!(run.mailbox.stats().retry_deferred, 1);
    let failed: Vec<_> = run
        .mailbox
        .capture()
        .batches_after(0)
        .filter(|b| b.failure.is_some())
        .map(|b| b.parent_path.to_vec())
        .collect();
    assert_eq!(failed, [bytes(&b)]);
}

#[test]
fn cached_ancestor_replacement_is_detected() {
    let (_guard, root) = tempdir();
    write(&root.join("p/q/r1/x"), 1);
    write(&root.join("p/q/r2/y"), 1);
    struct Second {
        seen: AtomicUsize,
        root: std::path::PathBuf,
    }
    impl DiscoveryHooks for Second {
        fn before_directory(&self, path: &[u8], _ticket: u64) {
            let prefix = bytes(&self.root.join("p/q/"));
            if path.starts_with(&prefix) && self.seen.fetch_add(1, Ordering::SeqCst) == 1 {
                // The worker holds cached descriptors for p and q; p moves away.
                fs::rename(self.root.join("p"), self.root.join("p-moved")).unwrap();
                fs::create_dir_all(self.root.join("p/q")).unwrap();
            }
        }
    }
    let hook = Arc::new(Second { seen: AtomicUsize::new(0), root: root.clone() });
    let run = run(&root, DiscoveryOptions { workers: 1, hooks: Some(hook), ..DiscoveryOptions::default() });
    assert_eq!(run.report.failures, 1);
    assert!(run.report.cursor.revalidations > 0);
    assert_eq!(run.live.totals().files, 1, "only the listing before the move is kept");
}

#[test]
fn cancellation_fences_live_and_durable_publication() {
    let (_guard, root) = tempdir();
    for i in 0..60 {
        write(&root.join(format!("d{i}/f")), 1);
    }
    // Every worker that reaches ticket 10 or later parks until the test has cancelled, so the
    // number of listed folders is bounded no matter how the scheduler interleaves workers.
    struct Gate {
        reached: Barrier,
        released: Mutex<bool>,
        wake: Condvar,
    }
    impl DiscoveryHooks for Gate {
        fn before_directory(&self, _path: &[u8], ticket: u64) {
            if ticket == 10 {
                self.reached.wait();
            }
            if ticket >= 10 {
                let mut released = self.released.lock();
                while !*released {
                    self.wake.wait(&mut released);
                }
            }
        }
    }
    let gate = Arc::new(Gate { reached: Barrier::new(2), released: Mutex::new(false), wake: Condvar::new() });
    let (platform, live, mailbox, cancel) = setup(&root);
    let options = DiscoveryOptions { workers: 4, hooks: Some(gate.clone()), ..DiscoveryOptions::default() };
    let handle = start(platform, mailbox.clone(), live.clone(), options, cancel).unwrap();
    gate.reached.wait();
    handle.cancel();
    let revision = live.revision();
    *gate.released.lock() = true;
    gate.wake.notify_all();
    let report = handle.join();
    assert!(report.cancelled);
    assert_eq!(live.revision(), revision, "nothing reached the live inventory after cancel");
    assert!(mailbox.is_cancelled());
    assert_eq!(mailbox.capture().batches_after(0).count(), 0);
    assert!(report.directories < 60);
}

struct Duplicate {
    target: Vec<u8>,
    chunk: usize,
    seen: Mutex<(usize, Vec<u8>)>,
}

impl DiscoveryHooks for Duplicate {
    fn records(&self, path: &[u8], records: &mut RecordBatch) {
        if path != self.target {
            return;
        }
        let mut seen = self.seen.lock();
        if seen.0 == 0 {
            seen.1 = records.name(0).to_vec();
        }
        if seen.0 == self.chunk {
            let (kind, id, nlink, size, allocated) = {
                let first = records.get(0);
                (first.kind, first.id, first.nlink, first.size, first.allocated)
            };
            let name = seen.1.clone();
            records.push(&EntryRecord { name: &name, kind, id, nlink, size, allocated, is_mount_point: false });
        }
        seen.0 += 1;
    }
}

#[test]
fn duplicate_names_reject_the_listing_before_exposure() {
    for (chunk, exposed) in [(0, 0), (2, 128)] {
        let (_guard, root) = tempdir();
        for i in 0..384 {
            write(&root.join(format!("suspect/dup-{i:03}")), 1);
        }
        for i in 0..17 {
            write(&root.join(format!("unrelated/retained-{i}")), 1);
        }
        let hook =
            Arc::new(Duplicate { target: bytes(&root.join("suspect")), chunk, seen: Mutex::new((0, Vec::new())) });
        let run = run(&root, DiscoveryOptions { workers: 2, hooks: Some(hook), ..DiscoveryOptions::default() });
        assert_eq!(run.report.failures, 1);
        assert_eq!(run.mailbox.stats().retry_deferred, 1);
        assert_eq!(run.live.search("dup-", 1000, &[]).1, exposed);
        assert_eq!(run.live.search("retained-", 100, &[]).1, 17);
        let durable: usize = run
            .mailbox
            .capture()
            .batches_after(0)
            .filter(|b| *b.parent_path == bytes(&root.join("suspect"))[..])
            .map(|b| b.records.len())
            .sum();
        assert_eq!(durable, exposed);
    }
}

#[test]
fn unreadable_folder_is_inaccessible_and_does_not_stop_discovery() {
    let (_guard, root) = tempdir();
    write(&root.join("locked/secret"), 1);
    write(&root.join("open/visible"), 2);
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = fs::read_dir(root.join("locked")).is_err();
    let run = run(&root, workers(2));
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    if !enforced {
        return; // running with privileges that bypass permissions
    }
    let locked = run.live.node(&bytes(&root.join("locked"))).unwrap();
    assert!(!locked.flags.accessible);
    assert_eq!(locked.state, crate::types::ScanState::Complete);
    assert_eq!(run.live.totals().files, 1);
    assert_eq!(run.report.failures, 1);
    assert!(!run.live.view(&bytes(&root), &[]).unwrap().is_listing);
}

#[test]
fn cloud_and_cache_directories_are_excluded_by_path_and_identity() {
    let (_guard, root) = tempdir();
    write(&root.join("Library/CloudStorage/Drive/doc"), 100);
    write(&root.join("Library/CloudStorage-old/kept"), 1);
    write(&root.join("Library/Mobile Documents/icloud"), 100);
    write(&root.join("cache/Indexes/map"), 100);
    write(&root.join("local"), 1);
    let platform = Native::new();
    let layout = platform.volume_layout(&root, platform.open_root(&root).unwrap().1);
    let mut exclusions =
        Exclusions::resolve(&platform, &[root.join("Library/CloudStorage"), root.join("cache/./Indexes/")], &layout);
    // Reached under another spelling: only the identity matches.
    let icloud = platform.open_root(&root.join("Library/Mobile Documents")).unwrap().1;
    exclusions.insert(b"/elsewhere/Mobile Documents".to_vec(), Some(icloud));
    let run = run(&root, DiscoveryOptions { workers: 4, exclusions, ..DiscoveryOptions::default() });
    for hidden in ["doc", "icloud", "map", "CloudStorage", "Mobile", "Indexes"] {
        let query = crate::fold::fold_query(hidden);
        let (hits, _) = run.live.search(&query, 10, &[]);
        assert!(hits.iter().all(|h| !h.path.ends_with(hidden.as_bytes())), "{hidden} was discovered");
    }
    assert_eq!(run.live.totals().files, 2);
    assert!(run.live.node(&bytes(&root.join("Library/CloudStorage-old/kept"))).is_some());
    for batch in run.mailbox.capture().batches_after(0) {
        assert!(batch.records.iter().all(|r| r.name != b"CloudStorage" && r.name != b"Indexes"));
    }
}

struct FakeCache {
    path: Vec<u8>,
    dirty: Vec<u8>,
}

struct FakeReader {
    path: Vec<u8>,
    dirty: Vec<u8>,
}

impl ListingCache for FakeCache {
    fn open_reader(&self) -> Option<Box<dyn ListingCacheReader>> {
        Some(Box::new(FakeReader { path: self.path.clone(), dirty: self.dirty.clone() }))
    }
}

impl ListingCacheReader for FakeReader {
    fn generation(&self) -> i64 {
        3
    }

    fn listing(&mut self, path: &[u8], identity: FileIdentity) -> Option<CachedListing> {
        if path == self.dirty {
            return Some(CachedListing { records: None, revision: 7, generation: 3, import: false });
        }
        if path != self.path {
            return None;
        }
        let mut records = RecordBatch::default();
        records.push(&EntryRecord {
            name: b"from-cache",
            kind: EntryKind::File,
            id: FileIdentity { dev: identity.dev, ino: 1 << 40 },
            nlink: 1,
            size: 1234,
            allocated: 4096,
            is_mount_point: false,
        });
        Some(CachedListing { records: Some(records), revision: 0, generation: 3, import: false })
    }
}

#[test]
fn completed_cached_listings_are_reused_without_relisting() {
    let (_guard, root) = tempdir();
    write(&root.join("cached/on-disk"), 1);
    write(&root.join("dirty/on-disk-too"), 1);
    let cache = Arc::new(FakeCache { path: bytes(&root.join("cached")), dirty: bytes(&root.join("dirty")) });
    let run = run(&root, DiscoveryOptions { workers: 2, cache: Some(cache), ..DiscoveryOptions::default() });
    assert_eq!(run.report.cache_hits, 1);
    assert_eq!(run.live.search("from-cache", 10, &[]).1, 1);
    assert_eq!(run.live.search("on-disk", 10, &[]).1, 1, "only the dirty folder was listed");
    let batches: Vec<_> = run.mailbox.capture().batches_after(0).cloned().collect();
    assert!(
        batches.iter().all(|b| *b.parent_path != bytes(&root.join("cached"))[..]),
        "cached listings are not re-saved"
    );
    let dirty = batches.iter().find(|b| *b.parent_path == bytes(&root.join("dirty"))[..]).unwrap();
    assert_eq!((dirty.expected_revision, dirty.expected_full_generation), (7, 3));
}

#[test]
fn a_replaced_root_is_rejected() {
    let (_guard, root) = tempdir();
    let (platform, live, mailbox, cancel) = setup(&root);
    fs::rename(&root, root.with_file_name("r-old")).unwrap();
    fs::create_dir(&root).unwrap();
    let error = start(platform, mailbox, live, workers(2), cancel).unwrap_err();
    assert!(matches!(error, CoreError::Platform(PlatformError::IdentityChanged { .. })), "{error:?}");
}

#[test]
fn live_views_stay_consistent_while_discovery_runs() {
    let (_guard, root) = tempdir();
    for i in 0..300 {
        for j in 0..10 {
            write(&root.join(format!("d{:02}/e{i}/f{j}", i % 30)), j + 1);
        }
    }
    let (platform, live, mailbox, cancel) = setup(&root);
    let handle = start(platform, mailbox, live.clone(), workers(8), cancel).unwrap();
    let root_bytes = bytes(&root);
    let mut views = 0;
    let mut last = 0;
    loop {
        let finished = handle.is_finished();
        let tree = live.view(&root_bytes, &[]).unwrap();
        assert!(tree.revision >= last);
        last = tree.revision;
        let kids = tree.children(FolderTree::FOLDER);
        let folder = tree.node(FolderTree::FOLDER).meta;
        assert_eq!(folder.size, kids.iter().map(|&c| tree.node(c).meta.size).sum::<u64>());
        assert_eq!(folder.file_count, kids.iter().map(|&c| tree.node(c).meta.file_count).sum::<u64>());
        assert_eq!(folder.allocated, kids.iter().map(|&c| tree.node(c).meta.allocated).sum::<u64>());
        views += 1;
        if finished {
            assert!(!tree.is_listing);
            break;
        }
    }
    handle.join();
    assert!(views > 0);
    assert_eq!(live.totals().files, 3000);
}
