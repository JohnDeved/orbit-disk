//! Lister parity and traversal safety on real filesystems.
#![cfg(target_os = "macos")]

use std::os::unix::fs::{MetadataExt, symlink};
use std::path::Path;

use orbit_platform::{
    Cancellation, DirectoryLister, EntryKind, EntryRecord, FileIdentity, Issue, LISTING_BATCH, ListingSink,
    ListingTotals, Platform, PlatformError, Stop,
};
use orbit_platform_macos::{MacLister, MacPlatform};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Owned {
    name: Vec<u8>,
    kind: u8,
    id: FileIdentity,
    nlink: u32,
    size: u64,
    allocated: u64,
    is_mount_point: bool,
}

#[derive(Default)]
struct Collect {
    records: Vec<Owned>,
    batches: Vec<usize>,
    issues: Vec<Issue>,
    stop_after_batches: Option<usize>,
}

impl ListingSink for Collect {
    fn batch(&mut self, records: &[EntryRecord<'_>]) -> Result<(), Stop> {
        self.batches.push(records.len());
        self.records.extend(records.iter().map(|r| Owned {
            name: r.name.to_vec(),
            kind: r.kind as u8,
            id: r.id,
            nlink: r.nlink,
            size: r.size,
            allocated: r.allocated,
            is_mount_point: r.is_mount_point,
        }));
        if self.stop_after_batches.is_some_and(|limit| self.batches.len() >= limit) {
            return Err(Stop);
        }
        Ok(())
    }

    fn issue(&mut self, issue: Issue) {
        self.issues.push(issue);
    }
}

fn list(lister: &mut MacLister, dir: &std::os::fd::OwnedFd, path: &[u8]) -> (Collect, ListingTotals) {
    let mut sink = Collect::default();
    let totals = lister.list(dir, path, &Cancellation::new(), &mut sink).unwrap();
    sink.records.sort();
    (sink, totals)
}

fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

/// What an independent `lstat` says each entry should look like.
fn oracle(dir: &Path) -> Vec<Owned> {
    use std::os::unix::ffi::OsStrExt;
    let mut out: Vec<Owned> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            let kind = if meta.is_dir() {
                EntryKind::Directory
            } else if meta.file_type().is_symlink() {
                EntryKind::Symlink
            } else if meta.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            let is_dir = kind == EntryKind::Directory;
            Owned {
                name: entry.file_name().as_bytes().to_vec(),
                kind: kind as u8,
                id: FileIdentity { dev: meta.dev() & u64::from(u32::MAX), ino: meta.ino() },
                nlink: if is_dir { 1 } else { meta.nlink() as u32 },
                size: if is_dir { 0 } else { meta.size() },
                allocated: if is_dir { 0 } else { meta.blocks() * 512 },
                is_mount_point: false,
            }
        })
        .collect();
    out.sort();
    out
}

fn mkfifo(path: &Path) {
    let c = std::ffi::CString::new(path_bytes(path)).unwrap();
    // SAFETY: valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
}

/// Files, nested folders, symlinks (to a folder and dangling), hard links, empty and sparse files,
/// a FIFO, more than two batches of entries and, where the filesystem allows it, a non-UTF-8 name.
fn build_fixture(root: &Path) -> bool {
    std::fs::create_dir_all(root.join("nested/deeper")).unwrap();
    std::fs::write(root.join("nested/child"), b"child").unwrap();
    std::fs::write(root.join("small"), b"hello").unwrap();
    std::fs::write(root.join("empty"), b"").unwrap();
    std::fs::write(root.join("hard-a"), vec![7u8; 10_000]).unwrap();
    std::fs::hard_link(root.join("hard-a"), root.join("hard-b")).unwrap();
    let sparse = std::fs::File::create(root.join("sparse")).unwrap();
    sparse.set_len(64 * 1024 * 1024).unwrap();
    symlink("nested", root.join("link-dir")).unwrap();
    symlink("missing-target", root.join("dangling")).unwrap();
    std::fs::write(root.join("unicode 日本語 café 😀"), b"u").unwrap();
    mkfifo(&root.join("fifo"));
    for index in 0..300 {
        std::fs::write(root.join(format!("item-{index:03}")), vec![1u8; index % 73]).unwrap();
    }
    let mut odd = path_bytes(root);
    odd.extend_from_slice(b"/odd-\xff\xfe-name");
    use std::os::unix::ffi::OsStrExt;
    // APFS and HFS+ reject invalid UTF-8 (EILSEQ); other filesystems accept it.
    std::fs::write(std::ffi::OsStr::from_bytes(&odd), b"x").is_ok()
}

#[test]
fn bulk_listing_matches_fallback_and_lstat() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("tree");
    let has_odd_name = build_fixture(&root);
    let platform = MacPlatform::new();
    let (dir, _) = platform.open_root(&root).unwrap();
    let path = path_bytes(&root);

    let mut bulk = platform.new_lister();
    let (bulk_out, bulk_totals) = list(&mut bulk, &dir, &path);
    let mut fallback = platform.new_fallback_lister();
    let (fallback_out, fallback_totals) = list(&mut fallback, &dir, &path);

    assert!(bulk_out.issues.is_empty(), "{:?}", bulk_out.issues);
    assert!(fallback_out.issues.is_empty(), "{:?}", fallback_out.issues);
    assert_eq!(bulk_out.records, fallback_out.records);
    assert_eq!(bulk_out.records, oracle(&root));
    assert_eq!(bulk_totals, fallback_totals);
    assert_eq!(bulk_totals.entries as usize, bulk_out.records.len());
    assert_eq!(bulk_totals.logical_bytes, bulk_out.records.iter().map(|r| r.size).sum::<u64>());
    for batches in [&bulk_out.batches, &fallback_out.batches] {
        assert!(batches.len() >= 3 && batches.iter().all(|&n| n > 0 && n <= LISTING_BATCH), "{batches:?}");
    }

    let by_name = |name: &[u8]| bulk_out.records.iter().find(|r| r.name == name).unwrap().clone();
    let (a, b) = (by_name(b"hard-a"), by_name(b"hard-b"));
    assert_eq!((a.id, a.nlink), (b.id, 2), "hard links must share one identity");
    let sparse = by_name(b"sparse");
    assert!(sparse.size == 64 * 1024 * 1024 && sparse.allocated < sparse.size, "{sparse:?}");
    assert_eq!(by_name(b"link-dir").kind, EntryKind::Symlink as u8, "symlinks are never followed");
    assert_eq!(by_name(b"fifo").kind, EntryKind::Other as u8);
    assert_eq!(bulk_out.records.iter().any(|r| r.name == b"odd-\xff\xfe-name"), has_odd_name);

    // Listing the same descriptor again restarts the enumeration.
    let (again, _) = list(&mut bulk, &dir, &path);
    assert_eq!(again.records, bulk_out.records);
}

#[test]
fn stop_and_cancellation_end_the_listing() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..(LISTING_BATCH * 3) {
        std::fs::write(temp.path().join(format!("f{index}")), b"").unwrap();
    }
    let platform = MacPlatform::new();
    let (dir, _) = platform.open_root(temp.path()).unwrap();
    let path = path_bytes(temp.path());
    for mut lister in [platform.new_lister(), platform.new_fallback_lister()] {
        let mut sink = Collect { stop_after_batches: Some(1), ..Default::default() };
        let result = lister.list(&dir, &path, &Cancellation::new(), &mut sink);
        assert!(matches!(result, Err(PlatformError::Cancelled)), "{result:?}");
        assert_eq!(sink.batches, vec![LISTING_BATCH]);

        let cancel = Cancellation::new();
        cancel.cancel();
        let mut sink = Collect::default();
        let result = lister.list(&dir, &path, &cancel, &mut sink);
        assert!(matches!(result, Err(PlatformError::Cancelled)) && sink.batches.is_empty(), "{result:?}");
    }
}

#[test]
fn open_child_rejects_swapped_directories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir(root.join("a")).unwrap();
    let platform = MacPlatform::new();
    let (dir, _) = platform.open_root(root).unwrap();
    let (listed, _) = list(&mut platform.new_lister(), &dir, &path_bytes(root));
    let queued = listed.records.iter().find(|r| r.name == b"a").unwrap().id;
    assert!(platform.open_child(&dir, b"a", queued).is_ok());

    // Another directory renamed into place.
    std::fs::rename(root.join("a"), root.join("a-old")).unwrap();
    std::fs::create_dir(root.join("a")).unwrap();
    let err = platform.open_child(&dir, b"a", queued).unwrap_err();
    assert!(matches!(err, PlatformError::IdentityChanged { .. }), "{err:?}");

    // A symlink to the original directory swapped in.
    std::fs::remove_dir(root.join("a")).unwrap();
    symlink("a-old", root.join("a")).unwrap();
    let err = platform.open_child(&dir, b"a", queued).unwrap_err();
    assert!(matches!(err, PlatformError::IdentityChanged { .. }), "{err:?}");
}

#[test]
fn boot_volume_firmlinks_and_data_mount_are_reported_as_opened() {
    let data = Path::new("/System/Volumes/Data");
    let platform = MacPlatform::new();
    let (root, root_id) = platform.open_root(Path::new("/")).unwrap();
    let Some(boot) = platform.volume_layout(Path::new("/"), root_id).boot else {
        eprintln!("SKIP: boot-volume checks need a verified split System/Data layout");
        return;
    };
    assert!(boot.logical_aliases.iter().any(|(path, _)| path == b"/Users"));
    for mut lister in [platform.new_lister(), platform.new_fallback_lister()] {
        let (listing, _) = list(&mut lister, &root, b"/");
        assert!(listing.issues.iter().all(|i| !i.path.starts_with(b"/Users")), "{:?}", listing.issues);
        for name in ["Users", "Applications", "Library", "private"] {
            let observed = listing.records.iter().find(|r| r.name == name.as_bytes()).unwrap();
            let meta = std::fs::symlink_metadata(Path::new("/").join(name)).unwrap();
            let actual = FileIdentity { dev: meta.dev() & u64::from(u32::MAX), ino: meta.ino() };
            assert_eq!(observed.id, actual, "firmlink {name} reported the stub identity");
            assert!(!observed.is_mount_point);
            assert!(platform.open_child(&root, name.as_bytes(), actual).is_ok(), "{name}");
        }
        let users = listing.records.iter().find(|r| r.name == b"Users").unwrap().id;
        let stale = FileIdentity { ino: users.ino + 1, ..users };
        assert!(matches!(platform.open_child(&root, b"Users", stale), Err(PlatformError::IdentityChanged { .. })));

        let (volumes_dir, _) = platform.open_root(data.parent().unwrap()).unwrap();
        let (volumes, _) = list(&mut lister, &volumes_dir, b"/System/Volumes");
        let data_entry = volumes.records.iter().find(|r| r.name == b"Data").unwrap();
        assert!(data_entry.is_mount_point, "Data volume not reported as a mount point");
        assert_eq!(data_entry.id, boot.data_identity);
    }
}

/// Cached-ancestor revalidation relies on `child_identity` reaching the same object as
/// `open_child`, including firmlinks (`/Users`) and mount points (`/System/Volumes/Data`).
#[test]
fn child_identity_agrees_with_open_child_across_firmlinks_and_mounts() {
    let platform = MacPlatform::new();
    for (parent, names) in
        [("/", &["Users", "Applications", "System", "private"][..]), ("/System/Volumes", &["Data"][..])]
    {
        let (dir, _) = platform.open_root(Path::new(parent)).unwrap();
        for name in names {
            let id = platform.child_identity(&dir, name.as_bytes()).unwrap();
            let opened = platform.open_child(&dir, name.as_bytes(), id);
            assert!(opened.is_ok(), "{parent}/{name}: {opened:?}");
            assert_eq!(platform.dir_identity(&opened.unwrap()).unwrap(), id, "{parent}/{name}");
        }
    }
}
