//! End-to-end mapping sessions on real temporary trees with the native platform: views while
//! mapping, publication, resume after a kill, refresh after changes, removal + journal
//! recovery, search parity, concurrent sessions, coverage and saved-map reuse.
#![cfg(unix)]

use orbit_core::discovery::DiscoveryHooks;
use orbit_core::journal::{NoFollowDelete, TrashExecutor};
use orbit_core::removal::{RemovalRequest, remove_with};
use orbit_core::saved::{CoverageSource, SavedMaps};
use orbit_core::session::{MappingSession, SessionOptions, SessionOutcome};
use orbit_core::store::IndexPaths;
use orbit_core::view::FolderTree;
use orbit_core::{CoreError, ScanRemoval};
use orbit_native::Native;
use orbit_platform::{Cancellation, ChangeCheckpoint, FileIdentity, Platform};
use parking_lot::{Condvar, Mutex};
use std::collections::{BTreeMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);

struct Fixture {
    _dir: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    paths: IndexPaths,
    platform: Arc<Native>,
}

fn bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

fn identity(path: &Path) -> FileIdentity {
    let m = std::fs::symlink_metadata(path).unwrap();
    FileIdentity { dev: m.dev(), ino: m.ino() }
}

fn write(path: &Path, size: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; size]).unwrap();
}

impl Fixture {
    #[allow(clippy::default_constructed_unit_structs)] // `Native` is a unit struct only on macOS.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root = base.join("root");
        std::fs::create_dir(&root).unwrap();
        Self { _dir: dir, paths: IndexPaths::under(&base), base, root, platform: Arc::new(Native::default()) }
    }

    /// `folders` folders of `files` files, three levels deep, plus a hard link.
    fn populate(&self, folders: usize, files: usize) {
        for f in 0..folders {
            let dir = self.root.join(format!("d{}", f % 7)).join(format!("e{}", f % 5)).join(format!("f{f}"));
            for i in 0..files {
                write(&dir.join(format!("file-{i}.dat")), 100 + (f * 31 + i * 7) % 900);
            }
        }
        std::fs::hard_link(self.root.join("d0/e0/f0/file-0.dat"), self.root.join("d1/linked.dat")).unwrap();
    }

    fn maps(&self) -> SavedMaps<Native> {
        SavedMaps::new(self.platform.clone(), self.paths.clone())
    }

    fn options(&self, event: u64) -> SessionOptions {
        SessionOptions {
            checkpoint: Some(ChangeCheckpoint {
                root: self.root.clone(),
                identity: "test-history".into(),
                event_id: event,
            }),
            prepare_search: false,
            busy_retry: Duration::ZERO,
            ..SessionOptions::default()
        }
    }

    fn start(&self, options: SessionOptions) -> MappingSession<Native> {
        MappingSession::start(self.platform.clone(), self.paths.clone(), &bytes(&self.root), options).unwrap()
    }

    fn map(&self) -> orbit_core::schema::SnapshotInfo {
        let session = self.start(self.options(1));
        published(&session)
    }
}

fn published(session: &MappingSession<Native>) -> orbit_core::schema::SnapshotInfo {
    match session.wait(WAIT) {
        SessionOutcome::Published(snapshot) => snapshot,
        other => panic!("session did not publish: {other:?}"),
    }
}

/// Files below `dir` (by path) and their logical size, from a plain walk.
fn reference(dir: &Path) -> (u64, u64) {
    let (mut files, mut size) = (0, 0);
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let m = entry.metadata().unwrap();
        if m.is_dir() {
            let (f, s) = reference(&entry.path());
            files += f;
            size += s;
        } else {
            files += 1;
            size += m.len();
        }
    }
    (files, size)
}

/// Every folder of the tree with its (files, size), for comparing a whole map.
fn folders(maps: &SavedMaps<Native>, snapshot: &orbit_core::schema::SnapshotInfo) -> BTreeMap<Vec<u8>, (u64, u64)> {
    let mut out = BTreeMap::new();
    let mut pending = vec![snapshot.root_path.clone()];
    while let Some(path) = pending.pop() {
        let tree = maps.view(snapshot, &path, &[], None).unwrap();
        let meta = tree.node(FolderTree::FOLDER).meta;
        out.insert(path.clone(), (meta.file_count, meta.size));
        for &child in tree.children(FolderTree::FOLDER) {
            if tree.node(child).meta.flags.is_dir {
                pending.push(tree.path(child));
            }
        }
    }
    out
}

fn assert_matches_disk(maps: &SavedMaps<Native>, snapshot: &orbit_core::schema::SnapshotInfo) {
    for (path, (files, size)) in folders(maps, snapshot) {
        let expected = reference(Path::new(std::ffi::OsStr::from_bytes(&path)));
        assert_eq!((files, size), expected, "{}", String::from_utf8_lossy(&path));
    }
}

/// Blocks discovery before the `limit`-th folder until released.
#[derive(Default)]
struct Barrier {
    limit: usize,
    seen: Mutex<usize>,
    released: Mutex<bool>,
    changed: Condvar,
}

impl Barrier {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self { limit, ..Self::default() })
    }

    fn release(&self) {
        *self.released.lock() = true;
        self.changed.notify_all();
    }

    fn wait_reached(&self) {
        let mut seen = self.seen.lock();
        while *seen < self.limit {
            assert!(!self.changed.wait_for(&mut seen, WAIT).timed_out(), "barrier not reached");
        }
    }
}

impl DiscoveryHooks for Barrier {
    fn before_directory(&self, _path: &[u8], _ticket: u64) {
        {
            let mut seen = self.seen.lock();
            *seen += 1;
            self.changed.notify_all();
            if *seen < self.limit {
                return;
            }
        }
        let mut released = self.released.lock();
        while !*released {
            self.changed.wait(&mut released);
        }
    }
}

#[test]
fn views_while_mapping_are_monotone_and_the_saved_map_matches_the_disk() {
    let fx = Fixture::new();
    fx.populate(120, 12);
    let barrier = Barrier::new(40);
    let session = fx.start(SessionOptions { hooks: Some(barrier.clone()), ..fx.options(1) });
    barrier.wait_reached();
    // Discovery is held: what is visible must be internally consistent.
    let root = bytes(&fx.root);
    let held = session.view(&root, &[]).unwrap().expect("root visible while mapping");
    let held_files = held.node(FolderTree::FOLDER).meta.file_count;
    let children: u64 = held.children(FolderTree::FOLDER).iter().map(|&c| held.node(c).meta.size).sum();
    assert_eq!(held.node(FolderTree::FOLDER).meta.size, children);
    assert!(held.is_listing);
    barrier.release();
    let mut last = held_files;
    let mut seen = session.changes().take();
    while matches!(session.outcome(), SessionOutcome::Running) {
        if let Some(view) = session.view(&root, &[]).unwrap() {
            let files = view.node(FolderTree::FOLDER).meta.file_count;
            assert!(files >= last, "file count went backwards: {files} < {last}");
            last = files;
        }
        seen = session.changes().wait(seen, Duration::from_millis(50));
        session.changes().take();
    }
    let snapshot = published(&session);
    let maps = fx.maps();
    assert_matches_disk(&maps, &snapshot);
    let progress = session.progress();
    assert!(progress.discovery_complete);
    assert_eq!(progress.cached_file_count, Some(reference(&fx.root).0));
    // The stage was retired after publication.
    assert!(!orbit_core::lease::has_stage(&fx.paths.staging, &root, session.root_identity()));
}

#[test]
fn a_killed_session_resumes_committed_work() {
    let fx = Fixture::new();
    fx.populate(80, 10);
    let barrier = Barrier::new(30);
    let session = fx.start(SessionOptions { hooks: Some(barrier.clone()), ..fx.options(1) });
    barrier.wait_reached();
    // Let the writer commit what discovery produced so far, then kill the session.
    let deadline = std::time::Instant::now() + WAIT;
    while session.discovery_progress().settled_save_work == 0 {
        assert!(std::time::Instant::now() < deadline, "nothing committed");
        session.changes().wait(session.revision(), Duration::from_millis(20));
    }
    let committed = session.discovery_progress().settled_save_work;
    session.cancel();
    barrier.release();
    drop(session);

    let resumed = fx.start(fx.options(1));
    assert!(resumed.is_resumed());
    let snapshot = published(&resumed);
    let report = resumed.discovery_report().expect("discovery ran");
    assert!(report.cache_hits > 0, "no committed listing was reused ({committed} units committed)");
    assert_matches_disk(&fx.maps(), &snapshot);
}

#[test]
fn a_second_session_on_the_same_root_is_refused() {
    let fx = Fixture::new();
    fx.populate(10, 4);
    let barrier = Barrier::new(2);
    let first = fx.start(SessionOptions { hooks: Some(barrier.clone()), ..fx.options(1) });
    barrier.wait_reached();
    let second = MappingSession::start(fx.platform.clone(), fx.paths.clone(), &bytes(&fx.root), fx.options(1));
    assert!(matches!(second, Err(CoreError::InUse(_))), "{second:?}");
    barrier.release();
    published(&first);
}

#[test]
fn refresh_after_changes_converges_with_the_disk() {
    let fx = Fixture::new();
    fx.populate(30, 6);
    let snapshot = fx.map();
    let maps = fx.maps();
    // Add, grow, delete and replace.
    write(&fx.root.join("d2/e1/new/inner/added.bin"), 5_000);
    write(&fx.root.join("d3/e3/f3/file-0.dat"), 9_999);
    std::fs::remove_file(fx.root.join("d4/e4/f4/file-1.dat")).unwrap();
    std::fs::remove_dir_all(fx.root.join("d5/e0/f5")).unwrap();
    write(&fx.root.join("d5/e0/f5"), 77);
    let changed: Vec<Vec<u8>> =
        ["d2/e1", "d3/e3/f3", "d4/e4/f4", "d5/e0"].iter().map(|p| bytes(&fx.root.join(p))).collect();
    let checkpoint = ChangeCheckpoint { root: fx.root.clone(), identity: "test-history".into(), event_id: 9 };
    let updated = maps.refresh(&snapshot, &[], &changed, Some(&checkpoint), &Cancellation::new()).unwrap();
    assert_ne!(updated.id, snapshot.id);
    assert_eq!(updated.event_id, 9);
    assert_matches_disk(&maps, &updated);
}

/// What a file-level change monitor reports (each changed item's folder, and a changed folder
/// itself) is enough: re-listing only those folders converges with the disk.
#[test]
fn refresh_of_reported_listings_converges_with_the_disk() {
    let fx = Fixture::new();
    fx.populate(30, 6);
    let snapshot = fx.map();
    let maps = fx.maps();
    // A new nested folder, a grown file, a deleted file, a folder replaced by a file, a folder
    // replaced by another folder of the same name, and a file directly in the root.
    write(&fx.root.join("d2/e1/new/inner/added.bin"), 5_000);
    write(&fx.root.join("d3/e3/f3/file-0.dat"), 9_999);
    std::fs::remove_file(fx.root.join("d4/e4/f4/file-1.dat")).unwrap();
    std::fs::remove_dir_all(fx.root.join("d5/e0/f5")).unwrap();
    write(&fx.root.join("d5/e0/f5"), 77);
    std::fs::remove_dir_all(fx.root.join("d6/e1")).unwrap();
    write(&fx.root.join("d6/e1/other/replacement.bin"), 4_444);
    write(&fx.root.join("top.bin"), 1_234);
    let listings: Vec<Vec<u8>> =
        ["d2/e1/new/inner", "d2/e1/new", "d2/e1", "d3/e3/f3", "d4/e4/f4", "d5/e0", "d6/e1/other", "d6/e1", "d6", ""]
            .iter()
            .map(|p| bytes(&fx.root.join(p).components().collect::<PathBuf>()))
            .collect();
    let checkpoint = ChangeCheckpoint { root: fx.root.clone(), identity: "test-history".into(), event_id: 9 };
    let updated = maps.refresh(&snapshot, &listings, &[], Some(&checkpoint), &Cancellation::new()).unwrap();
    assert_ne!(updated.id, snapshot.id);
    assert_eq!(updated.event_id, 9);
    assert_matches_disk(&maps, &updated);
}

/// A listing re-reads that folder alone; a subtree re-reads everything below it.
#[test]
fn a_listing_refresh_does_not_reread_subfolders() {
    let fx = Fixture::new();
    fx.populate(30, 6);
    let snapshot = fx.map();
    let maps = fx.maps();
    let d0 = fx.root.join("d0");
    let deep = d0.join("e0/f0");
    let before = folders(&maps, &snapshot);
    // An unreported change deep below, and a reported one in `d0` itself.
    write(&deep.join("file-2.dat"), 8_888);
    write(&d0.join("top.bin"), 1_000);
    let at = |event_id| ChangeCheckpoint { root: fx.root.clone(), identity: "test-history".into(), event_id };
    let listed = maps.refresh(&snapshot, &[bytes(&d0)], &[], Some(&at(5)), &Cancellation::new()).unwrap();
    let after = folders(&maps, &listed);
    assert_eq!(after[&bytes(&deep)], before[&bytes(&deep)], "the deep folder was re-read");
    let (files, size) = before[&bytes(&d0)];
    assert_eq!(after[&bytes(&d0)], (files + 1, size + 1_000), "d0's own listing was not repeated");
    let reconciled = maps.refresh(&listed, &[], &[bytes(&d0)], Some(&at(6)), &Cancellation::new()).unwrap();
    assert_matches_disk(&maps, &reconciled);
}

#[test]
fn refresh_of_unchanged_folders_keeps_the_saved_map() {
    let fx = Fixture::new();
    fx.populate(30, 6);
    let snapshot = fx.map();
    let maps = fx.maps();
    let folder = fx.root.join("d3/e3/f3");
    let changed = vec![bytes(&folder)];
    let at = |event_id| ChangeCheckpoint { root: fx.root.clone(), identity: "test-history".into(), event_id };
    // A real change publishes.
    write(&folder.join("file-0.dat"), 4_321);
    let first = maps.refresh(&snapshot, &changed, &[], Some(&at(5)), &Cancellation::new()).unwrap();
    assert_ne!(first.id, snapshot.id);
    let published = identity(&first.database);
    // The folder reported again without a real change (rewritten with the same size): the saved map
    // stays as it is, file and all.
    write(&folder.join("file-0.dat"), 4_321);
    let second = maps.refresh(&first, &changed, &[], Some(&at(6)), &Cancellation::new()).unwrap();
    assert_eq!(second, first);
    assert_eq!(identity(&first.database), published, "nothing was published");
    assert_eq!(maps.snapshot(&bytes(&fx.root)).map(|s| s.event_id), Some(5), "the history position lags");
    // The next real change publishes, with the newer position.
    write(&folder.join("file-1.dat"), 1);
    let third = maps.refresh(&second, &changed, &[], Some(&at(7)), &Cancellation::new()).unwrap();
    assert_ne!(third.id, first.id);
    assert_eq!(third.event_id, 7);
    assert_ne!(identity(&third.database), published);
    assert_matches_disk(&maps, &third);
}

fn allow_all(_: &[u8]) -> Option<String> {
    None
}

#[test]
fn removal_hides_items_and_journal_recovery_repairs_the_saved_map() {
    let fx = Fixture::new();
    fx.populate(20, 5);
    let snapshot = fx.map();
    let maps = fx.maps();
    let root = bytes(&fx.root);
    let file = fx.root.join("d1/e1/f1/file-2.dat");
    let folder = fx.root.join("d2/e2/f2");
    let items = vec![
        ScanRemoval { path: bytes(&file), identity: identity(&file) },
        ScanRemoval { path: bytes(&folder), identity: identity(&folder) },
    ];
    let request = RemovalRequest { root: root.clone(), root_identity: identity(&fx.root), items };
    let outcome = remove_with(&*fx.platform, &fx.paths, request, &mut NoFollowDelete, allow_all, &Cancellation::new());
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.removed.len(), 2);
    assert!(!file.exists() && !folder.exists());
    // Until the map catches up, the overlay hides the items and their totals.
    let hidden = maps.view(&snapshot, &root, &outcome.overlay, None).unwrap();
    assert_eq!(hidden.node(FolderTree::FOLDER).meta.file_count, reference(&fx.root).0);
    // Journal recovery repairs the saved map itself.
    let (recovery, repaired) = maps.recover(&snapshot, &Cancellation::new()).unwrap();
    assert!(matches!(recovery, orbit_core::recovery::SavedRecovery::Published(_)), "{recovery:?}");
    let repaired = repaired.expect("published");
    assert_matches_disk(&maps, &repaired);
    // Recovering again finds nothing new.
    let (again, _) = maps.recover(&repaired, &Cancellation::new()).unwrap();
    assert!(matches!(again, orbit_core::recovery::SavedRecovery::UpToDate(_)), "{again:?}");
}

#[test]
fn trash_executor_runs_through_the_journal() {
    let fx = Fixture::new();
    fx.populate(4, 3);
    let trash = fx.base.join("trash");
    std::fs::create_dir(&trash).unwrap();
    let item = fx.root.join("d0/e0/f0");
    let request = RemovalRequest {
        root: bytes(&fx.root),
        root_identity: identity(&fx.root),
        items: vec![ScanRemoval { path: bytes(&item), identity: identity(&item) }],
    };
    let mut executor = TrashExecutor(|path: &Path| {
        let target = trash.join(path.file_name().unwrap());
        std::fs::rename(path, &target)
            .map_err(|e| orbit_platform::PlatformError::os(path, e.raw_os_error().unwrap()))?;
        Ok(Some(target))
    });
    let outcome = remove_with(&*fx.platform, &fx.paths, request, &mut executor, allow_all, &Cancellation::new());
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert!(trash.join("f0").is_dir() && !item.exists());
    assert!(outcome.cursor.is_some() && outcome.changed_parents.contains(&bytes(&fx.root.join("d0/e0"))));
}

#[test]
fn a_removal_during_mapping_is_replayed_before_publication() {
    let fx = Fixture::new();
    fx.populate(40, 6);
    let victim = fx.root.join("d3/e3/f3");
    let victim_identity = identity(&victim);
    let barrier = Barrier::new(1_000_000);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let hold = gate.clone();
    let session = fx.start(SessionOptions {
        hooks: Some(barrier.clone()),
        before_publish: Some(Arc::new(move || {
            let mut released = hold.0.lock();
            while !*released {
                hold.1.wait(&mut released);
            }
        })),
        ..fx.options(1)
    });
    // Remove while the stage is complete but not yet published.
    barrier.release();
    let deadline = std::time::Instant::now() + WAIT;
    while session.phase() != orbit_core::session::Phase::Saving
        || !session.progress().discovery_complete
        || session.view(&bytes(&fx.root), &[]).unwrap().is_none()
    {
        assert!(std::time::Instant::now() < deadline);
        session.changes().wait(session.revision(), Duration::from_millis(20));
    }
    let request = RemovalRequest {
        root: bytes(&fx.root),
        root_identity: identity(&fx.root),
        items: vec![ScanRemoval { path: bytes(&victim), identity: victim_identity }],
    };
    let outcome = remove_with(&*fx.platform, &fx.paths, request, &mut NoFollowDelete, allow_all, &Cancellation::new());
    assert_eq!(outcome.removed.len(), 1);
    *gate.0.lock() = true;
    gate.1.notify_all();
    let snapshot = published(&session);
    assert_matches_disk(&fx.maps(), &snapshot);
}

#[test]
fn live_and_saved_search_agree() {
    let fx = Fixture::new();
    fx.populate(25, 8);
    write(&fx.root.join("d6/Résumé final.PDF"), 4321);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let hold = gate.clone();
    let session = fx.start(SessionOptions {
        before_publish: Some(Arc::new(move || {
            let mut released = hold.0.lock();
            while !*released {
                hold.1.wait(&mut released);
            }
        })),
        ..fx.options(1)
    });
    let deadline = std::time::Instant::now() + WAIT;
    while !session.progress().discovery_complete {
        assert!(std::time::Instant::now() < deadline);
        session.changes().wait(session.revision(), Duration::from_millis(20));
    }
    let queries = ["file-3", "RESUME", "e1", "dat", "nothing-matches"];
    let key = |r: &orbit_core::query::SearchResults| {
        let mut hits: Vec<(u64, Vec<u8>)> = r.hits.iter().map(|h| (h.meta.size, h.path.clone())).collect();
        hits.sort();
        (r.total, hits)
    };
    let removal_path = fx.root.join("d1");
    let removals = vec![ScanRemoval { path: bytes(&removal_path), identity: identity(&removal_path) }];
    // `None` means "not answerable right now" (the removal replay settles once writing ends);
    // like the app, ask again.
    let answered = |q: &str, removals: &[ScanRemoval]| {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if let Some(results) = session.search(q, removals, 10_000).unwrap() {
                return results;
            }
            assert!(std::time::Instant::now() < deadline, "search never became answerable");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let live: Vec<_> = queries.iter().flat_map(|q| [key(&answered(q, &[])), key(&answered(q, &removals))]).collect();
    *gate.0.lock() = true;
    gate.1.notify_all();
    let snapshot = published(&session);
    let maps = fx.maps();
    let saved = |maps: &SavedMaps<Native>| -> Vec<_> {
        queries
            .iter()
            .flat_map(|q| {
                [
                    key(&maps.search(&snapshot, q, &[], 10_000, None).unwrap()),
                    key(&maps.search(&snapshot, q, &removals, 10_000, None).unwrap()),
                ]
            })
            .collect()
    };
    assert_eq!(live, saved(&maps));
    // The prepared accelerator answers exactly like SQL.
    assert!(maps.prepare_search(&snapshot, &Cancellation::new()).unwrap());
    assert!(maps.is_search_prepared(&snapshot));
    assert_eq!(live, saved(&maps));
    let results = maps.search(&snapshot, "résumé", &[], 1, None).unwrap();
    assert_eq!((results.total, results.hits.len()), (1, 1));
}

#[test]
fn coverage_finds_the_covering_saved_map() {
    let fx = Fixture::new();
    fx.populate(6, 2);
    fx.map();
    let maps = fx.maps();
    let cancel = Cancellation::new();
    let sub = fx.root.join("d2/e2");
    let coverage = maps.coverage(&bytes(&sub), &cancel).unwrap().expect("covered");
    assert_eq!(coverage.root, bytes(&fx.root));
    assert_eq!(coverage.source, CoverageSource::Saved);
    assert_eq!(coverage.requested_identity, identity(&sub));
    // A folder outside the mapped root, and a path that is a file, are not covered.
    assert!(maps.coverage(&bytes(&fx.base), &cancel).unwrap().is_none());
    assert!(maps.coverage(&bytes(&fx.root.join("d1/linked.dat")), &cancel).unwrap().is_none());
    // A replaced folder is not reachable as the indexed directory any more. The replacement is
    // created while the original still exists so the two can never share an inode number
    // (ext4 reuses freed inodes immediately).
    let replacement = fx.root.join("d2/e2-new");
    std::fs::create_dir(&replacement).unwrap();
    std::fs::remove_dir_all(&sub).unwrap();
    std::fs::rename(&replacement, &sub).unwrap();
    let replaced = maps.coverage(&bytes(&sub), &cancel).unwrap();
    assert!(replaced.is_none(), "{replaced:?}");
}

#[test]
fn mapping_a_parent_reuses_the_saved_map_of_a_subfolder() {
    let fx = Fixture::new();
    fx.populate(30, 5);
    let sub = fx.root.join("d1");
    let sub_session = MappingSession::start(
        fx.platform.clone(),
        fx.paths.clone(),
        &bytes(&sub),
        SessionOptions {
            checkpoint: Some(ChangeCheckpoint { root: sub.clone(), identity: "test-history".into(), event_id: 3 }),
            ..fx.options(3)
        },
    )
    .unwrap();
    published(&sub_session);
    drop(sub_session);
    let parent = fx.start(fx.options(7));
    let reused: HashSet<Vec<u8>> = parent.reused_maps().iter().map(|c| c.root.clone()).collect();
    assert_eq!(reused, HashSet::from([bytes(&sub)]));
    let snapshot = published(&parent);
    // The reused map is older: the parent's change-history floor moved down to it.
    assert_eq!(snapshot.event_id, 3);
    assert!(parent.discovery_report().unwrap().cache_hits > 0);
    assert_matches_disk(&fx.maps(), &snapshot);
}

#[test]
fn obsolete_caches_are_removed_but_durable_folders_never() {
    let fx = Fixture::new();
    let old = fx.base.join("Caches/Indexes");
    write(&old.join("x.sqlite"), 10);
    let journal = fx.base.join("Support/Mapping");
    write(&journal.join(".orbit-removals/journal.sqlite"), 10);
    let dirs = orbit_platform::AppDirs {
        cache: fx.base.join("Caches/Maps"),
        support: fx.base.join("Support/Engine"),
        settings: fx.base.join("Support/Settings"),
        runtime: fx.base.join("run"),
        removal_journal: journal.clone(),
        // The journal's parent must never be deleted, even if listed by mistake.
        obsolete_caches: vec![old.clone(), fx.base.join("Support")],
    };
    let removed = orbit_core::store::discard_obsolete_caches(&dirs);
    assert_eq!(removed, vec![old.clone()]);
    assert!(!old.exists());
    assert!(journal.join(".orbit-removals/journal.sqlite").exists());
    // Paths for the platform resolve symlinked ancestors and exclude every engine folder.
    let link = fx.base.join("link");
    std::os::unix::fs::symlink(&fx.base, &link).unwrap();
    let resolved = IndexPaths::from_dirs(
        &orbit_platform::AppDirs { cache: link.join("Caches/Maps"), ..dirs.clone() },
        &fx.platform.path_aliases(),
    );
    assert_eq!(resolved.cache, fx.base.join("Caches/Maps"));
    assert!(resolved.exclusion_roots().contains(&journal.join(".orbit-removals")));
}

/// A folder replaced by another of the same name is reconciled from its parent's listing alone:
/// the changed identity prunes the old registration and the new folder is mapped in full.
#[test]
fn a_parent_listing_reconciles_a_replaced_subfolder() {
    let fx = Fixture::new();
    fx.populate(30, 6);
    let snapshot = fx.map();
    let maps = fx.maps();
    // Built beside the original so the two can never share an inode number.
    write(&fx.base.join("replacement/other/deeper/replacement.bin"), 4_444);
    std::fs::remove_dir_all(fx.root.join("d6/e1")).unwrap();
    std::fs::rename(fx.base.join("replacement"), fx.root.join("d6/e1")).unwrap();
    let checkpoint = ChangeCheckpoint { root: fx.root.clone(), identity: "test-history".into(), event_id: 9 };
    let listings = [bytes(&fx.root.join("d6"))];
    let updated = maps.refresh(&snapshot, &listings, &[], Some(&checkpoint), &Cancellation::new()).unwrap();
    assert_matches_disk(&maps, &updated);
}

/// A change reported inside a folder the map could not read re-lists only the folder holding
/// it, and that listing succeeds although it meets the unreadable folder again.
#[test]
fn changes_inside_an_unreadable_folder_relist_only_its_parent() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.populate(30, 6);
    let locked = fx.root.join("d1/locked");
    write(&locked.join("inner/secret.bin"), 10);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let restore = || std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        restore();
        return; // Running with privileges that ignore permissions.
    }
    let snapshot = fx.map();
    let maps = fx.maps();
    let before = folders(&maps, &snapshot);
    // An unreported change elsewhere below `d1` stays unread; `d1`'s own new file is listed.
    write(&fx.root.join("d1/e1/f1/file-0.dat"), 7_777);
    write(&fx.root.join("d1/top.bin"), 500);
    let checkpoint = ChangeCheckpoint { root: fx.root.clone(), identity: "test-history".into(), event_id: 9 };
    let listings = [bytes(&locked.join("inner")), bytes(&locked)];
    let result = maps.refresh(&snapshot, &listings, &[], Some(&checkpoint), &Cancellation::new());
    restore();
    let updated = result.unwrap();
    let after = folders(&maps, &updated);
    let d1 = bytes(&fx.root.join("d1"));
    let (files, size) = before[&d1];
    assert_eq!(after[&d1], (files + 1, size + 500), "the unreadable folder's parent was not re-listed");
    let deep = bytes(&fx.root.join("d1/e1/f1"));
    assert_eq!(after[&deep], before[&deep], "the parent's subtree was re-read");
}

/// Refreshes exclude cloud storage under the user's home exactly like the mapping did: a
/// recursive refresh above `Library/CloudStorage` never lists the provider folders (on macOS
/// those are placeholders that fail with EDEADLK rather than download).
#[test]
fn refresh_keeps_cloud_storage_excluded() {
    let fx = Fixture::new();
    fx.populate(10, 3);
    write(&fx.root.join("Library/CloudStorage/Provider/.Trash/placeholder.bin"), 9_000);
    write(&fx.root.join("Library/Preferences/settings.plist"), 100);
    let options = SessionOptions { home: Some(fx.root.clone()), ..fx.options(1) };
    let snapshot = published(&fx.start(options));
    let library = bytes(&fx.root.join("Library"));
    let maps = fx.maps().with_home(fx.root.clone());
    let before = folders(&maps, &snapshot)[&library];
    assert_eq!(before, (1, 100), "the mapping excluded cloud storage");
    write(&fx.root.join("Library/Preferences/more.plist"), 50);
    let checkpoint = ChangeCheckpoint { root: fx.root.clone(), identity: "test-history".into(), event_id: 9 };
    let updated =
        maps.refresh(&snapshot, &[], std::slice::from_ref(&library), Some(&checkpoint), &Cancellation::new()).unwrap();
    assert_eq!(folders(&maps, &updated)[&library], (2, 150), "the refresh mapped cloud storage");
}

/// Rows whose epoch is older than their folder's: left untouched by the latest listing.
fn untouched_rows(snapshot: &orbit_core::schema::SnapshotInfo) -> i64 {
    let conn = rusqlite::Connection::open(&snapshot.database).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM nodes n JOIN dirs d ON d.id=n.parent WHERE n.epoch<d.epoch AND n.kind<>1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn rescan(fx: &Fixture, saved: &orbit_core::schema::SnapshotInfo, event: u64) -> orbit_core::schema::SnapshotInfo {
    let options = SessionOptions { seed: Some(saved.clone()), reuse_saved_maps: false, ..fx.options(event) };
    published(&fx.start(options))
}

/// A forced rescan starts from the saved map: unchanged files are read but not written, and the
/// result is a new snapshot that matches the disk.
#[test]
fn a_seeded_rescan_of_unchanged_data_rewrites_nothing() {
    let fx = Fixture::new();
    fx.populate(40, 6);
    let saved = fx.map();
    let files = folders(&fx.maps(), &saved)[&saved.root_path].0;
    let rescanned = rescan(&fx, &saved, 2);
    assert_ne!(rescanned.id, saved.id, "a rescan publishes a new snapshot");
    assert_eq!(rescanned.event_id, 2);
    assert_matches_disk(&fx.maps(), &rescanned);
    assert_eq!(untouched_rows(&rescanned) as u64, files, "every unchanged file row stayed as it was");
}

/// Everything a rescan can find changed ends up exactly as on disk.
#[test]
fn a_seeded_rescan_applies_every_kind_of_change() {
    let fx = Fixture::new();
    fx.populate(30, 6);
    let saved = fx.map();
    write(&fx.root.join("d2/e1/new/inner/added.bin"), 5_000);
    write(&fx.root.join("d3/e3/f3/file-0.dat"), 9_999);
    std::fs::remove_file(fx.root.join("d4/e4/f4/file-1.dat")).unwrap();
    std::fs::remove_dir_all(fx.root.join("d5/e0/f5")).unwrap();
    write(&fx.root.join("d5/e0/f5"), 77);
    std::fs::remove_dir_all(fx.root.join("d6/e1")).unwrap();
    std::fs::hard_link(fx.root.join("d0/e0/f0/file-2.dat"), fx.root.join("d2/linked-2.dat")).unwrap();
    let rescanned = rescan(&fx, &saved, 3);
    assert_matches_disk(&fx.maps(), &rescanned);
    assert!(untouched_rows(&rescanned) > 100, "unchanged rows were rewritten");
}

/// A saved map already repaired by an update (its snapshot id differs from its original seal)
/// seeds a rescan too, instead of resuming the old stage it was made from.
#[test]
fn a_repaired_saved_map_seeds_a_rescan() {
    let fx = Fixture::new();
    fx.populate(20, 4);
    let saved = fx.map();
    let maps = fx.maps();
    write(&fx.root.join("d1/e1/f1/file-0.dat"), 4_444);
    let at = ChangeCheckpoint { root: fx.root.clone(), identity: "test-history".into(), event_id: 5 };
    let listings = [bytes(&fx.root.join("d1/e1/f1"))];
    let repaired = maps.refresh(&saved, &listings, &[], Some(&at), &Cancellation::new()).unwrap();
    assert_ne!(repaired.id, saved.id);
    write(&fx.root.join("d2/fresh.bin"), 123);
    let rescanned = rescan(&fx, &repaired, 6);
    assert_ne!(rescanned.id, repaired.id);
    assert_eq!(rescanned.event_id, 6);
    assert_matches_disk(&fx.maps(), &rescanned);
    assert!(untouched_rows(&rescanned) > 50);
}
