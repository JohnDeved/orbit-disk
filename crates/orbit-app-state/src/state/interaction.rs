//! Hover relations, selection, navigation, gestures, sorting, rows and the map presentation.

use std::ops::Range;
use std::sync::Arc;

use orbit_core::paths::{file_name, is_within};
use orbit_core::view::{FolderTree, NodeIx, NodeMeta};
use orbit_sunburst::{CollectionState, Layout, Members, Presentation, SegmentIndex};

use super::{AppState, Pending};
use crate::coverage::CollectionCoverage;
use crate::event::{ComputeJob, Computed, Effect, Regions, SortRows, name_order};
use crate::folder::PreparedFolder;
use crate::ids::{GestureId, RequestId};
use crate::model::{Item, SortOrder};
use crate::paths::{parent, parent_or_root, relative_hover_path};

/// Name sorts of at least this many rows run off the UI thread.
pub const BACKGROUND_SORT_ROWS: usize = 2_000;

/// What the pointer is over on the map.
#[derive(Clone, Debug, Default)]
pub(crate) enum MapHover {
    #[default]
    None,
    /// Exact nodes of the current tree.
    Nodes(Vec<NodeIx>),
    /// A group segment with its real members.
    Group { segment: SegmentIndex, layout_revision: u64, parent: NodeIx, members: Members<NodeIx> },
}

/// Rows related to the map hover (indices into the unsorted rows) and the hover caption.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HoverPresentation {
    related: Related,
    description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
enum Related {
    #[default]
    None,
    /// A contiguous run of rows (a group of the folder's own children).
    Range(Range<u32>),
    /// Sorted row indices.
    Rows(Vec<u32>),
}

impl HoverPresentation {
    /// Caption above the list: the hovered path relative to the current folder, or
    /// `"N items in <folder>"` for several items.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Whether the unsorted row `base` is related. O(1) for groups of any size.
    pub fn contains(&self, base: u32) -> bool {
        match &self.related {
            Related::None => false,
            Related::Range(range) => range.contains(&base),
            Related::Rows(rows) => rows.binary_search(&base).is_ok(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.related {
            Related::None => true,
            Related::Range(range) => range.is_empty(),
            Related::Rows(rows) => rows.is_empty(),
        }
    }

    /// The first display position of a related row under a name order.
    fn first_rank(&self, order: &crate::event::NameOrder) -> Option<u32> {
        match &self.related {
            Related::None => None,
            Related::Range(range) => order.first_rank_in(range.clone()),
            Related::Rows(rows) => rows.iter().filter_map(|&row| order.rank(row)).min(),
        }
    }

    fn min(&self) -> Option<u32> {
        match &self.related {
            Related::None => None,
            Related::Range(range) => (!range.is_empty()).then_some(range.start),
            Related::Rows(rows) => rows.first().copied(),
        }
    }
}

/// One row of the Contents list: a child of the current folder or a search hit.
#[derive(Clone, Copy, Debug)]
pub enum RowView<'a> {
    Node { folder: &'a PreparedFolder, ix: NodeIx },
    Hit(&'a Item),
}

impl<'a> RowView<'a> {
    pub fn name(&self) -> &'a [u8] {
        match *self {
            Self::Node { folder, ix } => folder.name(ix),
            Self::Hit(item) => item.name(),
        }
    }

    pub fn meta(&self) -> &'a NodeMeta {
        match *self {
            Self::Node { folder, ix } => folder.meta(ix),
            Self::Hit(item) => &item.meta,
        }
    }

    pub fn path(&self) -> Vec<u8> {
        match *self {
            Self::Node { folder, ix } => folder.path(ix),
            Self::Hit(item) => item.path.to_vec(),
        }
    }

    pub fn item(&self) -> Item {
        match *self {
            Self::Node { folder, ix } => folder.item(ix),
            Self::Hit(item) => item.clone(),
        }
    }
}

fn segment_state(
    folder: &PreparedFolder,
    coverage: &CollectionCoverage,
    layout: &Layout<NodeIx>,
    index: SegmentIndex,
) -> CollectionState {
    if coverage.is_empty() {
        return CollectionState::None;
    }
    let segment = &layout.segments()[index];
    if let Some(ix) = segment.node {
        return coverage.state(&folder.path(ix));
    }
    let Some(group) = layout.group(index) else { return CollectionState::None };
    let parent_path = folder.path(group.parent);
    coverage.state_of_group(
        &parent_path,
        group.members.len(),
        true,
        |child| folder.child_named(group.parent, file_name(child)).is_some_and(|ix| layout.group_contains(index, ix)),
        |coverage| {
            let paths: Vec<Vec<u8>> = group.members.iter().map(|&ix| folder.path(ix)).collect();
            coverage.state_of_all(paths.iter().map(Vec::as_slice))
        },
    )
}

impl AppState {
    // -------------------------------------------------------------------------------------
    // Rows.

    pub fn filtering(&self) -> bool {
        !self.search.query.trim().is_empty()
    }

    pub fn row_count(&self) -> usize {
        if self.filtering() { self.search.results.len() } else { self.folder.as_ref().map_or(0, |f| f.rows().len()) }
    }

    /// Unsorted row index shown at `display` position.
    pub fn base_row(&self, display: usize) -> usize {
        self.order.as_ref().and_then(|order| order.row_at(display)).map_or(display, |base| base as usize)
    }

    pub fn row(&self, display: usize) -> Option<RowView<'_>> {
        let base = self.base_row(display);
        if self.filtering() {
            return self.search.results.get(base).map(RowView::Hit);
        }
        let folder = self.folder.as_deref()?;
        folder.rows().get(base).map(|&ix| RowView::Node { folder, ix })
    }

    /// Collection mark of an item (`Included`: check; `Partial`: minus).
    pub fn collection_state(&self, path: &[u8]) -> CollectionState {
        self.collection().map_or(CollectionState::None, |c| c.coverage().state(path))
    }

    pub fn sort_order(&self) -> SortOrder {
        self.sort
    }

    /// A background name sort is running; the list keeps its previous order meanwhile.
    pub fn is_sorting_rows(&self) -> bool {
        self.sorting
    }

    /// The mapped item at `path`: a search hit, or a node of the displayed tree.
    pub fn lookup_item(&self, path: &[u8]) -> Option<Item> {
        if let Some(item) = self.search.item(path) {
            return Some(item.clone());
        }
        let folder = self.folder.as_ref()?;
        folder.find(path).map(|ix| folder.item(ix))
    }

    // -------------------------------------------------------------------------------------
    // Presentation (layout + collection marks).

    /// The map to paint. Rebuilds the layout only for a new folder revision and the collection
    /// marks only when the displayed collection changed.
    pub fn presentation(&mut self) -> Option<Presentation<NodeIx>> {
        let folder = Arc::clone(self.folder.as_ref()?);
        let colors = folder.layout_colors(&self.colors.ordinals);
        let collection = self.collection().map(|c| (Arc::clone(c.coverage()), c.revision()));
        let (coverage, revision) = collection.unwrap_or_else(|| (Arc::default(), 0));
        let presentation = self.presentation.presentation(
            &*folder,
            FolderTree::FOLDER,
            self.map_revision,
            Some(&colors),
            revision,
            |layout, index| segment_state(&folder, &coverage, layout, index),
        );
        Some(presentation.clone())
    }

    // -------------------------------------------------------------------------------------
    // Hover.

    pub fn hover(&self) -> &HoverPresentation {
        &self.hover
    }

    pub fn hover_description(&self) -> Option<&str> {
        self.hover.description()
    }

    pub fn is_row_related(&self, display: usize) -> bool {
        self.hover.contains(self.base_row(display) as u32)
    }

    /// The first related row in display order (scrolled into view on map hover).
    pub fn first_related_row(&self) -> Option<usize> {
        match &self.order {
            None => self.hover.min().map(|base| base as usize),
            Some(order) => self.hover.first_rank(order).map(|rank| rank as usize),
        }
    }

    pub fn row_hover(&self) -> Option<&[u8]> {
        self.row_hover.as_deref()
    }

    /// The map segment to outline for the hovered row.
    pub fn row_hover_segment(&mut self) -> Option<SegmentIndex> {
        let path = self.row_hover.clone()?;
        let presentation = self.presentation()?;
        let folder = self.folder.as_ref()?;
        let mut candidate: &[u8] = &path;
        loop {
            if !is_within(candidate, folder.folder_path()) || candidate == folder.folder_path() {
                return None;
            }
            if let Some(ix) = folder.find(candidate) {
                let parent = folder.tree().node(ix).parent;
                return presentation.layout.index_of_child(ix, parent);
            }
            candidate = parent(candidate)?;
        }
    }

    pub(crate) fn hover_segment(&mut self, segment: SegmentIndex, layout_revision: u64) {
        let Some(presentation) = self.presentation() else { return };
        if presentation.layout_revision != layout_revision {
            return;
        }
        let layout = &presentation.layout;
        let Some(found) = layout.segments().get(segment) else { return };
        let hover = match found.node {
            Some(ix) => MapHover::Nodes(vec![ix]),
            None => {
                let Some(group) = layout.group(segment) else { return };
                MapHover::Group { segment, layout_revision, parent: group.parent, members: group.members.clone() }
            }
        };
        let same = match (&self.map_hover, &hover) {
            (MapHover::Nodes(a), MapHover::Nodes(b)) => a == b,
            (
                MapHover::Group { segment: a, layout_revision: ra, .. },
                MapHover::Group { segment: b, layout_revision: rb, .. },
            ) => a == b && ra == rb,
            _ => false,
        };
        if same && self.row_hover.is_none() {
            return;
        }
        self.map_hover = hover;
        self.row_hover = None;
        self.hover_dirty = true;
        self.notify(Regions::MAP);
    }

    pub(crate) fn clear_map_hover(&mut self) {
        if matches!(self.map_hover, MapHover::None) {
            return;
        }
        self.map_hover = MapHover::None;
        self.hover_dirty = true;
        self.notify(Regions::MAP);
    }

    pub(crate) fn hover_row(&mut self, path: Option<Arc<[u8]>>) {
        if self.row_hover == path {
            return;
        }
        self.row_hover = path;
        self.notify(Regions::MAP | Regions::LIST);
    }

    pub(crate) fn clear_all_hover(&mut self) {
        if !matches!(self.map_hover, MapHover::None) || self.row_hover.is_some() {
            self.notify(Regions::MAP | Regions::LIST);
        }
        self.map_hover = MapHover::None;
        self.row_hover = None;
        self.hover_dirty = true;
    }

    /// Re-targets the hover onto a new revision of the same folder (Swift `installFolder`
    /// hover refresh): exact members are looked up by path and identity; a group whose
    /// membership is unchanged stays a group, otherwise its exact members stay hovered; any
    /// vanished or replaced member clears the hover.
    pub(crate) fn refresh_hover(&mut self, old: &PreparedFolder) {
        self.hover_dirty = true;
        let Some(new) = self.folder.clone() else { return };
        let same_item = |ix: NodeIx, found: Option<NodeIx>| {
            found.filter(|&found| new.meta(found).identity == old.meta(ix).identity)
        };
        let hover = match std::mem::take(&mut self.map_hover) {
            MapHover::None => MapHover::None,
            MapHover::Nodes(nodes) => {
                let refreshed: Option<Vec<NodeIx>> =
                    nodes.iter().map(|&ix| same_item(ix, new.find(&old.path(ix)))).collect();
                refreshed.map_or(MapHover::None, MapHover::Nodes)
            }
            MapHover::Group { parent, members, .. } => {
                let parent_path = old.path(parent);
                let refreshed = new.find(&parent_path).and_then(|new_parent| {
                    let members: Option<Vec<NodeIx>> =
                        members.iter().map(|&ix| same_item(ix, new.child_named(new_parent, old.name(ix)))).collect();
                    members.map(|members| (new_parent, members))
                });
                match refreshed {
                    None => MapHover::None,
                    Some((new_parent, members)) => self.regroup(new_parent, members),
                }
            }
        };
        self.map_hover = hover;
        if let Some(path) = self.row_hover.clone()
            && self.lookup_item(&path).is_none()
        {
            self.row_hover = None;
        }
    }

    fn regroup(&mut self, parent: NodeIx, members: Vec<NodeIx>) -> MapHover {
        let Some(presentation) = self.presentation() else { return MapHover::Nodes(members) };
        let layout = &presentation.layout;
        if let Some(segment) = layout.group_of_parent(parent)
            && let Some(group) = layout.group(segment)
            && group.members.len() == members.len()
            && members.iter().all(|&ix| layout.group_contains(segment, ix))
        {
            return MapHover::Group {
                segment,
                layout_revision: presentation.layout_revision,
                parent,
                members: group.members.clone(),
            };
        }
        MapHover::Nodes(members)
    }

    fn is_group_member(folder: &PreparedFolder, parent: NodeIx, members: &Members<NodeIx>, ix: NodeIx) -> bool {
        let (backing, range) = members.backing();
        if parent == FolderTree::FOLDER && Arc::ptr_eq(backing, folder.rows()) {
            return folder.row_of(ix).is_some_and(|row| range.contains(&row));
        }
        members.contains(&ix)
    }

    pub(crate) fn make_hover_presentation(&self) -> HoverPresentation {
        let Some(folder) = self.folder.as_deref() else { return HoverPresentation::default() };
        let current = folder.folder_path();
        let home = &self.cfg.home;
        let relative = |path: &[u8]| relative_hover_path(path, Some(current), home);
        let filtering = self.filtering();
        match &self.map_hover {
            MapHover::None => HoverPresentation::default(),
            MapHover::Nodes(nodes) if nodes.is_empty() => HoverPresentation::default(),
            MapHover::Nodes(nodes) => {
                let paths: Vec<Vec<u8>> = nodes.iter().map(|&ix| folder.path(ix)).collect();
                let mut rows = Vec::new();
                for (&ix, path) in nodes.iter().zip(&paths) {
                    if filtering {
                        let mut candidate: &[u8] = path;
                        while is_within(candidate, current) {
                            if let Some(hit) = self.search.index_of(candidate) {
                                rows.push(hit);
                            }
                            if candidate == current {
                                break;
                            }
                            candidate = parent_or_root(candidate);
                        }
                    } else if let Some(row) = folder.row_ancestor(ix).and_then(|row| folder.row_of(row)) {
                        rows.push(row as u32);
                    }
                }
                rows.sort_unstable();
                rows.dedup();
                let description = if paths.len() == 1 {
                    relative(&paths[0])
                } else {
                    let mut common: &[u8] = parent_or_root(&paths[0]);
                    for path in &paths {
                        while common != b"/" && !is_within(path, common) {
                            common = parent_or_root(common);
                        }
                    }
                    format!("{} items in {}", paths.len(), relative(common))
                };
                HoverPresentation { related: Related::Rows(rows), description: Some(description) }
            }
            MapHover::Group { parent, members, .. } => {
                let parent_path = folder.path(*parent);
                let related = if filtering {
                    let mut rows = Vec::new();
                    for (index, hit) in self.search.results.iter().enumerate() {
                        let member = parent_or_root(&hit.path) == &parent_path[..]
                            && folder
                                .child_named(*parent, hit.name())
                                .is_some_and(|ix| Self::is_group_member(folder, *parent, members, ix));
                        if member || (is_within(&parent_path, &hit.path) && is_within(&hit.path, current)) {
                            rows.push(index as u32);
                        }
                    }
                    Related::Rows(rows)
                } else if *parent == FolderTree::FOLDER {
                    let (backing, range) = members.backing();
                    if Arc::ptr_eq(backing, folder.rows()) {
                        Related::Range(range.start as u32..range.end as u32)
                    } else {
                        let mut rows: Vec<u32> =
                            members.iter().filter_map(|&ix| folder.row_of(ix).map(|row| row as u32)).collect();
                        rows.sort_unstable();
                        Related::Rows(rows)
                    }
                } else {
                    let row = folder.row_ancestor(*parent).and_then(|row| folder.row_of(row));
                    Related::Rows(row.map(|row| vec![row as u32]).unwrap_or_default())
                };
                let description = if members.len() == 1 {
                    relative(&folder.path(members[0]))
                } else {
                    format!("{} items in {}", members.len(), relative(&parent_path))
                };
                HoverPresentation { related, description: Some(description) }
            }
        }
    }

    // -------------------------------------------------------------------------------------
    // Selection and navigation.

    pub(crate) fn select(&mut self, path: Option<Arc<[u8]>>) {
        self.selected = path.and_then(|path| self.lookup_item(&path));
        self.notify(Regions::INSPECTOR | Regions::LIST | Regions::MAP);
    }

    /// Swift `open(_:)`: folders open; files (and symlinks) are selected.
    pub(crate) fn open(&mut self, path: &[u8]) {
        if !self.displayed_matches_active() {
            return;
        }
        if self.requested_target.as_ref().is_some_and(|(target, _)| target[..] != *path) {
            self.requested_target = None;
        }
        let Some(item) = self.lookup_item(path) else { return };
        if !item.is_dir() || item.meta.flags.is_symlink {
            self.selected = Some(item);
            self.notify(Regions::INSPECTOR | Regions::LIST);
            return;
        }
        self.set_query(String::new());
        self.load_folder(path.to_vec(), None);
    }

    /// Swift `goBack`: open the parent and keep the departed folder selected.
    pub(crate) fn go_up(&mut self) {
        if !self.displayed_matches_active() {
            return;
        }
        self.requested_target = None;
        let Some(folder) = self.folder.as_ref().filter(|f| !f.tree().ancestors.is_empty()) else { return };
        let departed: Arc<[u8]> = Arc::from(folder.folder_path());
        let parent = parent_or_root(&departed).to_vec();
        self.set_query(String::new());
        self.load_folder(parent, Some(departed));
    }

    // -------------------------------------------------------------------------------------
    // Gestures: pointer down holds the latest preview, indexed folder, scan completion and
    // name sort until the gesture ends, then applies them in this order.

    pub(crate) fn begin_gesture(&mut self, id: GestureId) {
        self.gesture = Some(id);
    }

    pub fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }

    pub(crate) fn end_gesture(&mut self, id: GestureId) {
        if self.gesture != Some(id) {
            return;
        }
        self.gesture = None;
        if self.deferred.completion.is_some() && !self.is_removing() {
            if let Some(completion) = self.deferred.completion.take() {
                self.run_completion(completion);
            }
        } else if let Some((view, generation, browse)) = self.deferred.preview.take() {
            let blocked = self.active_session().is_some_and(|s| s.preview_blocked);
            if generation == self.generation && browse == self.browse && self.partial && !blocked {
                self.publish_preview(view);
            }
        }
        if let Some(indexed) = self.deferred.indexed.take() {
            let session = self.active_session();
            let current = indexed.generation == self.generation
                && indexed.browse == self.browse
                && indexed.session == self.active
                && !session.is_some_and(|s| s.preview_blocked);
            if current {
                let selection = self.selected.as_ref().map(|item| Arc::clone(&item.path));
                let epoch = session.map_or(0, |s| s.epoch);
                if epoch != indexed.epoch || self.snapshot.as_ref().map(|s| s.id) != Some(indexed.snapshot.id) {
                    self.load_folder(indexed.view.folder_path().to_vec(), selection);
                } else {
                    self.publish(indexed.view, indexed.snapshot, selection, None);
                }
            }
        }
        if let Some((order, revision)) = self.deferred.sort.take() {
            self.finish_name_sort(order, revision);
        }
    }

    // -------------------------------------------------------------------------------------
    // Sorting.

    pub(crate) fn set_sort(&mut self, order: SortOrder) {
        if self.sort == order {
            return;
        }
        self.sort = order;
        self.schedule_name_sort();
        self.notify(Regions::LIST);
    }

    fn sort_rows(&self) -> Option<SortRows> {
        if self.filtering() {
            Some(SortRows::Hits(Arc::clone(&self.search.results)))
        } else {
            self.folder.as_ref().map(|folder| SortRows::Folder(Arc::clone(folder)))
        }
    }

    /// Swift `scheduleNameSort`: called whenever the rows or the order change.
    pub(crate) fn schedule_name_sort(&mut self) {
        if let Some(id) = self.sort_request.take() {
            self.cancel(Some(id));
        }
        self.deferred.sort = None;
        self.sort_revision = self.token();
        self.list_revision = self.token();
        self.hover_dirty = true;
        let rows = self.sort_rows();
        let len = rows.as_ref().map_or(0, SortRows::len);
        if self.sort != SortOrder::Name || len < BACKGROUND_SORT_ROWS {
            self.sorting = false;
            self.order = match (self.sort, rows) {
                (SortOrder::Name, Some(rows)) => Some(name_order(&rows, self.cfg.compare_names)),
                _ => None,
            };
            return;
        }
        let Some(rows) = rows else { return };
        self.sorting = true;
        let id = RequestId(self.token());
        self.pending.insert(id, Pending::Sort { revision: self.sort_revision });
        self.sort_request = Some(id);
        self.out.push(Effect::Compute { id, job: ComputeJob::NameSort { rows, compare: self.cfg.compare_names } });
    }

    pub(crate) fn on_computed(&mut self, id: RequestId, result: Computed) {
        let Some(Pending::Sort { revision }) = self.pending.remove(&id) else { return };
        if self.sort_request == Some(id) {
            self.sort_request = None;
        }
        let Computed::NameOrder(order) = result;
        if revision != self.sort_revision || self.sort != SortOrder::Name {
            return;
        }
        if self.gesture.is_some() {
            self.deferred.sort = Some((order, revision));
            return;
        }
        self.finish_name_sort(order, revision);
    }

    pub(crate) fn finish_name_sort(&mut self, order: crate::event::NameOrder, revision: u64) {
        if revision != self.sort_revision || self.sort != SortOrder::Name {
            return;
        }
        self.order = Some(order);
        self.sorting = false;
        self.list_revision = self.token();
        self.notify(Regions::LIST);
    }
}
