//! Layout invariants, reference parity with the Swift algorithm, and geometry/hit-test agreement
//! with independent brute-force predicates.

use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, TAU};
use std::sync::Arc;

use crate::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Default)]
struct Tree {
    sizes: Vec<u64>,
    children: Vec<Arc<[u32]>>,
    complete: Vec<bool>,
    share: bool,
}

impl Tree {
    fn node(&mut self, size: u64) -> u32 {
        self.sizes.push(size);
        self.children.push(Arc::from([]));
        self.complete.push(true);
        (self.sizes.len() - 1) as u32
    }

    /// Sets children (sorted largest first unless `sorted` is false) and returns their size sum.
    fn adopt(&mut self, parent: u32, mut ids: Vec<u32>, sorted: bool) -> u64 {
        if sorted {
            ids.sort_by_key(|&id| std::cmp::Reverse(self.sizes[id as usize]));
        }
        let sum = ids.iter().fold(0u64, |sum, &id| sum.saturating_add(self.sizes[id as usize]));
        self.children[parent as usize] = ids.into();
        sum
    }

    fn flat(sizes: &[u64]) -> (Self, u32) {
        let mut tree = Self { share: true, ..Self::default() };
        let root = tree.node(0);
        let ids = sizes.iter().map(|&size| tree.node(size)).collect();
        tree.sizes[root as usize] = tree.adopt(root, ids, true).max(1);
        (tree, root)
    }
}

impl LayoutSource for Tree {
    type Id = u32;

    fn size(&self, id: u32) -> u64 {
        self.sizes[id as usize]
    }

    fn children(&self, id: u32) -> &[u32] {
        &self.children[id as usize]
    }

    fn shared_children(&self, id: u32) -> Option<Arc<[u32]>> {
        self.share.then(|| Arc::clone(&self.children[id as usize]))
    }

    fn is_complete(&self, id: u32) -> bool {
        self.complete[id as usize]
    }
}

/// Random tree mixing equal, power-law and huge sizes, zero-size entries, occasional unsorted child
/// lists and parents whose recorded size disagrees with their children (mid-scan).
fn random_tree(rng: &mut Rng) -> (Tree, u32) {
    let mut tree = Tree { share: rng.below(2) == 0, ..Tree::default() };
    let root = tree.node(0);
    let mut budget = 6_000usize;
    fn grow(tree: &mut Tree, rng: &mut Rng, parent: u32, depth: usize, budget: &mut usize) -> u64 {
        let count = match rng.below(6) {
            0 => rng.below(4),
            1 => 40 + rng.below(20),
            2 => rng.below(1_600),
            _ => rng.below(80),
        } as usize;
        let count = count.min(*budget);
        *budget -= count;
        let mode = rng.below(4);
        let mut ids = Vec::with_capacity(count);
        for index in 0..count {
            let size = match mode {
                0 => 100,
                1 => 1_000_000 / (index as u64 + 1),
                2 => rng.below(1_000) + 1,
                _ => u64::MAX / 3,
            };
            let size = if rng.below(25) == 0 { 0 } else { size };
            let id = tree.node(size);
            tree.complete[id as usize] = rng.below(9) != 0;
            ids.push(id);
        }
        for &id in &ids {
            if depth < 5 && rng.below(3) == 0 && *budget > 0 {
                let sum = grow(tree, rng, id, depth + 1, budget);
                tree.sizes[id as usize] = match rng.below(4) {
                    0 => sum / 2,
                    1 => sum.saturating_add(rng.below(1_000)),
                    _ => sum,
                };
            }
        }
        let sorted = rng.below(8) != 0;
        tree.adopt(parent, ids, sorted)
    }
    let sum = grow(&mut tree, rng, root, 0, &mut budget);
    tree.sizes[root as usize] = sum.max(1);
    (tree, root)
}

#[derive(Debug, PartialEq)]
struct RefSegment {
    depth: u8,
    start: f64,
    end: f64,
    size: u64,
    node: Option<u32>,
    branch: Option<u32>,
    group_parent: Option<u32>,
    members: Vec<u32>,
}

/// Line-by-line port of Swift `SunburstLayout.make` without its fast path, as the reference.
fn reference(tree: &Tree, root: u32, colors: Option<&ColorOrdinals<u32>>) -> Vec<RefSegment> {
    let mut result = Vec::new();
    if tree.size(root) == 0 {
        return result;
    }
    let mut queue = vec![(root, 0.0, TAU, 0u8, None::<u32>)];
    let mut next = 0;
    while next < queue.len() && result.len() < 768 {
        let (id, start, end, depth, color) = queue[next];
        next += 1;
        let children = tree.children(id);
        if depth >= 4 || children.is_empty() {
            continue;
        }
        let total: f64 = children.iter().map(|&child| tree.size(child) as f64).sum();
        let denominator = (tree.size(id) as f64).max(total);
        if denominator <= 0.0 {
            continue;
        }
        let sweep = end - start;
        let remaining = 48.min(768usize.saturating_sub(result.len() + 1));
        let mut cursor = start;
        let (mut grouped_size, mut members, mut visible) = (0u64, Vec::new(), 0);
        for (index, &child) in children.iter().enumerate() {
            let size = tree.size(child);
            if size == 0 {
                continue;
            }
            let angle = sweep * size as f64 / denominator;
            if angle < 0.014 || visible >= remaining {
                grouped_size = grouped_size.saturating_add(size);
                members.push(child);
                continue;
            }
            let color = color.or_else(|| colors.and_then(|c| c.get(child))).unwrap_or(index as u32);
            result.push(RefSegment {
                depth,
                start: cursor,
                end: cursor + angle,
                size,
                node: Some(child),
                branch: Some(color),
                group_parent: None,
                members: vec![child],
            });
            if depth < 3 {
                queue.push((child, cursor, cursor + angle, depth + 1, Some(color)));
            }
            cursor += angle;
            visible += 1;
        }
        if grouped_size > 0 {
            result.push(RefSegment {
                depth,
                start: cursor,
                end: cursor + sweep * grouped_size as f64 / denominator,
                size: grouped_size,
                node: None,
                branch: color,
                group_parent: Some(id),
                members,
            });
        }
    }
    result
}

fn assert_matches_reference(tree: &Tree, root: u32, colors: Option<&ColorOrdinals<u32>>, label: &str) -> Layout<u32> {
    let layout = Layout::build(tree, root, colors);
    let expected = reference(tree, root, colors);
    assert_eq!(layout.len(), expected.len(), "{label}: segment count");
    for (index, (actual, expected)) in layout.segments().iter().zip(&expected).enumerate() {
        let context = format!("{label}: segment {index}");
        assert_eq!(actual.depth, expected.depth, "{context}");
        assert_eq!(actual.size, expected.size, "{context}");
        assert_eq!(actual.node, expected.node, "{context}");
        assert_eq!(actual.branch, expected.branch, "{context}");
        assert!((actual.start - expected.start).abs() < 1e-9, "{context}: start");
        assert!((actual.end - expected.end).abs() < 1e-9, "{context}: end");
        let group = layout.group(index);
        assert_eq!(group.map(|group| group.parent), expected.group_parent, "{context}");
        if let Some(group) = group {
            assert_eq!(group.members.as_slice(), expected.members.as_slice(), "{context}: members");
            let complete = expected.members.iter().all(|&member| tree.is_complete(member));
            assert_eq!(group.complete, complete, "{context}: complete");
            assert_eq!(group.covers_all_children, expected.members.len() == tree.children(group.parent).len());
            assert_eq!(layout.group_of_parent(group.parent), Some(index));
        }
        for &member in &expected.members {
            assert_eq!(layout.index_of(member), Some(index), "{context}: lookup");
            if let Some(group) = group {
                assert!(layout.group_contains(index, member), "{context}: membership");
                assert_eq!(layout.index_of_child(member, group.parent), Some(index));
            }
        }
    }
    layout
}

/// Structural invariants that hold for every layout, independent of the reference.
fn assert_invariants(tree: &Tree, root: u32, layout: &Layout<u32>) {
    assert!(layout.len() <= MAX_SEGMENTS);
    let mut spans: HashMap<u32, (f64, f64)> = HashMap::from([(root, (0.0, TAU))]);
    let mut seen = HashSet::new();
    let mut visible_per_parent: HashMap<u32, usize> = HashMap::new();
    let mut parent_of = HashMap::new();
    for (id, children) in tree.children.iter().enumerate() {
        for &child in children.iter() {
            parent_of.insert(child, id as u32);
        }
    }
    for depth in 0..RING_COUNT {
        let ring = layout.ring(depth);
        for index in ring.clone() {
            let segment = &layout.segments()[index];
            assert_eq!(usize::from(segment.depth), depth);
            assert!(segment.start <= segment.end, "segment {index} is inverted");
            if index > ring.start {
                assert!(layout.segments()[index - 1].end <= segment.start, "ring {depth} not ordered at {index}");
            }
            let parent = match (segment.node, layout.group(index)) {
                (Some(node), None) => parent_of[&node],
                (None, Some(group)) => group.parent,
                _ => panic!("segment {index} must be exactly one of node or group"),
            };
            let &(start, end) = spans.get(&parent).expect("parent must be laid out on the previous ring");
            assert!(segment.start >= start && segment.end <= end, "segment {index} escapes its parent's span");
            if let Some(node) = segment.node {
                assert!(seen.insert(node), "node {node} laid out twice");
                *visible_per_parent.entry(parent).or_default() += 1;
                spans.insert(node, (segment.start, segment.end));
            } else {
                for &member in layout.group(index).unwrap().members.iter() {
                    assert!(seen.insert(member), "member {member} laid out twice");
                    assert!(tree.size(member) > 0);
                    assert_eq!(parent_of[&member], parent);
                }
            }
        }
    }
    assert!(visible_per_parent.values().all(|&count| count <= MAX_VISIBLE_CHILDREN));
}

#[test]
fn layout_matches_swift_reference_and_invariants_on_random_trees() {
    let mut rng = Rng(0x0129_482d);
    for sample in 0..120 {
        let (tree, root) = random_tree(&mut rng);
        let colors = (sample % 3 == 0).then(|| {
            let mut ids = tree.children(root).to_vec();
            ids.reverse();
            ColorOrdinals::for_children(&ids)
        });
        let layout = assert_matches_reference(&tree, root, colors.as_ref(), &format!("random {sample}"));
        assert_invariants(&tree, root, &layout);
    }
}

#[test]
fn layout_matches_reference_at_swift_root_group_boundaries() {
    // Ported from OrbitRootGroupChecks: counts around the 1,024 fast-path cutoff, the largest child
    // straddling the 0.014 rad threshold, a zero-size child, and saturating sizes.
    for count in [0, 1, 71, 72, 1_023, 1_024, 1_025, 1_500] {
        let (tree, root) = Tree::flat(&vec![100; count]);
        assert_matches_reference(&tree, root, None, &format!("flat {count}"));
    }
    for largest in [2_227, 2_228, 2_229, 2_230] {
        let mut sizes = vec![1; 1_024];
        sizes[0] = largest;
        let (mut tree, root) = Tree::flat(&sizes);
        tree.sizes[root as usize] = 1_000_000;
        let layout = assert_matches_reference(&tree, root, None, &format!("threshold {largest}"));
        assert_eq!(layout.len(), if largest <= 2_228 { 1 } else { 2 }, "threshold {largest}");
    }
    let mut sizes = vec![1; 1_024];
    sizes[500] = 0;
    let (tree, root) = Tree::flat(&sizes);
    let layout = assert_matches_reference(&tree, root, None, "zero-size child");
    assert!(!layout.group(0).unwrap().covers_all_children);
    let (tree, root) = Tree::flat(&vec![u64::MAX; 1_025]);
    let layout = assert_matches_reference(&tree, root, None, "saturated");
    assert_eq!(layout.segments().iter().map(|segment| segment.size).max(), Some(u64::MAX));
}

#[test]
fn whole_folder_group_aliases_the_child_list_and_resolves_without_member_index() {
    let (tree, root) = Tree::flat(&vec![7; 5_000]);
    let layout = assert_matches_reference(&tree, root, None, "all tiny");
    assert_eq!(layout.len(), 1);
    let group = layout.group(0).unwrap();
    let (backing, range) = group.members.backing();
    assert!(Arc::ptr_eq(backing, &tree.children[root as usize]) && range == (0..5_000));
    assert!(group.covers_all_children);

    let fresh = Layout::build(&tree, root, None);
    let child = tree.children(root)[4_321];
    assert_eq!(fresh.index_of_child(child, root), Some(0));
    assert!(fresh.members.get().is_none(), "child lookup must not build the member index");
    assert_eq!(fresh.index_of(child), Some(0));

    // Without a shared list the members are copied once, still exactly.
    let copied = Tree { share: false, ..tree };
    let layout = Layout::build(&copied, root, None);
    assert_eq!(layout.group(0).unwrap().members.as_slice(), copied.children(root));
}

#[test]
fn visible_children_are_capped_at_48_per_parent_and_768_overall() {
    let (tree, root) = Tree::flat(&vec![10; 100]);
    let layout = Layout::build(&tree, root, None);
    assert_eq!(layout.len(), 49);
    let group = layout.group(48).unwrap();
    assert_eq!(group.members.as_slice(), &tree.children(root)[48..]);
    assert_eq!(layout.segments()[48].size, 520);

    let (tree, root) = saturating_tree();
    let layout = assert_matches_reference(&tree, root, None, "saturating");
    assert_eq!(layout.len(), MAX_SEGMENTS);
    assert_invariants(&tree, root, &layout);

    let (tree, root) = zipf_tree();
    let layout = assert_matches_reference(&tree, root, None, "zipf");
    assert_invariants(&tree, root, &layout);
}

#[test]
fn color_ordinals_stay_stable_across_reorder_add_and_remove() {
    let mut tree = Tree::default();
    let root = tree.node(0);
    let a = tree.node(200);
    let b = tree.node(100);
    let c = tree.node(50);
    let nested = tree.node(10);
    tree.sizes[root as usize] = tree.adopt(root, vec![a, b], true);
    let mut ordinals = ColorOrdinals::for_children(tree.children(root));
    assert_eq!((ordinals.get(a), ordinals.get(b)), (Some(0), Some(1)));

    // b outgrows a, c appears, then a disappears and comes back: nobody changes hue.
    tree.sizes[b as usize] = 400;
    tree.sizes[a as usize] = 10;
    tree.adopt(a, vec![nested], true);
    tree.sizes[root as usize] = tree.adopt(root, vec![a, b, c], true);
    assert!(ordinals.extend(tree.children(root)));
    assert!(!ordinals.extend(&[b, c]));
    assert!(!ordinals.extend(&[a, b, c]));
    assert_eq!((ordinals.get(a), ordinals.get(b), ordinals.get(c)), (Some(0), Some(1), Some(2)));

    let layout = Layout::build(&tree, root, Some(&ordinals));
    let branch = |id| layout.segments()[layout.index_of(id).unwrap()].branch;
    assert_eq!((branch(b), branch(a), branch(c), branch(nested)), (Some(1), Some(0), Some(2), Some(0)));
    let colors: HashSet<_> = [a, b, c].map(|id| layout.segments()[layout.index_of(id).unwrap()].color()).into();
    assert_eq!(colors.len(), 3);
    assert_eq!(layout.segments()[layout.index_of(nested).unwrap()].color().hue_index, 0);
}

/// Independent point-in-sector predicate: squared radii and cross products instead of atan2.
fn brute_force_contains(metrics: &Metrics, segment: &Segment<u32>, point: Point) -> bool {
    let depth = f64::from(segment.depth);
    let inner = metrics.inner_radius + depth * metrics.ring_width + 1.4;
    let outer = inner + (metrics.ring_width - 2.8).max(1.0);
    let inset = ((segment.end - segment.start) * 0.17).min(1.2 / ((inner + outer) / 2.0));
    let (from, to) = (segment.start + inset, segment.end - inset);
    let (dx, dy) = (point.x - metrics.center.x, point.y - metrics.center.y);
    let distance = dx * dx + dy * dy;
    if distance < inner * inner || distance > outer * outer || from > to {
        return false;
    }
    let ray = |angle: f64| (angle.sin(), -angle.cos());
    let cross = |a: (f64, f64), b: (f64, f64)| a.0 * b.1 - a.1 * b.0;
    let (first, last, d) = (ray(from), ray(to), (dx, dy));
    if to - from <= PI {
        cross(first, d) >= 0.0 && cross(d, last) >= 0.0
    } else {
        !(cross(last, d) > 0.0 && cross(d, first) > 0.0)
    }
}

fn brute_force_hit(layout: &Layout<u32>, metrics: &Metrics, point: Point) -> Option<usize> {
    layout.segments().iter().position(|segment| brute_force_contains(metrics, segment, point))
}

/// 4 folders × 48 subfolders × 2 folders × 1 file would need 4 + 192 + 384 + 384 segments; the
/// per-parent quota shrinks as the global cap approaches, so the last parents only get a group.
fn saturating_tree() -> (Tree, u32) {
    let mut tree = Tree::default();
    let root = tree.node(0);
    let mut level = Vec::new();
    for _ in 0..4 {
        let folder = tree.node(0);
        let mut subfolders = Vec::new();
        for _ in 0..48 {
            let subfolder = tree.node(0);
            let pairs = (0..2)
                .map(|_| {
                    let inner = tree.node(0);
                    let file = tree.node(1_000);
                    tree.sizes[inner as usize] = tree.adopt(inner, vec![file], true);
                    inner
                })
                .collect();
            tree.sizes[subfolder as usize] = tree.adopt(subfolder, pairs, true);
            subfolders.push(subfolder);
        }
        tree.sizes[folder as usize] = tree.adopt(folder, subfolders, true);
        level.push(folder);
    }
    tree.sizes[root as usize] = tree.adopt(root, level, true);
    (tree, root)
}

/// Four levels of Zipf-sized folders (fan-out 16/10/8/6): ~470 segments of widely varying sweep
/// on every ring.
fn zipf_tree() -> (Tree, u32) {
    fn grow(tree: &mut Tree, depth: usize, rank: usize) -> u32 {
        const FANOUT: [usize; 4] = [16, 10, 8, 6];
        let folder = tree.node(0);
        let ids = (0..FANOUT[depth])
            .map(|i| if depth == 3 { tree.node(1_000_000 / (i as u64 + 1)) } else { grow(tree, depth + 1, i) })
            .collect();
        let sum = tree.adopt(folder, ids, true);
        tree.sizes[folder as usize] = sum / (rank as u64 / 2 + 1) + 1;
        folder
    }
    let mut tree = Tree { share: true, ..Tree::default() };
    let root = grow(&mut tree, 0, 0);
    for parent in 0..tree.children.len() {
        let ids = tree.children[parent].to_vec();
        tree.adopt(parent as u32, ids, true);
    }
    (tree, root)
}

fn fixture_layouts() -> Vec<(Tree, u32)> {
    // Swift interaction fixture: one large file and 40 tiny ones.
    let mut sizes = vec![10_000];
    sizes.extend([1; 40]);
    let mut layouts = vec![Tree::flat(&sizes)];
    // 24 equal sectors on each of three rings.
    let mut tree = Tree::default();
    let root = tree.node(0);
    let level0 = (0..24)
        .map(|_| {
            let folder = tree.node(0);
            let files = (0..1)
                .map(|_| {
                    let inner = tree.node(0);
                    let leaf = tree.node(1);
                    tree.sizes[inner as usize] = tree.adopt(inner, vec![leaf], true);
                    inner
                })
                .collect();
            tree.sizes[folder as usize] = tree.adopt(folder, files, true);
            folder
        })
        .collect();
    tree.sizes[root as usize] = tree.adopt(root, level0, true);
    layouts.push((tree, root));
    layouts.push(zipf_tree());
    layouts.push(saturating_tree());
    layouts
}

#[test]
fn hit_test_agrees_with_brute_force_on_random_and_boundary_points() {
    let mut rng = Rng(42);
    let all_metrics =
        [Metrics::new(400.0, 400.0), Metrics::from_bounds(30.0, 40.0, 360.0, 210.0), Metrics::new(10.0, 10.0)];
    for (tree, root) in fixture_layouts() {
        let layout = Layout::build(&tree, root, None);
        assert!(!layout.is_empty());
        for metrics in &all_metrics {
            let reach = metrics.outer_radius() + 10.0;
            let mut probes: Vec<Point> = (0..6_000)
                .map(|_| {
                    Point::new(
                        metrics.center.x + (rng.unit() * 2.0 - 1.0) * reach,
                        metrics.center.y + (rng.unit() * 2.0 - 1.0) * reach,
                    )
                })
                .collect();
            // Just inside and outside every radial and angular boundary.
            for segment in layout.segments().iter().take(200) {
                let sector = segment.sector(metrics);
                let eps = 1e-7;
                for theta in [sector.start - eps, sector.start + eps, sector.end - eps, sector.end + eps] {
                    for radius in [
                        sector.inner_radius - eps,
                        sector.inner_radius + eps,
                        sector.mid_radius(),
                        sector.outer_radius - eps,
                        sector.outer_radius + eps,
                    ] {
                        probes.push(Point::new(
                            metrics.center.x + radius * theta.sin(),
                            metrics.center.y - radius * theta.cos(),
                        ));
                    }
                }
            }
            for point in probes {
                let expected = brute_force_hit(&layout, metrics, point);
                assert_eq!(layout.hit_test(metrics, point), expected, "{point:?} at {metrics:?}");
                if let Some(index) = expected {
                    assert!(layout.sector(metrics, index).contains(point));
                }
            }
            assert_eq!(layout.hit(metrics, metrics.center), Hit::Center);
            assert_eq!(layout.hit_test(metrics, metrics.center), None);
            assert_eq!(layout.hit(metrics, Point::new(f64::NAN, 0.0)), Hit::None);
        }
    }
}

fn line_point(segment: &LineSegment, t: f64) -> Point {
    Point::new(
        segment.from.x + (segment.to.x - segment.from.x) * t,
        segment.from.y + (segment.to.y - segment.from.y) * t,
    )
}

#[test]
fn hatch_lines_are_exactly_the_sector_part_of_each_diagonal() {
    let (tree, root) = &fixture_layouts()[2];
    let layout = Layout::build(tree, *root, None);
    let mut lines = Vec::new();
    let mut checked_pieces = 0;
    for metrics in [Metrics::new(800.0, 640.0), Metrics::new(120.0, 120.0)] {
        for (index, segment) in layout.segments().iter().enumerate() {
            let sector = segment.sector(&metrics);
            lines.clear();
            sector.hatch(HATCH_SPACING, &mut lines);
            let inside = |point: Point| brute_force_contains(&metrics, segment, point);
            for piece in &lines {
                let length = (piece.to.x - piece.from.x).hypot(piece.to.y - piece.from.y);
                assert!(length > 0.0);
                assert!((piece.from.x - piece.from.y - (piece.to.x - piece.to.y)).abs() < 1e-9, "not diagonal");
                for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                    let eps = if t == 0.0 {
                        1e-7 / length
                    } else if t == 1.0 {
                        -1e-7 / length
                    } else {
                        0.0
                    };
                    assert!(inside(line_point(piece, t + eps)), "segment {index}: hatch leaves the sector");
                }
                // Maximal: just past either end is outside.
                assert!(!inside(line_point(piece, -1e-6 / length)), "segment {index}: clipped short at start");
                assert!(!inside(line_point(piece, 1.0 + 1e-6 / length)), "segment {index}: clipped short at end");
                checked_pieces += 1;
            }
            // Complete: sampling the Swift stride of full-height lines finds no uncovered inside point.
            let bounds = sector.bounds();
            let height = bounds.max.y - bounds.min.y;
            let mut x = bounds.min.x - height;
            while x <= bounds.max.x {
                for step in 1..64 {
                    let t = f64::from(step) / 64.0;
                    let point = Point::new(x + t * height, bounds.min.y + t * height);
                    let covered = lines.iter().any(|piece| {
                        let (lo, hi) = (piece.from.x.min(piece.to.x), piece.from.x.max(piece.to.x));
                        (piece.from.x - piece.from.y - (point.x - point.y)).abs() < 1e-9
                            && point.x >= lo - 1e-9
                            && point.x <= hi + 1e-9
                    });
                    if inside(point) {
                        assert!(covered, "segment {index}: hatch misses {point:?}");
                    }
                }
                x += HATCH_SPACING;
            }
        }
    }
    assert!(checked_pieces > 1_000);
}

#[test]
fn outline_flattening_stays_within_tolerance_and_badges_need_room() {
    let (tree, root) = Tree::flat(&[10_000].into_iter().chain([1; 40]).collect::<Vec<_>>());
    let layout = Layout::build(&tree, root, None);
    let metrics = Metrics::new(400.0, 400.0);
    let (large, group) = (layout.sector(&metrics, 0), layout.sector(&metrics, 1));
    assert!(layout.group(1).is_some());

    for tolerance in [0.05, 0.5] {
        let mut points = Vec::new();
        large.flatten(tolerance, &mut points);
        let outer = large.outer_arc();
        let inner = large.inner_arc();
        assert_eq!(points.first(), Some(&outer.start_point()));
        assert_eq!(points.last(), Some(&inner.end_point()));
        for pair in points.windows(2) {
            let mid = Point::new((pair[0].x + pair[1].x) / 2.0, (pair[0].y + pair[1].y) / 2.0);
            let radius = (mid.x - metrics.center.x).hypot(mid.y - metrics.center.y);
            let off_outer = large.outer_radius - radius;
            let off_inner = large.inner_radius - radius;
            // Chords sag inward; the one radial edge between the arcs is exact.
            let on_edge = off_outer > tolerance + 1e-9 && off_inner < -1e-9;
            assert!(on_edge || off_outer.abs() <= tolerance + 1e-9 || off_inner.abs() <= tolerance + 1e-9);
        }
    }

    // Swift collectionMarkerCenter: the tiny group has no room for a 16pt badge, the large file does.
    assert_eq!(group.badge_center(16.0), None);
    let badge = large.badge_center(16.0).unwrap();
    assert!(large.contains(badge));
}

#[test]
fn collection_change_keeps_layout_and_recomputes_states() {
    let (tree, root) = Tree::flat(&[50, 30, 20]);
    let mut cache = PresentationCache::new();
    let mut calls = 0;
    let first = cache
        .presentation(&tree, root, 1, None, 1, |_, _| {
            calls += 1;
            CollectionState::None
        })
        .clone();
    let same = cache.presentation(&tree, root, 1, None, 1, |_, _| unreachable!()).clone();
    assert_eq!((same.revision, calls), (first.revision, 3));

    let collected = tree.children(root)[1];
    let changed = cache
        .presentation(&tree, root, 1, None, 2, |layout, index| {
            if layout.segments()[index].node == Some(collected) {
                CollectionState::Included
            } else {
                CollectionState::None
            }
        })
        .clone();
    assert!(Arc::ptr_eq(&changed.layout, &first.layout));
    assert_eq!(changed.layout_revision, first.layout_revision);
    assert_ne!(changed.revision, first.revision);
    assert_eq!(changed.state(changed.layout.index_of(collected).unwrap()), CollectionState::Included);

    let rebuilt = cache.presentation(&tree, root, 2, None, 2, |_, _| CollectionState::None);
    assert!(!Arc::ptr_eq(&rebuilt.layout, &first.layout));
    assert_ne!(rebuilt.layout_revision, first.layout_revision);
}
