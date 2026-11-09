//! Folder preparation: rows, row index, color ordinals and the [`LayoutSource`] over a
//! [`FolderTree`] arena. Port of Swift `prepareFolder` and the color bookkeeping in `installFolder`.
//!
//! Preparation is pure and may run on a background thread (the host does it next to the backend
//! read, see [`crate::backend::perform`]).

use std::sync::Arc;

use orbit_core::paths::{is_within, relative_components};
use orbit_core::types::ScanState;
use orbit_core::view::{FolderTree, NO_PARENT, NodeIx, NodeMeta};
use orbit_platform::Cancellation;
use orbit_sunburst::{ColorOrdinals, LayoutSource, MAX_VISIBLE_CHILDREN, PALETTE_HUES};

use crate::hash::{FxHashMap, name_hash};
use crate::model::Item;

/// Stable key of a child within its folder (hash of the name bytes). Colors are keyed by it so a
/// branch keeps its hue across tree revisions, whose node indices differ.
pub type NameKey = u64;

/// Folders with at least this many rows get a name → row index (Swift: 1,000).
pub const ROW_INDEX_THRESHOLD: usize = 1_000;

/// The color assignment of the visible folder, captured when a read is requested so the
/// background preparation can extend it (Swift `ColorPreparationState`).
#[derive(Clone, Debug)]
pub struct ColorState {
    pub folder: Arc<[u8]>,
    pub revision: u64,
    pub ordinals: Arc<ColorOrdinals<NameKey>>,
}

/// A folder ready to display: the tree, its rows (direct children, largest first) and lookups.
#[derive(Debug)]
pub struct PreparedFolder {
    tree: Arc<FolderTree>,
    rows: Arc<[NodeIx]>,
    index: Option<FxHashMap<u64, u32>>,
    pub(crate) initial_colors: Arc<ColorOrdinals<NameKey>>,
    /// `seed` extended with this folder's new children, when the seed was for the same folder.
    pub(crate) preserved_colors: Option<(Arc<ColorOrdinals<NameKey>>, u64, bool)>,
}

/// Prepares `tree` for display. `None` when `cancel` fired.
pub fn prepare_folder(
    tree: impl Into<Arc<FolderTree>>,
    seed: Option<&ColorState>,
    cancel: Option<&Cancellation>,
) -> Option<PreparedFolder> {
    let tree = tree.into();
    let cancelled = || cancel.is_some_and(Cancellation::is_cancelled);
    let children = tree.children(FolderTree::FOLDER);
    let rows: Arc<[NodeIx]> = Arc::from(children);
    let mut index = (rows.len() >= ROW_INDEX_THRESHOLD).then(FxHashMap::default);
    let mut initial = ColorOrdinals::new();
    let mut keys = Vec::with_capacity(rows.len());
    for (row, &ix) in rows.iter().enumerate() {
        if row & 255 == 0 && cancelled() {
            return None;
        }
        let key = name_hash(tree.name(ix));
        keys.push(key);
        if let Some(index) = index.as_mut() {
            index.entry(key).or_insert(row as u32);
        }
    }
    initial.extend(&keys);
    let preserved = seed.filter(|seed| *seed.folder == *tree.folder_path).map(|seed| {
        let mut ordinals = (*seed.ordinals).clone();
        let extended = ordinals.extend(&keys);
        (Arc::new(ordinals), seed.revision, extended)
    });
    if cancelled() {
        return None;
    }
    Some(PreparedFolder { tree, rows, index, initial_colors: Arc::new(initial), preserved_colors: preserved })
}

impl PreparedFolder {
    pub fn tree(&self) -> &Arc<FolderTree> {
        &self.tree
    }

    /// Direct children of the folder, largest first. Shared with the layout's group members.
    pub fn rows(&self) -> &Arc<[NodeIx]> {
        &self.rows
    }

    pub fn folder_path(&self) -> &[u8] {
        &self.tree.folder_path
    }

    pub fn root_path(&self) -> &[u8] {
        &self.tree.root_path
    }

    /// The mapped root node (first ancestor, or the folder itself at the root).
    pub fn root_ix(&self) -> NodeIx {
        self.tree.ancestors.first().copied().unwrap_or(FolderTree::FOLDER)
    }

    pub fn root_meta(&self) -> &NodeMeta {
        &self.tree.node(self.root_ix()).meta
    }

    pub fn folder_meta(&self) -> &NodeMeta {
        &self.tree.node(FolderTree::FOLDER).meta
    }

    pub fn meta(&self, ix: NodeIx) -> &NodeMeta {
        &self.tree.node(ix).meta
    }

    pub fn name(&self, ix: NodeIx) -> &[u8] {
        self.tree.name(ix)
    }

    pub fn path(&self, ix: NodeIx) -> Vec<u8> {
        self.tree.path(ix)
    }

    pub fn item(&self, ix: NodeIx) -> Item {
        Item::new(self.path(ix), *self.meta(ix))
    }

    /// Navigation chain root-first, ending with the folder.
    pub fn navigation(&self) -> impl Iterator<Item = NodeIx> + '_ {
        self.tree.ancestors.iter().copied().chain(std::iter::once(FolderTree::FOLDER))
    }

    /// Row position of the child named `name`.
    pub fn row_of_name(&self, name: &[u8]) -> Option<usize> {
        if let Some(index) = &self.index {
            let row = *index.get(&name_hash(name))? as usize;
            if self.tree.name(self.rows[row]) == name {
                return Some(row);
            }
            // Hash collision: fall back to a scan.
        }
        self.rows.iter().position(|&ix| self.tree.name(ix) == name)
    }

    /// Row position of a direct child node.
    pub fn row_of(&self, ix: NodeIx) -> Option<usize> {
        if self.tree.node(ix).parent != FolderTree::FOLDER || ix == FolderTree::FOLDER {
            return None;
        }
        self.row_of_name(self.tree.name(ix))
    }

    pub(crate) fn child_named(&self, parent: NodeIx, name: &[u8]) -> Option<NodeIx> {
        if parent == FolderTree::FOLDER {
            return self.row_of_name(name).map(|row| self.rows[row]);
        }
        self.tree.children(parent).iter().copied().find(|&ix| self.tree.name(ix) == name)
    }

    /// The node at `path`: the folder, an ancestor, or a loaded descendant.
    pub fn find(&self, path: &[u8]) -> Option<NodeIx> {
        let folder = self.folder_path();
        if path == folder {
            return Some(FolderTree::FOLDER);
        }
        if is_within(folder, path) {
            if !is_within(path, self.root_path()) {
                return None;
            }
            let below = relative_components(folder, path)?.count();
            let position = self.tree.ancestors.len().checked_sub(below)?;
            return self.tree.ancestors.get(position).copied();
        }
        let mut current = FolderTree::FOLDER;
        for component in relative_components(path, folder)? {
            current = self.child_named(current, component)?;
        }
        Some(current)
    }

    /// The top-level row on the way to `ix` (`ix` itself for a row).
    pub fn row_ancestor(&self, mut ix: NodeIx) -> Option<NodeIx> {
        loop {
            let parent = self.tree.node(ix).parent;
            if parent == FolderTree::FOLDER {
                return Some(ix);
            }
            if parent == NO_PARENT {
                return None;
            }
            ix = parent;
        }
    }

    /// Whether `ix` lies in the folder subtree (not an ancestor).
    pub fn is_descendant(&self, ix: NodeIx) -> bool {
        ix != FolderTree::FOLDER && self.row_ancestor(ix).is_some()
    }

    /// Converts stable ordinals of the folder's children to the layout's node indices. Only the
    /// first [`MAX_VISIBLE_CHILDREN`] non-empty children can get their own segment, and the
    /// layout only paints `ordinal % 8`, so each visible child gets the smallest distinct ordinal
    /// congruent to its stable ordinal; gaps are padded with ids that are not in the tree.
    pub fn layout_colors(&self, ordinals: &ColorOrdinals<NameKey>) -> ColorOrdinals<NodeIx> {
        let hues = PALETTE_HUES.len() as u32;
        let mut wanted: Vec<(u32, NodeIx)> = Vec::with_capacity(MAX_VISIBLE_CHILDREN);
        let mut per_hue = [0u32; PALETTE_HUES.len()];
        for &ix in self.rows.iter().filter(|&&ix| self.tree.node(ix).meta.size > 0).take(MAX_VISIBLE_CHILDREN) {
            let Some(stable) = ordinals.get(name_hash(self.tree.name(ix))) else { continue };
            let hue = stable % hues;
            let ordinal = hue + hues * per_hue[hue as usize];
            per_hue[hue as usize] += 1;
            wanted.push((ordinal, ix));
        }
        wanted.sort_unstable();
        let mut sequence = Vec::with_capacity(wanted.last().map_or(0, |(o, _)| *o as usize + 1));
        let mut pad = NodeIx::MAX - 1;
        for (ordinal, ix) in wanted {
            while sequence.len() < ordinal as usize {
                sequence.push(pad);
                pad -= 1;
            }
            sequence.push(ix);
        }
        ColorOrdinals::for_children(&sequence)
    }
}

impl LayoutSource for PreparedFolder {
    type Id = NodeIx;

    #[inline]
    fn size(&self, id: NodeIx) -> u64 {
        self.tree.node(id).meta.size
    }

    #[inline]
    fn children(&self, id: NodeIx) -> &[NodeIx] {
        if id == FolderTree::FOLDER { &self.rows } else { self.tree.children(id) }
    }

    fn shared_children(&self, id: NodeIx) -> Option<Arc<[NodeIx]>> {
        (id == FolderTree::FOLDER).then(|| Arc::clone(&self.rows))
    }

    fn is_complete(&self, id: NodeIx) -> bool {
        self.tree.node(id).meta.state == ScanState::Complete
    }
}
