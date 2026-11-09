//! Criterion benches for the pure algorithm modules of `orbit-core`: the filename accelerator,
//! Name-sort collation and removal projection.
//!
//! `cargo bench -p orbit-bench --bench algorithms`

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use orbit_core::collate::Collator;
use orbit_core::projection::{RemovalProjection, RemovalSource, Totals};
use orbit_core::search::{NameIndex, QueryOptions};
use orbit_core::view::{FolderTree, FolderTreeBuilder, NodeMeta, ViewSource};
use orbit_core::{ScanRemoval, fold};
use orbit_platform::{Cancellation, FileIdentity};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "IMG",
    "DSC",
    "report",
    "Report",
    "final",
    "draft",
    "invoice",
    "photo",
    "backup",
    "node_modules",
    "index",
    "main",
    "README",
    "config",
    "data",
    "test",
    "Screenshot",
    "Untitled",
    "notes",
    "project",
    "budget",
    "résumé",
    "Café",
    "cafe\u{301}",
    "über",
    "日本語",
    "фото",
    "lib",
    "src",
    "build",
    "cache",
    "v2",
    "copy",
    "Archive",
];
const EXTENSIONS: &[&str] = &["jpg", "png", "txt", "pdf", "docx", "rs", "js", "json", "mov", "zip", "dylib", "h"];
const SEPARATORS: &[&str] = &["-", "_", " ", ".", ""];

/// File names shaped like a real home folder: words, separators, counters and extensions, with
/// some mixed case, diacritics (NFC and NFD) and non-Latin scripts.
fn realistic_names(count: usize, seed: u64) -> Vec<Vec<u8>> {
    let mut rng = Rng(seed);
    (0..count)
        .map(|_| {
            let mut name = String::new();
            for i in 0..1 + rng.below(3) {
                if i > 0 {
                    name.push_str(SEPARATORS[rng.below(SEPARATORS.len())]);
                }
                name.push_str(WORDS[rng.below(WORDS.len())]);
            }
            if rng.below(3) > 0 {
                name.push_str(SEPARATORS[rng.below(SEPARATORS.len())]);
                name.push_str(&rng.below(10_000).to_string());
            }
            if rng.below(5) > 0 {
                name.push('.');
                name.push_str(EXTENSIONS[rng.below(EXTENSIONS.len())]);
            }
            name.into_bytes()
        })
        .collect()
}

fn search(c: &mut Criterion) {
    let folded: Vec<(i64, String)> =
        realistic_names(1_000_000, 1).iter().enumerate().map(|(i, n)| (i as i64 + 1, fold::fold(n))).collect();
    let rows = || folded.iter().map(|(id, n)| (*id, n.as_bytes()));
    let mut group = c.benchmark_group("search");
    group.sample_size(10).measurement_time(Duration::from_secs(10));
    // Diverse names need more postings than the default 96 MiB budget admits for 1M rows, so the
    // full-size build and the query benches use an unlimited budget; the capped prefix is reported.
    let unlimited = || {
        let mut index = NameIndex::with_byte_limit(usize::MAX);
        index.extend(rows());
        index
    };
    group.bench_function("build_1m", |b| b.iter(|| black_box(unlimited())));
    group.bench_function("build_default_budget", |b| b.iter(|| black_box(NameIndex::build(rows()))));
    let capped = NameIndex::build(rows());
    let index = unlimited();
    eprintln!(
        "1M-name index: {} MiB retained estimate; default budget keeps {} rows ({} MiB)",
        index.retained_bytes() / (1024 * 1024),
        capped.len(),
        capped.retained_bytes() / (1024 * 1024),
    );
    group.finish();

    let mut group = c.benchmark_group("search/query_1m");
    group.measurement_time(Duration::from_secs(4));
    let options = QueryOptions::default();
    // Common: many matches, falls back to "too many" after counting. Rare: few or no matches.
    for (label, needle) in [
        ("common_3_img", "img"),
        ("common_4_.jpg", ".jpg"),
        ("common_6_report", "report"),
        ("rare_4_9999", "9999"),
        ("rare_5_cafe_", "café_"),
        ("rare_6_zqxjkv", "zqxjkv"),
        ("short_2_xy", "xy"),
    ] {
        let needle = fold::fold_query(needle);
        let count = index.query(needle.as_bytes(), options, || Ok::<_, ()>(())).unwrap().count;
        eprintln!("query {label}: {count} matches");
        group.bench_function(label, |b| {
            b.iter(|| black_box(index.query(black_box(needle.as_bytes()), options, || Ok::<_, ()>(())).unwrap()))
        });
    }
    group.finish();
}

fn collate(c: &mut Criterion) {
    let hybrid = Collator::new();
    let icu_only = Collator::new().with_ascii_fast_path(false);
    let names_1m = realistic_names(1_000_000, 2);
    let ascii_share = names_1m.iter().filter(|n| n.is_ascii()).count() as f64 / names_1m.len() as f64;
    eprintln!("collation corpus: {:.0}% ASCII names", ascii_share * 100.0);
    for (label, count) in [("100k", 100_000), ("1m", 1_000_000)] {
        let names: Vec<&[u8]> = names_1m[..count].iter().map(Vec::as_slice).collect();
        let mut group = c.benchmark_group(format!("collate/sort_{label}"));
        group.sample_size(10).measurement_time(Duration::from_secs(if count > 100_000 { 20 } else { 5 }));
        group.bench_function("icu_compare", |b| {
            b.iter_batched_ref(|| names.clone(), |v| v.sort_by(|x, y| icu_only.compare(x, y)), BatchSize::LargeInput)
        });
        group.bench_function("hybrid_compare", |b| {
            b.iter_batched_ref(|| names.clone(), |v| v.sort_by(|x, y| hybrid.compare(x, y)), BatchSize::LargeInput)
        });
        group.bench_function("sort_keys", |b| {
            b.iter_batched_ref(
                || names.clone(),
                |v| {
                    let mut keyed: Vec<_> = v.iter().map(|n| (hybrid.sort_key(n), *n)).collect();
                    keyed.sort_unstable();
                    black_box(keyed);
                },
                BatchSize::LargeInput,
            )
        });
        group.bench_function("bytes_baseline", |b| {
            b.iter_batched_ref(|| names.clone(), |v| v.sort(), BatchSize::LargeInput)
        });
        group.finish();
    }
}

fn projection(c: &mut Criterion) {
    const CHILDREN: u64 = 100_000;
    let file = |size: u64, ino: u64| NodeMeta {
        size,
        allocated: size,
        file_count: 1,
        identity: Some(FileIdentity { dev: 1, ino }),
        ..NodeMeta::default()
    };
    let mut folder = NodeMeta { size: 0, allocated: 0, file_count: CHILDREN, ..NodeMeta::default() };
    folder.flags.is_dir = true;
    let mut builder = FolderTreeBuilder::new(Arc::from(&b"/m"[..]), b"/m/folder".to_vec(), b"folder", folder);
    builder.push_ancestor(b"m", folder);
    let mut total = 0;
    let mut sources = Vec::new();
    for i in 0..CHILDREN {
        let size = 1 + (i * 7_919) % 100_000;
        total += size;
        let name = format!("file-{i:06}.bin");
        builder.push_child(FolderTree::FOLDER, name.as_bytes(), file(size, i + 10));
        sources.push((format!("/m/folder/{name}").into_bytes(), FileIdentity { dev: 1, ino: i + 10 }, size));
    }
    builder.meta_mut(FolderTree::FOLDER).size = total;
    builder.meta_mut(FolderTree::FOLDER).allocated = total;
    let tree = builder.finish(1, false, ViewSource::Saved);
    let removals: Vec<ScanRemoval> = sources
        .iter()
        .step_by(10)
        .map(|(path, identity, _)| ScanRemoval { path: path.clone(), identity: *identity })
        .collect();
    let cancellation = Cancellation::new();
    let build = || {
        RemovalProjection::new(
            &removals,
            sources.iter().step_by(10).map(|(path, identity, size)| RemovalSource {
                path,
                identity: Some(*identity),
                totals: Totals { size: *size, allocated: *size, file_count: 1 },
            }),
            &cancellation,
        )
        .unwrap()
    };
    let projection = build();
    let projected = projection.project_tree(&tree, &cancellation).unwrap().unwrap();
    assert_eq!(projected.children(FolderTree::FOLDER).len() as u64, CHILDREN - CHILDREN / 10);

    let mut group = c.benchmark_group("projection");
    group.bench_function("new_10k_removals", |b| b.iter(|| black_box(build())));
    group.bench_function("tree_100k_children_10k_removals", |b| {
        b.iter(|| black_box(projection.project_tree(&tree, &cancellation).unwrap()))
    });
    group.finish();
}

criterion_group!(benches, search, collate, projection);
criterion_main!(benches);
