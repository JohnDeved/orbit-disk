//! An in-memory [`Backend`] over a generated tree (~200k entries, power-law folder sizes and
//! log-normal file sizes) for `--fixture` runs and the frame harness, or over the small fixed
//! sample Home of the README screenshots ([`FixtureLayout::Sample`], `--marketing-state`).
//!
//! `orbit_app_state::testing::FakeBackend` recomputes folder totals by scanning every entry, so it
//! cannot serve views of a tree this size interactively, and its mapping never reports progress.
//! This backend precomputes totals, keeps children sorted by size, and simulates a mapping run
//! (discovery, progress ticks, preview updates, publication) on its own thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use orbit_app_state::backend::{
    MappingRequest, RefreshRequest, RemovalOutcome, RemovalRequest, Resolution, ResolveCandidate, SearchReply,
    SearchRequest, ViewRequest, ViewSource,
};
use orbit_app_state::model::ProgressSnapshot;
use orbit_app_state::{Backend, Event, Item, MappingEvent, Notifier, SessionId, SnapshotInfo, Volume};
use orbit_core::fold::{fold, fold_query};
use orbit_core::paths::is_within;
use orbit_core::types::ScanRemoval;
use orbit_core::view::{
    FolderTree, FolderTreeBuilder, NodeFlags, NodeMeta, VIEW_DEPTH, VIEW_DESCENDANT_BUDGET, ViewSource as TreeSource,
};
use orbit_platform::{Cancellation, FileIdentity};

/// The fixture volume root.
pub const ROOT: &[u8] = b"/Fixture";
/// The fixture user's home (use as `Config::home`).
pub const HOME: &[u8] = b"/Fixture/Users/demo";

/// The tree the fixture serves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FixtureLayout {
    /// About `entries` generated entries on a whole fake volume.
    #[default]
    Generated,
    /// The README sample Home ([`sample`]): a few recognizable folders with fixed sizes on a
    /// 1 TB "Macintosh HD", with Home already mapped. `entries` and `seed` are ignored.
    Sample,
}

#[derive(Clone, Debug)]
pub struct FixtureOptions {
    pub layout: FixtureLayout,
    /// Approximate number of entries (files + folders).
    pub entries: usize,
    pub seed: u64,
    /// Simulated mapping time of a fresh (non-reused) mapping run.
    pub mapping_duration: Duration,
    /// Interval of progress notifications while mapping.
    pub progress_interval: Duration,
}

impl FixtureOptions {
    /// The README sample Home ([`FixtureLayout::Sample`]).
    pub fn sample() -> Self {
        Self { layout: FixtureLayout::Sample, ..Self::default() }
    }
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            layout: FixtureLayout::Generated,
            entries: 200_000,
            seed: 0x0eb1_7d15_c0de,
            mapping_duration: Duration::from_millis(2_500),
            progress_interval: Duration::from_millis(50),
        }
    }
}

#[derive(Debug)]
struct Node {
    parent: u32,
    is_dir: bool,
    removed: bool,
    size: u64,
    files: u64,
    path: Box<[u8]>,
    /// Offset of the last path component in `path`.
    name_at: u32,
    folded: Box<str>,
    children: Vec<u32>,
}

impl Node {
    fn name(&self) -> &[u8] {
        &self.path[self.name_at as usize..]
    }
}

#[derive(Debug, Default)]
struct Tree {
    nodes: Vec<Node>,
    index: HashMap<Box<[u8]>, u32>,
}

const NONE: u32 = u32::MAX;

impl Tree {
    fn add(&mut self, parent: u32, name: &[u8], is_dir: bool, size: u64) -> u32 {
        let mut path = if parent == NONE { Vec::new() } else { self.nodes[parent as usize].path.to_vec() };
        let name_at = path.len() as u32 + u32::from(parent != NONE);
        if parent != NONE {
            path.push(b'/');
        }
        path.extend_from_slice(name);
        let id = self.nodes.len() as u32;
        let path: Box<[u8]> = path.into();
        self.index.insert(path.clone(), id);
        self.nodes.push(Node {
            parent,
            is_dir,
            removed: false,
            size,
            files: u64::from(!is_dir),
            folded: fold(name).into(),
            path,
            name_at,
            children: Vec::new(),
        });
        if parent != NONE {
            self.nodes[parent as usize].children.push(id);
        }
        id
    }

    fn find(&self, path: &[u8]) -> Option<u32> {
        self.index.get(path).copied().filter(|&id| !self.nodes[id as usize].removed)
    }

    fn identity(id: u32) -> FileIdentity {
        FileIdentity { dev: 0x0f17, ino: u64::from(id) + 2 }
    }

    fn meta(&self, id: u32) -> NodeMeta {
        let node = &self.nodes[id as usize];
        NodeMeta {
            flags: NodeFlags { is_dir: node.is_dir, is_symlink: false, is_mount_point: false, accessible: true },
            size: node.size,
            allocated: node.size.div_ceil(4096) * 4096,
            raw_allocated: if node.is_dir { 0 } else { node.size.div_ceil(4096) * 4096 },
            file_count: node.files,
            identity: Some(Self::identity(id)),
            state: Default::default(),
        }
    }

    fn item(&self, id: u32) -> Item {
        Item::new(&self.nodes[id as usize].path[..], self.meta(id))
    }

    /// Totals bottom-up (children are always created after their parent) and size order.
    fn finish(&mut self) {
        for id in (1..self.nodes.len()).rev() {
            let (parent, size, files) = {
                let n = &self.nodes[id];
                (n.parent, n.size, n.files)
            };
            let p = &mut self.nodes[parent as usize];
            p.size += size;
            p.files += files;
        }
        for id in 0..self.nodes.len() {
            let mut children = std::mem::take(&mut self.nodes[id].children);
            children.sort_by(|&a, &b| {
                let (a, b) = (&self.nodes[a as usize], &self.nodes[b as usize]);
                b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path))
            });
            self.nodes[id].children = children;
        }
    }

    fn live_children(&self, id: u32) -> impl Iterator<Item = u32> + '_ {
        self.nodes[id as usize].children.iter().copied().filter(|&c| !self.nodes[c as usize].removed)
    }

    /// The folder view: the folder, its ancestors up to `root`, and descendants to the view depth.
    fn view(&self, root: &[u8], folder: u32, revision: u64, source: TreeSource) -> FolderTree {
        let node = &self.nodes[folder as usize];
        let mut builder = FolderTreeBuilder::new(Arc::from(root), node.path.to_vec(), node.name(), self.meta(folder));
        let mut chain = Vec::new();
        let mut current = node.parent;
        if &node.path[..] != root {
            while current != NONE {
                chain.push(current);
                if &self.nodes[current as usize].path[..] == root {
                    break;
                }
                current = self.nodes[current as usize].parent;
            }
        }
        for &ancestor in chain.iter().rev() {
            builder.push_ancestor(self.nodes[ancestor as usize].name(), self.meta(ancestor));
        }
        let mut queue = vec![(FolderTree::FOLDER, folder, 0u32)];
        let mut next = 0;
        let mut descendants = 0;
        while next < queue.len() {
            let (ix, id, depth) = queue[next];
            next += 1;
            for child in self.live_children(id) {
                if depth > 0 && descendants >= VIEW_DESCENDANT_BUDGET {
                    break;
                }
                if depth > 0 {
                    descendants += 1;
                }
                let child_node = &self.nodes[child as usize];
                let child_ix = builder.push_child(ix, child_node.name(), self.meta(child));
                if child_node.is_dir && depth + 1 < VIEW_DEPTH {
                    queue.push((child_ix, child, depth + 1));
                }
            }
        }
        builder.finish(revision, false, source)
    }

    /// Marks `id` and its subtree removed and subtracts its totals from the ancestors.
    fn remove(&mut self, id: u32) {
        let (size, files) = (self.nodes[id as usize].size, self.nodes[id as usize].files);
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            self.nodes[n as usize].removed = true;
            stack.extend(self.nodes[n as usize].children.iter().copied());
        }
        let mut parent = self.nodes[id as usize].parent;
        while parent != NONE {
            let p = &mut self.nodes[parent as usize];
            p.size = p.size.saturating_sub(size);
            p.files = p.files.saturating_sub(files);
            parent = p.parent;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Generation.

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        let (u, v) = (self.unit().max(f64::MIN_POSITIVE), self.unit());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }

    /// Log-normal size around `median` bytes with spread `sigma` (natural log units).
    fn size(&mut self, median: f64, sigma: f64) -> u64 {
        (median * (sigma * self.normal()).exp()).clamp(0.0, 64e9) as u64
    }
}

#[derive(Clone, Copy)]
struct Flavor {
    median: f64,
    sigma: f64,
    extensions: &'static [&'static str],
}

const DOCS: Flavor = Flavor { median: 60e3, sigma: 2.0, extensions: &["pdf", "docx", "txt", "md", "pages", "key"] };
const MEDIA: Flavor = Flavor { median: 2.5e6, sigma: 1.6, extensions: &["jpg", "heic", "png", "mov", "mp4"] };
const VIDEO: Flavor = Flavor { median: 150e6, sigma: 1.4, extensions: &["mov", "mp4", "mkv"] };
const MUSIC: Flavor = Flavor { median: 8e6, sigma: 0.6, extensions: &["m4a", "mp3", "flac"] };
const CODE: Flavor = Flavor { median: 6e3, sigma: 1.8, extensions: &["rs", "swift", "ts", "json", "o", "rlib"] };
const APPS: Flavor = Flavor { median: 200e3, sigma: 2.6, extensions: &["dylib", "car", "nib", "plist", "strings"] };
const CACHE: Flavor = Flavor { median: 30e3, sigma: 2.4, extensions: &["db", "cache", "bin", "log", "sqlite"] };

const WORDS: &[&str] = &[
    "Projects",
    "Archive",
    "Assets",
    "Backups",
    "Build",
    "Cache",
    "Clients",
    "Data",
    "Drafts",
    "Exports",
    "Family",
    "Holiday",
    "Imports",
    "Invoices",
    "Library",
    "Media",
    "Notes",
    "Old",
    "Photos",
    "Plugins",
    "Recordings",
    "Resources",
    "Samples",
    "Scans",
    "Shared",
    "Sources",
    "Temp",
    "Travel",
    "Vendor",
    "Work",
];

fn grow(tree: &mut Tree, rng: &mut Rng, parent: u32, budget: usize, depth: u32, flavor: Flavor) {
    if budget == 0 {
        return;
    }
    let max_dirs = if depth >= 9 || budget < 12 { 0 } else { (2 + rng.below(10)) as usize };
    let dirs = max_dirs.min(budget / 6);
    let files = if dirs == 0 { budget } else { (budget / (4 + rng.below(12) as usize)).max(1).min(budget - dirs) };
    for i in 0..files {
        let ext = flavor.extensions[rng.below(flavor.extensions.len() as u64) as usize];
        let word = WORDS[rng.below(WORDS.len() as u64) as usize];
        let name = format!("{word} {:04}{i}.{ext}", rng.below(10_000));
        let size = rng.size(flavor.median, flavor.sigma);
        tree.add(parent, name.as_bytes(), false, size);
    }
    let remaining = budget - files - dirs;
    if dirs == 0 {
        return;
    }
    // Power-law split: a few large folders, many small ones.
    let weights: Vec<f64> = (0..dirs).map(|_| rng.unit().powi(3) + 0.002).collect();
    let total: f64 = weights.iter().sum();
    for (i, weight) in weights.iter().enumerate() {
        let word = WORDS[rng.below(WORDS.len() as u64) as usize];
        let name = if i == 0 { word.to_owned() } else { format!("{word} {i}") };
        let id = tree.add(parent, name.as_bytes(), true, 0);
        let share = ((remaining as f64) * weight / total) as usize;
        grow(tree, rng, id, share, depth + 1, flavor);
    }
}

fn generate(options: &FixtureOptions) -> Tree {
    let mut tree = Tree::default();
    let mut rng = Rng(options.seed | 1);
    let root = tree.add(NONE, ROOT, true, 0);
    // Root path is `/Fixture`, its name `Fixture`.
    tree.nodes[0].name_at = 1;
    tree.nodes[0].folded = fold(b"Fixture").into();
    let n = options.entries;
    let users = tree.add(root, b"Users", true, 0);
    let home = tree.add(users, b"demo", true, 0);
    let plan: [(u32, &str, f64, Flavor); 13] = [
        (root, "Applications", 0.14, APPS),
        (root, "Library", 0.10, CACHE),
        (root, "System", 0.08, APPS),
        (home, "Documents", 0.12, DOCS),
        (home, "Downloads", 0.06, DOCS),
        (home, "Desktop", 0.02, DOCS),
        (home, "Pictures", 0.12, MEDIA),
        (home, "Movies", 0.002, VIDEO),
        (home, "Music", 0.05, MUSIC),
        (home, "Developer", 0.208, CODE),
        (home, "Library", 0.09, CACHE),
        (home, ".Trash", 0.005, MEDIA),
        (users, "Shared", 0.005, DOCS),
    ];
    for (parent, name, share, flavor) in plan {
        let id = tree.add(parent, name.as_bytes(), true, 0);
        grow(&mut tree, &mut rng, id, (n as f64 * share) as usize, 1, flavor);
    }
    // A few single large files so the map has dominant sectors.
    tree.add(home, b"Virtual Machine.utm", false, 48_000_000_000);
    tree.add(root, b"swapfile", false, 6_000_000_000);
    tree.finish();
    tree
}

/// The sample Home of the README screenshots: the Swift app's marketing tree
/// (`scripts/marketing/Capture.swift` at v0.9.15) under [`HOME`].
pub mod sample {
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000 * MB;

    /// The largest Home folder: selected on the Home map, then opened.
    pub const PROJECTS: &str = "Projects";
    /// The largest folder in Projects: selected once Projects is open.
    pub const STUDIO: &str = "Projects/Studio";
    /// The folder that is collected and reviewed.
    pub const DOWNLOADS: &str = "Downloads";

    /// Files (paths relative to Home) and their sizes in bytes; folders are implied.
    pub const FILES: &[(&str, u64)] = &[
        ("Projects/Studio/Footage.mov", 28 * GB),
        ("Projects/Studio/Exports.zip", 12 * GB),
        ("Projects/Studio/Assets.psd", 6 * GB),
        ("Projects/Archive/2024.zip", 15 * GB),
        ("Projects/Archive/2023.zip", 9 * GB),
        ("Projects/Project backup.zip", 8 * GB),
        ("Photos/Photo Library.photoslibrary", 58 * GB),
        ("Photos/RAW archive.zip", 12 * GB),
        ("Downloads/Installer.dmg", 12 * GB),
        ("Downloads/Camera originals.zip", 10 * GB),
        ("Downloads/Archive.zip", 8 * GB),
        ("Music/Library.musiclibrary", 24 * GB),
        ("Music/Live recordings.wav", 10 * GB),
        ("Documents/Work archive.zip", 16 * GB),
        ("Documents/Presentation.pdf", 4 * GB),
        ("Movies/Travel.mov", 15 * GB),
        ("Movies/Family.mov", 8 * GB),
        ("Desktop archive.zip", 9 * GB),
        ("Notes.pdf", 100 * MB),
    ];

    /// The volume: name, capacity and available bytes.
    pub const VOLUME: (&str, u64, u64) = ("Macintosh HD", 1_000 * GB, 342 * GB);

    /// The absolute path of a sample path relative to Home.
    pub fn path(relative: &str) -> Vec<u8> {
        let mut path = super::HOME.to_vec();
        path.push(b'/');
        path.extend_from_slice(relative.as_bytes());
        path
    }
}

fn generate_sample() -> Tree {
    let mut tree = Tree::default();
    let root = tree.add(NONE, ROOT, true, 0);
    tree.nodes[0].name_at = 1;
    tree.nodes[0].folded = fold(b"Fixture").into();
    let users = tree.add(root, b"Users", true, 0);
    let home = tree.add(users, b"demo", true, 0);
    for &(relative, size) in sample::FILES {
        let mut parent = home;
        let mut components = relative.split('/').peekable();
        while let Some(name) = components.next() {
            if components.peek().is_none() {
                tree.add(parent, name.as_bytes(), false, size);
                break;
            }
            let path = [&tree.nodes[parent as usize].path[..], b"/", name.as_bytes()].concat();
            parent = match tree.find(&path) {
                Some(id) => id,
                None => tree.add(parent, name.as_bytes(), true, 0),
            };
        }
    }
    tree.finish();
    tree
}

// ---------------------------------------------------------------------------------------------
// Backend.

type Session = (Vec<u8>, Arc<AtomicBool>);

pub struct FixtureBackend {
    tree: RwLock<Tree>,
    options: FixtureOptions,
    saved: Mutex<HashMap<Vec<u8>, SnapshotInfo>>,
    snapshot_ids: AtomicU64,
    /// Mapping sessions: root and stop flag.
    sessions: Mutex<HashMap<SessionId, Session>>,
    revision: AtomicU64,
}

impl std::fmt::Debug for FixtureBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixtureBackend").field("options", &self.options).finish_non_exhaustive()
    }
}

impl FixtureBackend {
    pub fn new(options: FixtureOptions) -> Self {
        let tree = match options.layout {
            FixtureLayout::Generated => generate(&options),
            FixtureLayout::Sample => generate_sample(),
        };
        let backend = Self {
            tree: RwLock::new(tree),
            options,
            saved: Mutex::default(),
            snapshot_ids: AtomicU64::new(0),
            sessions: Mutex::default(),
            revision: AtomicU64::new(1),
        };
        if backend.options.layout == FixtureLayout::Sample {
            // Already mapped: opening Home publishes this saved map without a mapping run.
            backend.save(HOME);
        }
        backend
    }

    /// Entries generated (files and folders).
    pub fn len(&self) -> usize {
        self.read().nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Tree> {
        self.tree.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Tree> {
        self.tree.write().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, root: &[u8]) -> SnapshotInfo {
        let id = self.snapshot_ids.fetch_add(1, Ordering::Relaxed) + 1;
        let tree = self.read();
        let snapshot = SnapshotInfo {
            id,
            root: Arc::from(root),
            root_identity: tree.find(root).map(Tree::identity),
            indexed_at: SystemTime::now(),
            skipped_count: 0,
            issues: Arc::from([]),
            needs_boot_upgrade: false,
        };
        drop(tree);
        self.saved.lock().unwrap_or_else(|e| e.into_inner()).insert(root.to_vec(), snapshot.clone());
        snapshot
    }

    fn file_count(&self, root: &[u8]) -> u64 {
        let tree = self.read();
        tree.find(root).map_or(0, |id| tree.nodes[id as usize].files)
    }

    fn run_mapping(self: &Arc<Self>, request: MappingRequest, notify: Notifier, stop: Arc<AtomicBool>) {
        let session = request.session;
        let send = |event: MappingEvent| notify(Event::Mapping { session, event });
        let total = self.file_count(&request.root);
        let saved = self.saved.lock().unwrap_or_else(|e| e.into_inner()).get(&request.root[..]).cloned();
        if let Some(snapshot) = saved.as_ref().filter(|_| !request.force) {
            send(MappingEvent::Published { snapshot: snapshot.clone(), reused: true, root_file_count: total });
            return;
        }
        send(MappingEvent::DiscoveryStarted);
        if let Some(snapshot) = saved.filter(|_| request.keep_map) {
            send(MappingEvent::SavedAvailable(snapshot));
        }
        let ticks = (self.options.mapping_duration.as_millis() / self.options.progress_interval.as_millis().max(1))
            .max(1) as u64;
        for tick in 1..=ticks {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let found = total * tick / ticks;
            let progress = ProgressSnapshot {
                revision: tick,
                item_count: found,
                bytes_scanned: 0,
                current_path: request.root.to_vec(),
                discovery_complete: tick == ticks,
                discovery_fraction: Some(tick as f64 / ticks as f64),
                cache_fraction: Some((tick as f64 / ticks as f64 * 0.9).min(1.0)),
                cached_file_count: Some(found * 9 / 10),
                ..ProgressSnapshot::default()
            };
            send(MappingEvent::Progress(progress));
            if tick == 1 || tick % 10 == 0 {
                let revision = self.revision.fetch_add(1, Ordering::Relaxed) + 1;
                send(MappingEvent::PreviewChanged { revision, search_ready: tick > 1 });
            }
            std::thread::sleep(self.options.progress_interval);
        }
        if stop.load(Ordering::Acquire) {
            return;
        }
        let snapshot = self.save(&request.root);
        send(MappingEvent::Published { snapshot, reused: false, root_file_count: total });
    }
}

/// A backend that owns its mapping threads needs an `Arc` of itself.
#[derive(Debug, Clone)]
pub struct SharedFixture(pub Arc<FixtureBackend>);

impl SharedFixture {
    pub fn new(options: FixtureOptions) -> Self {
        Self(Arc::new(FixtureBackend::new(options)))
    }
}

impl Backend for SharedFixture {
    fn resolve(&self, path: &[u8], force: bool, candidate: Option<&ResolveCandidate>, _: &Cancellation) -> Resolution {
        let fixture = &self.0;
        let tree = fixture.read();
        let Some(target) = tree.find(path).map(Tree::identity) else {
            let name = orbit_core::paths::file_name(path);
            return Resolution {
                root: path.to_vec(),
                exclusion: Some(format!(
                    "“{}” couldn’t be opened because there is no such file.",
                    String::from_utf8_lossy(name)
                )),
                ..Resolution::default()
            };
        };
        if let Some(candidate) = candidate.filter(|_| !force)
            && tree.find(&candidate.root).map(Tree::identity) == Some(candidate.identity)
            && is_within(path, &candidate.root)
        {
            return Resolution {
                root: candidate.root.to_vec(),
                root_identity: Some(candidate.identity),
                target_identity: Some(target),
                used_candidate: true,
                exclusion: None,
            };
        }
        if !force {
            let saved = fixture.saved.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(root) = saved.keys().filter(|root| is_within(path, root)).max_by_key(|root| root.len()) {
                return Resolution {
                    root: root.clone(),
                    root_identity: tree.find(root).map(Tree::identity),
                    target_identity: Some(target),
                    used_candidate: false,
                    exclusion: None,
                };
            }
        }
        Resolution {
            root: path.to_vec(),
            root_identity: Some(target),
            target_identity: Some(target),
            ..Default::default()
        }
    }

    fn start_mapping(&self, request: &MappingRequest, notify: Notifier) {
        let stop = Arc::new(AtomicBool::new(false));
        self.0
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request.session, (request.root.to_vec(), Arc::clone(&stop)));
        let fixture = Arc::clone(&self.0);
        let request = request.clone();
        std::thread::Builder::new()
            .name("fixture-mapping".into())
            .spawn(move || fixture.run_mapping(request, notify, stop))
            .expect("spawn fixture mapping thread");
    }

    fn stop_mapping(&self, session: SessionId) {
        if let Some((_, stop)) = self.0.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(&session) {
            stop.store(true, Ordering::Release);
        }
    }

    fn prioritize(&self, _: SessionId, _: &[u8]) {}

    fn view(&self, request: &ViewRequest, _: &Cancellation) -> Result<Option<FolderTree>, String> {
        let tree = self.0.read();
        let preview = request.source == ViewSource::Preview;
        let fallback = request.fallback || !preview;
        let mut path: &[u8] = &request.path;
        let folder = loop {
            if is_within(path, &request.root)
                && let Some(id) = tree.find(path).filter(|&id| tree.nodes[id as usize].is_dir)
            {
                break id;
            }
            if !fallback {
                return Ok(None);
            }
            if path == &request.root[..] || path.len() <= 1 {
                return if preview { Ok(None) } else { Err("The indexed folder is unavailable.".into()) };
            }
            path = orbit_app_state::paths::parent_or_root(path);
        };
        let revision = self.0.revision.load(Ordering::Relaxed);
        let source = if preview { TreeSource::Live } else { TreeSource::Saved };
        Ok(Some(tree.view(&request.root, folder, revision, source)))
    }

    fn discovered_file_count(&self, session: SessionId, _: &[ScanRemoval], _: &Cancellation) -> Result<u64, String> {
        let root =
            self.0.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(&session).map(|(root, _)| root.clone());
        Ok(root.map_or(0, |root| self.0.file_count(&root)))
    }

    fn search(&self, request: &SearchRequest, cancel: &Cancellation) -> Result<SearchReply, String> {
        let tree = self.0.read();
        let needle = fold_query(&request.query);
        let mut hits: Vec<u32> = Vec::new();
        for (id, node) in tree.nodes.iter().enumerate() {
            if id % 8192 == 0 && cancel.is_cancelled() {
                return Err("Search cancelled.".into());
            }
            if !node.removed
                && node.folded.contains(needle.as_str())
                && node.path.len() > request.root.len()
                && is_within(&node.path, &request.root)
            {
                hits.push(id as u32);
            }
        }
        hits.sort_by(|&a, &b| {
            let (a, b) = (&tree.nodes[a as usize], &tree.nodes[b as usize]);
            b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path))
        });
        let total = hits.len() as u64;
        hits.truncate(request.limit);
        Ok(SearchReply {
            hits: hits.into_iter().map(|id| tree.item(id)).collect(),
            total,
            preview_revision: self.0.revision.load(Ordering::Relaxed),
            removals_changed: false,
        })
    }

    fn identity(&self, path: &[u8]) -> Option<FileIdentity> {
        self.0.read().find(path).map(Tree::identity)
    }

    fn rejection_reason(&self, path: &[u8], root: &[u8]) -> Option<String> {
        if path == root {
            return Some("The mapped folder cannot be removed.".into());
        }
        (!is_within(path, root)).then(|| "This item is outside the scanned folder.".to_owned())
    }

    fn remove(&self, request: &RemovalRequest) -> RemovalOutcome {
        let mut outcome = RemovalOutcome::default();
        let mut parents = std::collections::BTreeSet::new();
        let mut tree = self.0.write();
        for item in &request.items {
            let Some(id) = tree.find(&item.path).filter(|&id| Some(Tree::identity(id)) == item.meta.identity) else {
                outcome.failures.push(format!(
                    "{}: the item changed. Remove it from the collection and add it again.",
                    item.display_name()
                ));
                continue;
            };
            if let Some(reason) = self.rejection_reason(&item.path, &request.root) {
                outcome.failures.push(format!("{}: {reason}", item.display_name()));
                continue;
            }
            parents.insert(orbit_app_state::paths::parent_or_root(&item.path).to_vec());
            tree.remove(id);
            outcome.removed.push(Arc::clone(&item.path));
        }
        drop(tree);
        self.0.revision.fetch_add(1, Ordering::Relaxed);
        if parents.len() > request.parent_limit {
            outcome.parents_overflow = true;
        } else {
            outcome.changed_parents = parents.into_iter().collect();
        }
        outcome
    }

    fn refresh(&self, request: &RefreshRequest, _: &Cancellation) -> Result<SnapshotInfo, String> {
        Ok(self.0.save(&request.root))
    }

    fn monitor(&self, _: SessionId, _: &SnapshotInfo, _: Notifier) {}

    fn stop_monitor(&self, _: SessionId) {}

    fn volumes(&self) -> Vec<Volume> {
        if self.0.options.layout == FixtureLayout::Sample {
            let (name, capacity, available) = sample::VOLUME;
            return vec![Volume { path: ROOT.to_vec(), name: name.into(), capacity, available }];
        }
        let used = {
            let tree = self.0.read();
            tree.nodes[0].size
        };
        let capacity = 2_000_000_000_000u64.max(used + used / 4);
        vec![Volume { path: ROOT.to_vec(), name: "Fixture HD".into(), capacity, available: capacity - used }]
    }

    fn directories(&self, paths: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let tree = self.0.read();
        paths.iter().filter(|p| tree.find(p).is_some_and(|id| tree.nodes[id as usize].is_dir)).cloned().collect()
    }
}
