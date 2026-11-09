use super::*;
use std::cell::{Cell, RefCell};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

type Probe = Box<dyn Fn(&[u8]) -> Result<Option<bool>, FsError>>;

/// Real path facts from a temporary directory with an injected cloud probe.
struct TestFs {
    probe: RefCell<Probe>,
    materialization_fails: Cell<bool>,
}

impl TestFs {
    fn new() -> Self {
        Self::probing(|_| Ok(None))
    }
    fn probing(probe: impl Fn(&[u8]) -> Result<Option<bool>, FsError> + 'static) -> Self {
        Self { probe: RefCell::new(Box::new(probe)), materialization_fails: Cell::new(false) }
    }
}

impl PolicyFs for TestFs {
    fn canonicalize(&self, path: &[u8]) -> Vec<u8> {
        std_fs::canonicalize(path)
    }
    fn lstat_kind(&self, path: &[u8]) -> Result<FsKind, FsError> {
        std_fs::lstat_kind(path)
    }
    fn directory_identity(&self, path: &[u8]) -> Option<FileIdentity> {
        std_fs::directory_identity(path)
    }
    fn is_cloud_item(&self, path: &[u8]) -> Result<Option<bool>, FsError> {
        (self.probe.borrow())(path)
    }
    fn without_materialization<R>(&self, body: impl FnOnce() -> R) -> Result<R, FsError> {
        if self.materialization_fails.get() { Err(FsError::Other("policy unavailable".into())) } else { Ok(body()) }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    base: PathBuf,
    home: PathBuf,
    scan_root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // The policy inspects every ancestor up to `/`; keep the fixture out of hidden folders.
        let dir = tempfile::Builder::new().prefix("orbit-policy").tempdir_in("/tmp").unwrap();
        let base = dir.path().to_path_buf();
        let home = base.join("home");
        let scan_root = home.join("Downloads");
        std::fs::create_dir_all(&scan_root).unwrap();
        Self { _dir: dir, base, home, scan_root }
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.scan_root.join(name);
        std::fs::write(&path, b"test").unwrap();
        path
    }

    /// The macOS protection layout; the temporary directory stays open inside `/private`.
    fn protected(&self) -> ProtectedPaths {
        let mut system_roots: Vec<PathBuf> = ["/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc"]
            .iter()
            .map(PathBuf::from)
            .collect();
        system_roots.push(self.home.join("Library"));
        ProtectedPaths {
            system_roots,
            temporary_roots: vec![std::env::temp_dir(), PathBuf::from("/tmp"), PathBuf::from("/private/tmp")],
            restricted_root: Some(PathBuf::from("/private")),
            trash_components: vec![".Trash", ".Trashes"],
            volumes_root: Some(PathBuf::from("/Volumes")),
        }
    }

    fn known_cloud_roots(&self) -> Vec<Vec<u8>> {
        ["Library/CloudStorage", "Library/Mobile Documents"]
            .iter()
            .map(|r| bytes(&self.home.join(r)).to_vec())
            .collect()
    }

    fn reason_in(&self, fs: &TestFs, path: &Path, scan_root: &Path) -> Option<String> {
        let cloud = cloud_exclusions(&self.known_cloud_roots(), &[], fs);
        let volumes: Vec<Vec<u8>> = Vec::new();
        RemovalPolicy::new(fs, bytes(scan_root), bytes(&self.home), &self.protected(), &cloud, &volumes)
            .rejection_reason(bytes(path))
    }

    fn reason(&self, path: &Path) -> Option<String> {
        self.reason_in(&TestFs::new(), path, &self.scan_root)
    }
}

fn bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

#[test]
fn standardize_is_lexical_and_byte_preserving() {
    let cases: [(&[u8], Option<&[u8]>); 6] = [
        (b"/a//b/./c/", Some(b"/a/b/c")),
        (b"/a/b/../../..", Some(b"/")),
        (b"/", Some(b"/")),
        (b"relative/x", None),
        (b"", None),
        ("/e\u{301}/./\u{e9}".as_bytes(), Some("/e\u{301}/\u{e9}".as_bytes())),
    ];
    for (input, expected) in cases {
        assert_eq!(standardize(input).as_deref(), expected, "{:?}", String::from_utf8_lossy(input));
    }
}

#[test]
fn ordinary_descendant_allowed_but_root_sibling_and_home_protected() {
    let f = Fixture::new();
    assert_eq!(f.reason(&f.file("report.txt")), None);
    assert_eq!(f.reason(&f.scan_root).as_deref(), Some(MAPPED_FOLDER));
    let sibling = f.home.join("Downloads-old");
    std::fs::create_dir_all(&sibling).unwrap();
    assert_eq!(f.reason(&sibling).as_deref(), Some(OUTSIDE_ROOT));
    // Home contains the cloud roots, so the cloud rule rejects it first (as in Swift); without
    // cloud roots the home rule does.
    assert_eq!(f.reason_in(&TestFs::new(), &f.home, &f.base).as_deref(), Some(CLOUD_EXCLUDED));
    let fs = TestFs::new();
    let none: Vec<Vec<u8>> = Vec::new();
    let mut policy = RemovalPolicy::new(&fs, bytes(&f.base), bytes(&f.home), &f.protected(), &none, &none);
    assert_eq!(policy.rejection_reason(bytes(&f.home)).as_deref(), Some(PROTECTED_LOCATION));
    assert_eq!(f.reason(Path::new("relative/item")).as_deref(), Some(NOT_LOCAL));
}

#[test]
fn symlinks_and_ancestor_escapes_are_rejected() {
    let f = Fixture::new();
    let file = f.file("report.txt");
    let link = f.scan_root.join("report-link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert_eq!(f.reason(&link).as_deref(), Some(SYMLINK));
    let outside = f.base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("item"), b"outside").unwrap();
    let escape = f.scan_root.join("escape");
    std::os::unix::fs::symlink(&outside, &escape).unwrap();
    assert_eq!(f.reason(&escape.join("item")).as_deref(), Some(OUTSIDE_ROOT));
}

#[test]
fn hidden_folders_trash_and_library_are_protected() {
    let f = Fixture::new();
    for folder in [".ssh", ".Trash", ".Trashes"] {
        let directory = f.scan_root.join(folder);
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("item");
        std::fs::write(&file, b"").unwrap();
        assert!(f.reason(&directory).is_some(), "{folder}");
        assert!(f.reason(&file).is_some(), "{folder}/item");
    }
    assert_eq!(f.reason(&f.scan_root.join(".Trash/item")).as_deref(), Some(IN_TRASH));
    assert_eq!(f.reason(&f.scan_root.join(".ssh/item")).as_deref(), Some(HIDDEN_FOLDER));
    let library = f.home.join("Library");
    std::fs::create_dir_all(&library).unwrap();
    // Rejected as a folder containing cloud storage before the Library rule is reached (as in Swift).
    assert_eq!(f.reason_in(&TestFs::new(), &library, &f.home).as_deref(), Some(CLOUD_EXCLUDED));
    let fonts = library.join("Fonts");
    std::fs::create_dir_all(&fonts).unwrap();
    assert_eq!(f.reason_in(&TestFs::new(), &fonts, &f.home).as_deref(), Some(PROTECTED_SYSTEM));
    assert_eq!(f.reason(&f.file(".DS_Store")), None, "ordinary dotfiles are removable");
}

#[test]
fn system_paths_volumes_and_missing_items_are_rejected() {
    let f = Fixture::new();
    for path in [
        "/",
        "/System",
        "/Library",
        "/Applications",
        "/usr",
        "/bin",
        "/sbin",
        "/etc",
        "/private",
        "/private/var",
        "/Volumes/External",
    ] {
        assert!(f.reason_in(&TestFs::new(), Path::new(path), Path::new("/")).is_some(), "{path}");
    }
    assert_eq!(f.reason_in(&TestFs::new(), Path::new("/Volumes/External"), Path::new("/")).as_deref(), Some(VOLUME));
    let missing = f.reason(&f.scan_root.join("missing")).unwrap();
    assert!(missing.starts_with(NO_LONGER_ACCESSIBLE), "{missing}");

    // A folder containing a mounted volume cannot be removed either.
    let holder = f.scan_root.join("holder");
    std::fs::create_dir_all(holder.join("mounted")).unwrap();
    let fs = TestFs::new();
    let volumes = vec![bytes(&holder.join("mounted")).to_vec()];
    let none: Vec<Vec<u8>> = Vec::new();
    let mut policy = RemovalPolicy::new(&fs, bytes(&f.scan_root), bytes(&f.home), &f.protected(), &none, &volumes);
    assert_eq!(policy.rejection_reason(bytes(&holder)).as_deref(), Some(VOLUME));
}

#[test]
fn cloud_roots_and_resolved_aliases_are_protected() {
    let f = Fixture::new();
    let cloud = f.home.join("Library/CloudStorage/Provider");
    std::fs::create_dir_all(&cloud).unwrap();
    let file = cloud.join("report.txt");
    std::fs::write(&file, b"cloud").unwrap();
    let known = f.known_cloud_roots();
    assert!(is_within_any(bytes(&file), &known));
    assert!(!is_within_any(bytes(&f.home.join("Library/CloudStorage-old")), &known));
    assert_eq!(f.reason_in(&TestFs::new(), &file, &f.home).as_deref(), Some(CLOUD_EXCLUDED));
    let alias = f.scan_root.join("shared");
    std::os::unix::fs::symlink(&cloud, &alias).unwrap();
    assert_eq!(f.reason(&alias.join("report.txt")).as_deref(), Some(CLOUD_EXCLUDED));

    let fs = TestFs::new();
    let roots = [bytes(&cloud).to_vec()];
    let parent = cloud.parent().unwrap();
    assert_eq!(CloudValidator::new(&fs, &roots, true).exclusion_reason(bytes(parent)), Some(CLOUD_EXCLUDED));
    // On case-insensitive volumes a case-variant spelling must match by identity.
    let lower = f.home.join("library/cloudstorage/provider/report.txt");
    if lower.exists() {
        assert_eq!(CloudValidator::new(&fs, &roots, false).exclusion_reason(bytes(&lower)), Some(CLOUD_EXCLUDED));
        let variant = f.home.join("library");
        assert_eq!(CloudValidator::new(&fs, &roots, true).exclusion_reason(bytes(&variant)), Some(CLOUD_EXCLUDED));
    }
}

#[test]
fn cloud_exclusions_come_from_known_folders_and_readable_metadata() {
    let f = Fixture::new();
    let (desktop, documents) = (f.home.join("Desktop"), f.home.join("Documents"));
    std::fs::create_dir_all(&desktop).unwrap();
    std::fs::create_dir_all(&documents).unwrap();
    let known = f.known_cloud_roots();
    let optional = vec![bytes(&desktop).to_vec(), bytes(&documents).to_vec()];
    let documents_bytes = bytes(&documents).to_vec();
    let fs = TestFs::probing(move |p| Ok(Some(p == documents_bytes.as_slice())));
    let roots = cloud_exclusions(&known, &optional, &fs);
    assert!(roots.contains(&optional[1]) && !roots.contains(&optional[0]));
    assert!(known.iter().all(|k| roots.contains(k)));
    let unreadable = TestFs::probing(|_| Err(FsError::Other("permission denied".into())));
    assert!(!cloud_exclusions(&known, &optional, &unreadable).contains(&optional[1]));
    let placeholders = TestFs::probing(|_| Err(FsError::Dataless));
    assert!(cloud_exclusions(&known, &optional, &placeholders).contains(&optional[1]));
}

#[test]
fn dataless_and_ordinary_metadata_errors_are_distinguished() {
    let f = Fixture::new();
    let shared = f.scan_root.join("Synced");
    let folder = shared.join("nested");
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("report.txt");
    std::fs::write(&file, b"test").unwrap();
    let file = std_fs::canonicalize(bytes(&file));
    let shared = std_fs::canonicalize(bytes(&shared));
    let none: [Vec<u8>; 0] = [];
    let reason = |fs: TestFs| CloudValidator::new(&fs, &none, false).exclusion_reason(&file);
    assert_eq!(reason(TestFs::probing(move |p| Ok(Some(p == shared.as_slice())))), Some(CLOUD_EXCLUDED), "inherited");
    assert_eq!(reason(TestFs::probing(|_| Ok(Some(false)))), None);
    assert_eq!(reason(TestFs::probing(|_| Err(FsError::Other("denied".into())))), None, "access denied is not cloud");
    assert_eq!(reason(TestFs::probing(|_| Err(FsError::Dataless))), Some(CLOUD_DATALESS));

    let blocked = TestFs::new();
    blocked.materialization_fails.set(true);
    assert_eq!(
        f.reason_in(&blocked, Path::new(std::str::from_utf8(&file).unwrap()), &f.scan_root).as_deref(),
        Some(NEEDS_DOWNLOAD)
    );
}

#[test]
fn batch_validator_rechecks_each_item_and_new_batches_start_fresh() {
    let f = Fixture::new();
    let files: Vec<Vec<u8>> = (0..4).map(|i| std_fs::canonicalize(bytes(&f.file(&format!("item-{i}.txt"))))).collect();
    let changed: std::rc::Rc<RefCell<Option<Vec<u8>>>> = Default::default();
    let probe_changed = changed.clone();
    let fs = TestFs::probing(move |p| Ok(Some(probe_changed.borrow().as_deref() == Some(p))));
    let none: [Vec<u8>; 0] = [];
    let mut batch = CloudValidator::new(&fs, &none, true);
    for file in &files {
        assert_eq!(batch.exclusion_reason(file), None);
    }
    *changed.borrow_mut() = Some(files[0].clone());
    assert_eq!(batch.exclusion_reason(&files[0]), Some(CLOUD_EXCLUDED), "an item was not rechecked within its batch");
    *changed.borrow_mut() = Some(std_fs::canonicalize(bytes(&f.scan_root)));
    assert_eq!(
        CloudValidator::new(&fs, &none, true).exclusion_reason(&files[1]),
        Some(CLOUD_EXCLUDED),
        "a new batch inherited an old ancestor classification"
    );
}
