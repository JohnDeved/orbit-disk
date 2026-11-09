//! App-state hot paths: folder preparation, hover relations on huge groups, collection coverage
//! and installing a preview update of a large folder.

use std::hint::black_box;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use orbit_app_state::backend::{
    Backend, MappingRequest, Notifier, RefreshRequest, RemovalOutcome, RemovalRequest, Resolution, ResolveCandidate,
    SearchReply, SearchRequest, ViewRequest,
};
use orbit_app_state::collection::CollectionSnapshot;
use orbit_app_state::event::MappingEvent;
use orbit_app_state::model::{Item, ScanRemoval, SnapshotInfo, Volume};
use orbit_app_state::{AppState, Config, Effect, Event, Reply, Request, SessionId, perform, prepare_folder};
use orbit_core::view::{FolderTree, FolderTreeBuilder, NodeFlags, NodeMeta, ViewSource};
use orbit_platform::{Cancellation, FileIdentity};
use orbit_sunburst::Layout;

const ROOT: &[u8] = b"/bench";

fn meta(size: u64, ino: u64, is_dir: bool) -> NodeMeta {
    NodeMeta {
        flags: NodeFlags { is_dir, is_symlink: false, is_mount_point: false, accessible: true },
        size,
        allocated: size,
        raw_allocated: size,
        file_count: 1,
        identity: Some(FileIdentity { dev: 1, ino }),
        state: Default::default(),
    }
}

/// A folder with `children` files of power-law sizes (Swift's large-folder fixture shape).
fn folder(children: usize, revision: u64) -> FolderTree {
    let total: u64 = (0..children).map(size).sum();
    let mut builder = FolderTreeBuilder::new(Arc::from(ROOT), ROOT.to_vec(), b"bench", meta(total, 1, true));
    for i in 0..children {
        builder.push_child(
            FolderTree::FOLDER,
            format!("file-{i:07}.dat").as_bytes(),
            meta(size(i), i as u64 + 2, false),
        );
    }
    builder.finish(revision, true, ViewSource::Live)
}

fn size(i: usize) -> u64 {
    (1e9 / (i as f64 + 1.0).powf(1.1)) as u64 + 1
}

/// Serves one prepared tree for every view; everything else is inert.
struct BenchBackend {
    tree: Mutex<Option<FolderTree>>,
}

impl Backend for BenchBackend {
    fn resolve(&self, path: &[u8], _: bool, _: Option<&ResolveCandidate>, _: &Cancellation) -> Resolution {
        let identity = Some(FileIdentity { dev: 1, ino: 1 });
        Resolution { root: path.to_vec(), root_identity: identity, target_identity: identity, ..Resolution::default() }
    }
    fn start_mapping(&self, _: &MappingRequest, _: Notifier) {}
    fn stop_mapping(&self, _: SessionId) {}
    fn prioritize(&self, _: SessionId, _: &[u8]) {}
    fn view(&self, _: &ViewRequest, _: &Cancellation) -> Result<Option<FolderTree>, String> {
        Ok(self.tree.lock().unwrap().take())
    }
    fn discovered_file_count(&self, _: SessionId, _: &[ScanRemoval], _: &Cancellation) -> Result<u64, String> {
        Ok(0)
    }
    fn search(&self, _: &SearchRequest, _: &Cancellation) -> Result<SearchReply, String> {
        Ok(SearchReply::default())
    }
    fn identity(&self, _: &[u8]) -> Option<FileIdentity> {
        None
    }
    fn rejection_reason(&self, _: &[u8], _: &[u8]) -> Option<String> {
        None
    }
    fn remove(&self, _: &RemovalRequest) -> RemovalOutcome {
        RemovalOutcome::default()
    }
    fn refresh(&self, _: &RefreshRequest, _: &Cancellation) -> Result<SnapshotInfo, String> {
        Err("unsupported".into())
    }
    fn monitor(&self, _: SessionId, _: &SnapshotInfo, _: Notifier) {}
    fn stop_monitor(&self, _: SessionId) {}
    fn volumes(&self) -> Vec<Volume> {
        Vec::new()
    }
    fn directories(&self, _: &[Vec<u8>]) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

/// Runs effects synchronously until no backend work is left.
fn drain(state: &mut AppState, backend: &BenchBackend, effects: Vec<Effect>) {
    let notify: Notifier = Arc::new(|_| {});
    let mut queue = effects;
    while let Some(effect) = queue.pop() {
        if let Effect::Backend { id, request } = effect
            && let Some(event) = perform(backend, id, request, &Cancellation::new(), &notify)
        {
            queue.extend(state.handle(event, Instant::now()));
        }
    }
}

/// An app state showing the live preview of a folder with `children` rows.
fn showing(children: usize) -> (AppState, BenchBackend, SessionId) {
    let backend = BenchBackend { tree: Mutex::new(Some(folder(children, 1))) };
    let mut state = AppState::new(Config::new(b"/Users/bench".to_vec()), Instant::now());
    let effects = state.handle(Event::OpenLocation { path: ROOT.to_vec(), force: false }, Instant::now());
    drain(&mut state, &backend, effects);
    let session = state.active_session_id().expect("session");
    assert_eq!(state.row_count(), children);
    (state, backend, session)
}

fn prepare(c: &mut Criterion) {
    let tree = Arc::new(folder(100_000, 1));
    c.bench_function("prepare 100k-child folder (rows + ordinals + layout)", |b| {
        b.iter(|| {
            let prepared = prepare_folder(Arc::clone(&tree), None, None).unwrap();
            let colors = prepared.layout_colors(&Default::default());
            black_box(Layout::build(&prepared, FolderTree::FOLDER, Some(&colors)));
        });
    });
}

fn hover(c: &mut Criterion) {
    let (mut state, _backend, _) = showing(1_000_000);
    let presentation = state.presentation().unwrap();
    let group = (0..presentation.layout.len()).find(|&i| presentation.layout.group(i).is_some()).unwrap();
    let node = (0..presentation.layout.len()).find(|&i| presentation.layout.segments()[i].node.is_some()).unwrap();
    assert!(presentation.layout.group(group).unwrap().members.len() > 900_000);
    let revision = presentation.layout_revision;
    let mut flip = false;
    c.bench_function("hover a 1M-member group (relations + 1k row checks)", |b| {
        b.iter(|| {
            flip = !flip;
            let segment = if flip { group } else { node };
            state.handle(Event::HoverSegment { segment, layout_revision: revision }, Instant::now());
            let related = (0..1_000).filter(|&row| state.is_row_related(row * 997)).count();
            black_box((related, state.first_related_row()));
        });
    });
}

fn coverage(c: &mut Criterion) {
    let items: Vec<Item> = (0..10_000)
        .map(|i| {
            Item::new(format!("/Users/bench/p{}/q{}/file-{i}.dat", i % 97, i % 13).into_bytes(), meta(i, i, false))
        })
        .collect();
    c.bench_function("collection coverage update, 10k items", |b| {
        b.iter_batched(|| items.clone(), |items| black_box(CollectionSnapshot::new(items, 1)), BatchSize::LargeInput);
    });
}

fn preview_update(c: &mut Criterion) {
    let (mut state, backend, session) = showing(100_000);
    let mut revision = 1;
    let notify: Notifier = Arc::new(|_| {});
    let mut group = c.benchmark_group("preview update");
    group.sample_size(20);
    group.bench_function("apply preview update to a 100k-row folder (install + layout)", |b| {
        b.iter_custom(|iterations| {
            let mut measured = std::time::Duration::ZERO;
            for _ in 0..iterations {
                revision += 1;
                *backend.tree.lock().unwrap() = Some(folder(100_000, revision));
                let event =
                    Event::Mapping { session, event: MappingEvent::PreviewChanged { revision, search_ready: true } };
                let effects = state.handle(event, Instant::now());
                // Read and prepare off the measured path (the host does this on a background thread).
                let reply = effects
                    .into_iter()
                    .find_map(|effect| match effect {
                        Effect::Backend { id, request: request @ Request::View { .. } } => {
                            perform(&backend, id, request, &Cancellation::new(), &notify)
                        }
                        _ => None,
                    })
                    .expect("a preview read");
                assert!(matches!(reply, Event::Backend { reply: Reply::View(_), .. }));
                let started = Instant::now();
                state.handle(reply, Instant::now());
                black_box(state.presentation());
                measured += started.elapsed();
            }
            measured
        });
    });
    group.finish();
}

criterion_group!(benches, prepare, hover, coverage, preview_update);
criterion_main!(benches);
