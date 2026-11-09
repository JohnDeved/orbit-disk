use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use orbit_sunburst::{HATCH_SPACING, Layout, LayoutSource, MAX_SEGMENTS, Metrics, Point};

#[derive(Default)]
struct Arena {
    sizes: Vec<u64>,
    children: Vec<Arc<[u32]>>,
}

impl Arena {
    fn node(&mut self, size: u64) -> u32 {
        self.sizes.push(size);
        self.children.push(Arc::from([]));
        (self.sizes.len() - 1) as u32
    }

    fn adopt(&mut self, parent: u32, mut ids: Vec<u32>) {
        ids.sort_by_key(|&id| std::cmp::Reverse(self.sizes[id as usize]));
        self.sizes[parent as usize] = ids.iter().map(|&id| self.sizes[id as usize]).sum();
        self.children[parent as usize] = ids.into();
    }

    /// Folder with `count` children of power-law sizes.
    fn power_law(count: usize, exponent: f64) -> (Self, u32) {
        let mut arena = Self::default();
        let root = arena.node(0);
        let ids = (0..count).map(|i| arena.node((1e12 / (i as f64 + 1.0).powf(exponent)) as u64 + 1)).collect();
        arena.adopt(root, ids);
        (arena, root)
    }

    /// 4 folders × 48 subfolders × 2 folders × 1 file with ±10% size jitter: needs more than the
    /// 768-segment cap, so the cap and the shrinking per-parent quota are exercised.
    fn saturating() -> (Self, u32) {
        let mut arena = Self::default();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut jitter = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            900 + seed % 200
        };
        let root = arena.node(0);
        let folders = (0..4)
            .map(|_| {
                let folder = arena.node(0);
                let subfolders = (0..48)
                    .map(|_| {
                        let subfolder = arena.node(0);
                        let pairs = (0..2)
                            .map(|_| {
                                let inner = arena.node(0);
                                let file = arena.node(jitter());
                                arena.adopt(inner, vec![file]);
                                inner
                            })
                            .collect();
                        arena.adopt(subfolder, pairs);
                        subfolder
                    })
                    .collect();
                arena.adopt(folder, subfolders);
                folder
            })
            .collect();
        arena.adopt(root, folders);
        (arena, root)
    }
}

impl LayoutSource for Arena {
    type Id = u32;

    fn size(&self, id: u32) -> u64 {
        self.sizes[id as usize]
    }

    fn children(&self, id: u32) -> &[u32] {
        &self.children[id as usize]
    }

    fn shared_children(&self, id: u32) -> Option<Arc<[u32]>> {
        Some(Arc::clone(&self.children[id as usize]))
    }
}

fn layout(c: &mut Criterion) {
    let (saturating, saturating_root) = Arena::saturating();
    assert_eq!(Layout::build(&saturating, saturating_root, None).len(), MAX_SEGMENTS);
    c.bench_function("layout/saturating_768", |b| {
        b.iter(|| Layout::build(black_box(&saturating), saturating_root, None))
    });

    let mut flat = Arena::default();
    let flat_root = flat.node(0);
    let ids = (0..1_000_000).map(|_| flat.node(4_096)).collect();
    flat.adopt(flat_root, ids);
    assert_eq!(Layout::build(&flat, flat_root, None).len(), 1);
    c.bench_function("layout/flat_1m_children", |b| b.iter(|| Layout::build(black_box(&flat), flat_root, None)));

    let (power, power_root) = Arena::power_law(100_000, 1.1);
    c.bench_function("layout/power_law_100k_children", |b| {
        b.iter(|| Layout::build(black_box(&power), power_root, None))
    });
}

fn interaction(c: &mut Criterion) {
    let (arena, root) = Arena::saturating();
    let layout = Layout::build(&arena, root, None);
    let metrics = Metrics::new(900.0, 900.0);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut unit = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let points: Vec<Point> = (0..10_000).map(|_| Point::new(unit() * 900.0, unit() * 900.0)).collect();

    let mut group = c.benchmark_group("hit_test");
    group.throughput(Throughput::Elements(points.len() as u64));
    group.bench_function("10k_points_768_segments", |b| {
        b.iter(|| {
            let mut hits = 0usize;
            for &point in &points {
                hits += usize::from(layout.hit_test(&metrics, black_box(point)).is_some());
            }
            hits
        })
    });
    group.finish();

    let mut outline = Vec::new();
    let mut hatch = Vec::new();
    c.bench_function("geometry/outline_and_hatch_768_segments", |b| {
        b.iter(|| {
            let mut count = 0;
            for segment in layout.segments() {
                let sector = segment.sector(&metrics);
                outline.clear();
                hatch.clear();
                sector.flatten(0.25, &mut outline);
                sector.hatch(HATCH_SPACING, &mut hatch);
                count += outline.len() + hatch.len();
            }
            count
        })
    });
}

criterion_group!(benches, layout, interaction);
criterion_main!(benches);
