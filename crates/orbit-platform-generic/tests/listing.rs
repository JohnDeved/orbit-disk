//! Portable lister parity against an independent `lstat` inventory.
#![cfg(unix)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::Path;

use orbit_platform::{
    Cancellation, DirectoryLister, EntryKind, EntryRecord, FileIdentity, Issue, LISTING_BATCH, ListingSink, Platform,
    PlatformError, Stop,
};
use orbit_platform_generic::GenericPlatform;

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

fn dev(raw: u64) -> u64 {
    if std::mem::size_of::<libc::dev_t>() == 4 { raw & u64::from(u32::MAX) } else { raw }
}

fn oracle(dir: &Path) -> Vec<Owned> {
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
                id: FileIdentity { dev: dev(meta.dev()), ino: meta.ino() },
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

#[test]
fn listing_matches_lstat_across_batches() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("tree");
    std::fs::create_dir_all(root.join("nested/deeper")).unwrap();
    std::fs::write(root.join("small"), b"hello").unwrap();
    std::fs::write(root.join("empty"), b"").unwrap();
    std::fs::write(root.join("hard-a"), vec![7u8; 10_000]).unwrap();
    std::fs::hard_link(root.join("hard-a"), root.join("hard-b")).unwrap();
    std::fs::File::create(root.join("sparse")).unwrap().set_len(64 * 1024 * 1024).unwrap();
    symlink("nested", root.join("link-dir")).unwrap();
    symlink("missing-target", root.join("dangling")).unwrap();
    for index in 0..300 {
        std::fs::write(root.join(format!("item-{index:03}")), vec![1u8; index % 73]).unwrap();
    }
    let mut odd = root.as_os_str().as_bytes().to_vec();
    odd.extend_from_slice(b"/odd-\xff\xfe-name");
    // Linux filesystems accept any bytes; APFS/HFS+ reject invalid UTF-8.
    let has_odd_name = std::fs::write(OsStr::from_bytes(&odd), b"x").is_ok();

    let platform = GenericPlatform::new();
    let (dir, _) = platform.open_root(&root).unwrap();
    let path = root.as_os_str().as_bytes();
    let mut lister = platform.new_lister();
    let mut sink = Collect::default();
    let totals = lister.list(&dir, path, &Cancellation::new(), &mut sink).unwrap();
    sink.records.sort();
    assert!(sink.issues.is_empty(), "{:?}", sink.issues);
    assert_eq!(sink.records, oracle(&root));
    assert_eq!(totals.entries as usize, sink.records.len());
    assert!(sink.batches.len() >= 3 && sink.batches.iter().all(|&n| n > 0 && n <= LISTING_BATCH));
    let find = |name: &[u8]| sink.records.iter().find(|r| r.name == name).unwrap().clone();
    assert_eq!(find(b"hard-a").id, find(b"hard-b").id);
    assert_eq!(find(b"hard-a").nlink, 2);
    let sparse = find(b"sparse");
    assert!(sparse.allocated < sparse.size);
    assert_eq!(sink.records.iter().any(|r| r.name == b"odd-\xff\xfe-name"), has_odd_name);

    let mut again = Collect::default();
    lister.list(&dir, path, &Cancellation::new(), &mut again).unwrap();
    again.records.sort();
    assert_eq!(again.records, sink.records, "repeat listing on the same descriptor");

    let mut stopped = Collect { stop_after_batches: Some(1), ..Default::default() };
    let result = lister.list(&dir, path, &Cancellation::new(), &mut stopped);
    assert!(matches!(result, Err(PlatformError::Cancelled)) && stopped.batches.len() == 1, "{result:?}");
}
