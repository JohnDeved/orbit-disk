//! Breadth-first sunburst layout, id lookup and analytic hit-testing.
//! Port of Swift `SunburstLayout.make`, `SunburstPreparedLayout` and `MapHitRegions`.

use std::f64::consts::TAU;
use std::fmt;
use std::hash::Hash;
use std::ops::{Deref, Range};
use std::sync::{Arc, OnceLock};

use crate::color::{ColorOrdinals, SegmentColor};
use crate::geometry::{Metrics, Point, RING_COUNT, Sector, polar};
use crate::hash::FxHashMap;

/// Hard cap on segments per layout.
pub const MAX_SEGMENTS: usize = 768;
/// At most this many children of one parent get their own segment; the rest are grouped.
pub const MAX_VISIBLE_CHILDREN: usize = 48;
/// Children whose angle is below this (radians) are grouped.
pub const MIN_SEGMENT_ANGLE: f64 = 0.014;
/// Groups up to this size answer membership by scanning instead of building the member index.
const SCAN_MEMBERS_UP_TO: usize = 64;

/// Index into [`Layout::segments`].
pub type SegmentIndex = usize;

/// Tree access for the layout. Implement it over the app's node arena; nothing is copied except
/// ids of grouped children when the source cannot share its child list.
pub trait LayoutSource {
    type Id: Copy + Eq + Hash;

    /// Size in bytes. Zero-size children get no arc.
    fn size(&self, id: Self::Id) -> u64;

    /// Children, largest first. Files have none. Ordering is relied on for quality, not
    /// correctness: unsorted input still lays out (and groups) exactly like the Swift reference.
    fn children(&self, id: Self::Id) -> &[Self::Id];

    /// The same list as [`children`](Self::children) as a shared allocation. When provided, group
    /// members alias it instead of copying ids (a million-child folder becomes one group that
    /// shares the folder's child list).
    fn shared_children(&self, _id: Self::Id) -> Option<Arc<[Self::Id]>> {
        None
    }

    /// Whether the node is fully scanned. Only used to aggregate [`Group::complete`].
    fn is_complete(&self, _id: Self::Id) -> bool {
        true
    }
}

/// A contiguous slice of a shared id list.
pub struct Members<Id> {
    ids: Arc<[Id]>,
    start: usize,
    end: usize,
}

impl<Id> Members<Id> {
    fn whole(ids: Arc<[Id]>) -> Self {
        let end = ids.len();
        Self { ids, start: 0, end }
    }

    pub fn as_slice(&self) -> &[Id] {
        &self.ids[self.start..self.end]
    }

    /// The backing allocation and the range of it these members occupy.
    pub fn backing(&self) -> (&Arc<[Id]>, Range<usize>) {
        (&self.ids, self.start..self.end)
    }
}

impl<Id> Clone for Members<Id> {
    fn clone(&self) -> Self {
        Self { ids: Arc::clone(&self.ids), start: self.start, end: self.end }
    }
}

impl<Id> Deref for Members<Id> {
    type Target = [Id];

    fn deref(&self) -> &[Id] {
        self.as_slice()
    }
}

impl<Id> fmt::Debug for Members<Id> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Members").field("len", &(self.end - self.start)).finish()
    }
}

/// The real members behind a grouped segment.
#[derive(Clone, Debug)]
pub struct Group<Id> {
    /// The folder whose children were merged.
    pub parent: Id,
    /// Merged children in source order.
    pub members: Members<Id>,
    /// All members report [`LayoutSource::is_complete`].
    pub complete: bool,
    /// Members are exactly all children of `parent` (no visible siblings, no zero-size children).
    pub covers_all_children: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment<Id> {
    /// Ring, 0 = innermost.
    pub depth: u8,
    /// Chart angles, clockwise from 12 o'clock, before the angular gap trim.
    pub start: f64,
    pub end: f64,
    pub size: u64,
    /// The node, or `None` for a group segment.
    pub node: Option<Id>,
    /// Branch ordinal inherited from the root-level ancestor; `None` only for a root-level group.
    pub branch: Option<u32>,
    group: Option<u32>,
}

impl<Id> Segment<Id> {
    pub fn is_group(&self) -> bool {
        self.group.is_some()
    }

    pub fn color(&self) -> SegmentColor {
        match self.branch {
            Some(branch) if !self.is_group() => SegmentColor::branch(branch, self.depth),
            _ => SegmentColor::GROUP,
        }
    }

    pub fn sector(&self, metrics: &Metrics) -> Sector {
        metrics.sector(usize::from(self.depth), self.start, self.end)
    }
}

/// Result of [`Layout::hit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Outside every sector (including radial and angular gaps).
    None,
    /// Inside the center hole.
    Center,
    Segment(SegmentIndex),
}

#[derive(Clone, Copy)]
struct Branch<Id> {
    id: Id,
    start: f64,
    end: f64,
    depth: u8,
    color: Option<u32>,
}

/// Immutable result of a layout pass.
pub struct Layout<Id> {
    segments: Vec<Segment<Id>>,
    groups: Vec<Group<Id>>,
    rings: [Range<u32>; RING_COUNT],
    nodes: FxHashMap<Id, u32>,
    group_parents: FxHashMap<Id, u32>,
    /// Member id → segment index for every group, built on first lookup that needs it.
    pub(crate) members: OnceLock<FxHashMap<Id, u32>>,
}

impl<Id: fmt::Debug> fmt::Debug for Layout<Id> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Layout").field("segments", &self.segments).field("groups", &self.groups).finish()
    }
}

impl<Id: Copy + Eq + Hash> Default for Layout<Id> {
    fn default() -> Self {
        Self::from_parts(Vec::new(), Vec::new())
    }
}

impl<Id: Copy + Eq + Hash> Layout<Id> {
    /// Lays out up to four rings below `root`. `colors` supplies stable ordinals for the root's
    /// children; children without one use their index. Deeper segments inherit their branch ordinal.
    pub fn build<S: LayoutSource<Id = Id>>(source: &S, root: Id, colors: Option<&ColorOrdinals<Id>>) -> Self {
        let mut segments = Vec::new();
        let mut groups = Vec::new();
        if source.size(root) == 0 {
            return Self::from_parts(segments, groups);
        }
        let mut queue = vec![Branch { id: root, start: 0.0, end: TAU, depth: 0, color: None }];
        let mut next = 0;
        let mut scattered = Vec::new();

        while next < queue.len() && segments.len() < MAX_SEGMENTS {
            let branch = queue[next];
            next += 1;
            let children = source.children(branch.id);
            if usize::from(branch.depth) >= RING_COUNT || children.is_empty() {
                continue;
            }

            let mut total = 0.0;
            let mut sum = 0u64;
            let mut largest = 0;
            let mut smallest = u64::MAX;
            for &child in children {
                let size = source.size(child);
                total += size as f64;
                sum = sum.saturating_add(size);
                largest = largest.max(size);
                smallest = smallest.min(size);
            }
            let denominator = (source.size(branch.id) as f64).max(total);
            if denominator <= 0.0 {
                continue;
            }
            let sweep = branch.end - branch.start;
            let mut group = |segments: &mut Vec<Segment<Id>>,
                             start: f64,
                             size: u64,
                             members: Members<Id>,
                             complete: bool,
                             all: bool| {
                let end = (start + sweep * size as f64 / denominator).min(branch.end);
                segments.push(Segment {
                    depth: branch.depth,
                    start,
                    end,
                    size,
                    node: None,
                    branch: branch.color,
                    group: Some(groups.len() as u32),
                });
                groups.push(Group { parent: branch.id, members, complete, covers_all_children: all });
            };

            // Angles are monotonic in size, so when the largest child is below the threshold every
            // child merges. Equivalent to the general loop, minus the per-child work, and the group
            // aliases the whole child list. Generalizes Swift's root-only ≥1,024-child fast path.
            if smallest > 0 && sweep * largest as f64 / denominator < MIN_SEGMENT_ANGLE {
                let complete = children.iter().all(|&child| source.is_complete(child));
                let members = share(source, branch.id, children, 0..children.len());
                group(&mut segments, branch.start, sum, members, complete, true);
                continue;
            }

            let remaining = MAX_VISIBLE_CHILDREN.min(MAX_SEGMENTS.saturating_sub(segments.len() + 1));
            let mut cursor = branch.start;
            let mut visible = 0;
            let mut grouped = 0usize;
            let mut grouped_size = 0u64;
            let mut grouped_complete = true;
            let mut first = 0;
            let mut last = 0;
            // Grouped children are a contiguous run of sorted input; only unsorted input (or zero-size
            // children in between) forces collecting them into a fresh list.
            let mut spilled = false;
            scattered.clear();

            for (index, &child) in children.iter().enumerate() {
                let size = source.size(child);
                if size == 0 {
                    continue;
                }
                let angle = sweep * size as f64 / denominator;
                if angle < MIN_SEGMENT_ANGLE || visible >= remaining {
                    grouped_size = grouped_size.saturating_add(size);
                    grouped_complete &= source.is_complete(child);
                    if grouped == 0 {
                        first = index;
                    } else if !spilled && index != last + 1 {
                        scattered.extend_from_slice(&children[first..=last]);
                        spilled = true;
                    }
                    if spilled {
                        scattered.push(child);
                    }
                    last = index;
                    grouped += 1;
                    continue;
                }

                let color = branch.color.unwrap_or_else(|| {
                    colors.and_then(|colors| colors.get(child)).unwrap_or(u32::try_from(index).unwrap_or(u32::MAX))
                });
                let end = (cursor + angle).min(branch.end);
                segments.push(Segment {
                    depth: branch.depth,
                    start: cursor,
                    end,
                    size,
                    node: Some(child),
                    branch: Some(color),
                    group: None,
                });
                if usize::from(branch.depth) + 1 < RING_COUNT {
                    queue.push(Branch { id: child, start: cursor, end, depth: branch.depth + 1, color: Some(color) });
                }
                cursor = end;
                visible += 1;
            }

            if grouped > 0 {
                let members = if spilled {
                    Members::whole(Arc::from(scattered.as_slice()))
                } else {
                    share(source, branch.id, children, first..last + 1)
                };
                group(&mut segments, cursor, grouped_size, members, grouped_complete, grouped == children.len());
            }
        }
        Self::from_parts(segments, groups)
    }

    fn from_parts(segments: Vec<Segment<Id>>, groups: Vec<Group<Id>>) -> Self {
        let mut rings: [Range<u32>; RING_COUNT] = Default::default();
        let mut nodes = FxHashMap::default();
        let mut group_parents = FxHashMap::default();
        nodes.reserve(segments.len());
        let mut index = 0;
        for (depth, ring) in rings.iter_mut().enumerate() {
            let start = index;
            while index < segments.len() && usize::from(segments[index].depth) == depth {
                index += 1;
            }
            *ring = start as u32..index as u32;
        }
        debug_assert_eq!(index, segments.len(), "segments must be ordered by ring");
        for (index, segment) in segments.iter().enumerate() {
            debug_assert!(
                rings[usize::from(segment.depth)].start as usize == index || segments[index - 1].end <= segment.start,
                "segments within a ring must be ordered by angle"
            );
            if let Some(node) = segment.node {
                nodes.insert(node, index as u32);
            }
            if let Some(group) = segment.group {
                group_parents.insert(groups[group as usize].parent, index as u32);
            }
        }
        Self { segments, groups, rings, nodes, group_parents, members: OnceLock::new() }
    }

    pub fn segments(&self) -> &[Segment<Id>] {
        &self.segments
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Segment indices on ring `depth`, ordered by start angle.
    pub fn ring(&self, depth: usize) -> Range<SegmentIndex> {
        self.rings.get(depth).map_or(0..0, |ring| ring.start as usize..ring.end as usize)
    }

    pub fn group(&self, index: SegmentIndex) -> Option<&Group<Id>> {
        self.segments.get(index)?.group.map(|group| &self.groups[group as usize])
    }

    /// The segment showing `id`: its own segment, or the group it was merged into.
    pub fn index_of(&self, id: Id) -> Option<SegmentIndex> {
        if let Some(&index) = self.nodes.get(&id) {
            return Some(index as usize);
        }
        if self.groups.is_empty() {
            return None;
        }
        self.member_index().get(&id).map(|&index| index as usize)
    }

    /// Like [`index_of`](Self::index_of) for a caller that knows `id` is a child of `parent`. Avoids
    /// building the member index when the parent's children all merged into one group (the
    /// million-child folder case).
    pub fn index_of_child(&self, id: Id, parent: Id) -> Option<SegmentIndex> {
        if let Some(&index) = self.nodes.get(&id) {
            return Some(index as usize);
        }
        let group = *self.group_parents.get(&parent)?;
        if self.group(group as usize).is_some_and(|group| group.covers_all_children) {
            return Some(group as usize);
        }
        self.index_of(id)
    }

    /// The group segment holding merged children of `parent`, if any.
    pub fn group_of_parent(&self, parent: Id) -> Option<SegmentIndex> {
        self.group_parents.get(&parent).map(|&index| index as usize)
    }

    /// Whether `id` is one of the merged members of the group at `index`.
    pub fn group_contains(&self, index: SegmentIndex, id: Id) -> bool {
        let Some(group) = self.group(index) else { return false };
        if group.members.len() <= SCAN_MEMBERS_UP_TO {
            return group.members.contains(&id);
        }
        self.member_index().get(&id).is_some_and(|&found| found as usize == index)
    }

    /// Whether some lookup may have to build the member index (groups too large to scan). Hosts
    /// can then call [`warm_member_index`](Self::warm_member_index) off the UI thread right after
    /// a layout is built, so the first hover never pays for it.
    pub fn has_large_groups(&self) -> bool {
        self.groups.iter().any(|group| group.members.len() > SCAN_MEMBERS_UP_TO)
    }

    /// Builds the member index now (idempotent, thread-safe).
    pub fn warm_member_index(&self) {
        if !self.groups.is_empty() {
            self.member_index();
        }
    }

    fn member_index(&self) -> &FxHashMap<Id, u32> {
        self.members.get_or_init(|| {
            let mut map = FxHashMap::default();
            map.reserve(self.groups.iter().map(|group| group.members.len()).sum());
            for (index, segment) in self.segments.iter().enumerate() {
                if let Some(group) = segment.group {
                    for &member in self.groups[group as usize].members.iter() {
                        map.insert(member, index as u32);
                    }
                }
            }
            map
        })
    }

    pub fn sector(&self, metrics: &Metrics, index: SegmentIndex) -> Sector {
        self.segments[index].sector(metrics)
    }

    /// Analytic hit test: polar coordinates → candidate rings → binary search by start angle →
    /// gap check. O(log n) per ring, no allocation. Agrees exactly with [`Sector::contains`];
    /// where tiny charts make ring bands overlap, the inner ring wins (as Swift's path order did).
    pub fn hit(&self, metrics: &Metrics, point: Point) -> Hit {
        if !(point.x.is_finite() && point.y.is_finite()) {
            return Hit::None;
        }
        let (radius, theta) = polar(metrics.center, point);
        if radius < metrics.inner_radius {
            return Hit::Center;
        }
        for (depth, ring) in self.rings.iter().enumerate() {
            let (inner, outer) = metrics.ring_radii(depth);
            if radius < inner || radius > outer || ring.is_empty() {
                continue;
            }
            let ring = &self.segments[ring.start as usize..ring.end as usize];
            let candidate = ring.partition_point(|segment| segment.start <= theta);
            if candidate == 0 {
                continue;
            }
            let segment = &ring[candidate - 1];
            let sector = metrics.sector(depth, segment.start, segment.end);
            if theta >= sector.start && theta <= sector.end {
                return Hit::Segment(self.rings[depth].start as usize + candidate - 1);
            }
        }
        Hit::None
    }

    /// The segment under `point`; `None` for gaps, outside and the center hole (see [`hit`](Self::hit)
    /// to tell the center apart).
    pub fn hit_test(&self, metrics: &Metrics, point: Point) -> Option<SegmentIndex> {
        match self.hit(metrics, point) {
            Hit::Segment(index) => Some(index),
            Hit::None | Hit::Center => None,
        }
    }
}

fn share<S: LayoutSource>(source: &S, parent: S::Id, children: &[S::Id], range: Range<usize>) -> Members<S::Id> {
    match source.shared_children(parent) {
        Some(ids) if ids.len() == children.len() => Members { ids, start: range.start, end: range.end },
        _ => Members::whole(Arc::from(&children[range])),
    }
}
