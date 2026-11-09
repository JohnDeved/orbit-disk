//! `FolderTree`: what every reader (live inventory, stage, saved map) returns for one folder,
//! and what the sunburst and Contents list consume.
//!
//! The tree holds the folder, *all* of its direct children, and a bounded set of deeper
//! descendants for the map (Swift: up to 2,000 descendants across 4 levels). Names live in one
//! arena; children of each node are contiguous and sorted **largest first, then by name bytes**.

use crate::types::ScanState;
use orbit_platform::FileIdentity;
use std::sync::Arc;

pub type NodeIx = u32;
pub const NO_PARENT: NodeIx = NodeIx::MAX;

/// Maximum depth below the folder that views load for the map.
pub const VIEW_DEPTH: u32 = 4;
/// Maximum number of descendants (beyond direct children) a view loads for the map.
pub const VIEW_DESCENDANT_BUDGET: usize = 2_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeFlags {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_mount_point: bool,
    /// False when the folder could not be read (permissions): sizes are incomplete.
    pub accessible: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeMeta {
    pub flags: NodeFlags,
    /// Logical bytes (files) or subtree total (directories).
    pub size: u64,
    /// Allocated bytes after hard-link deduplication (subtree total for directories).
    pub allocated: u64,
    /// Allocation before deduplication; zero for directories.
    pub raw_allocated: u64,
    /// Files in the subtree (1 for a file).
    pub file_count: u64,
    pub identity: Option<FileIdentity>,
    pub state: ScanState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeNode {
    name_start: u32,
    name_len: u32,
    pub parent: NodeIx,
    children_start: u32,
    children_len: u32,
    /// Children that exist, including ones not loaded into this view.
    pub total_children: u32,
    pub meta: NodeMeta,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ViewSource {
    /// In-memory inventory while discovery runs.
    #[default]
    Live,
    /// A mapping stage (SQLite + unacknowledged mailbox batches).
    Stage,
    /// A published, saved map.
    Saved,
}

#[derive(Clone, Debug, Default)]
pub struct FolderTree {
    pub root_path: Arc<[u8]>,
    pub folder_path: Vec<u8>,
    /// Ancestors from the mapped root down to the folder's parent (root first).
    pub ancestors: Vec<NodeIx>,
    /// Monotonic per source; a higher revision of the same folder supersedes a lower one.
    pub revision: u64,
    /// Discovery is still listing this folder or its descendants.
    pub is_listing: bool,
    pub source: ViewSource,
    names: Vec<u8>,
    nodes: Vec<TreeNode>,
    children: Vec<NodeIx>,
}

impl FolderTree {
    /// The folder itself.
    pub const FOLDER: NodeIx = 0;

    #[inline]
    pub fn node(&self, ix: NodeIx) -> &TreeNode {
        &self.nodes[ix as usize]
    }

    #[inline]
    pub fn name(&self, ix: NodeIx) -> &[u8] {
        let n = &self.nodes[ix as usize];
        &self.names[n.name_start as usize..(n.name_start + n.name_len) as usize]
    }

    #[inline]
    pub fn children(&self, ix: NodeIx) -> &[NodeIx] {
        let n = &self.nodes[ix as usize];
        &self.children[n.children_start as usize..(n.children_start + n.children_len) as usize]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Full path of a node in this tree (folder, ancestor or descendant).
    pub fn path(&self, ix: NodeIx) -> Vec<u8> {
        if let Some(pos) = self.ancestors.iter().position(|a| *a == ix) {
            // Ancestors are root-first; rebuild from the folder path by trimming components.
            let mut path = self.folder_path.clone();
            for _ in pos..self.ancestors.len() {
                path = crate::paths::parent(&path).map(<[u8]>::to_vec).unwrap_or_else(|| b"/".to_vec());
            }
            return path;
        }
        let mut chain = Vec::new();
        let mut cur = ix;
        while cur != Self::FOLDER && cur != NO_PARENT {
            chain.push(cur);
            cur = self.node(cur).parent;
        }
        let mut path = self.folder_path.clone();
        for ix in chain.into_iter().rev() {
            path = crate::paths::join(&path, self.name(ix));
        }
        path
    }
}

/// Builds a [`FolderTree`] from nodes added in any order; `finish` sorts children.
#[derive(Debug, Default)]
pub struct FolderTreeBuilder {
    tree: FolderTree,
    pending_children: Vec<(NodeIx, NodeIx)>,
}

impl FolderTreeBuilder {
    /// Starts a tree whose node 0 is the folder.
    pub fn new(root_path: Arc<[u8]>, folder_path: Vec<u8>, folder_name: &[u8], folder: NodeMeta) -> Self {
        let mut builder =
            Self { tree: FolderTree { root_path, folder_path, ..FolderTree::default() }, ..Self::default() };
        builder.push_raw(folder_name, NO_PARENT, folder, 0);
        builder
    }

    fn push_raw(&mut self, name: &[u8], parent: NodeIx, meta: NodeMeta, total_children: u32) -> NodeIx {
        let ix = self.tree.nodes.len() as NodeIx;
        let name_start = self.tree.names.len() as u32;
        self.tree.names.extend_from_slice(name);
        self.tree.nodes.push(TreeNode {
            name_start,
            name_len: name.len() as u32,
            parent,
            children_start: 0,
            children_len: 0,
            total_children,
            meta,
        });
        ix
    }

    /// Adds an ancestor (call root first). Ancestors are not linked as children.
    pub fn push_ancestor(&mut self, name: &[u8], meta: NodeMeta) -> NodeIx {
        let ix = self.push_raw(name, NO_PARENT, meta, 0);
        self.tree.ancestors.push(ix);
        ix
    }

    /// Adds a child of `parent` (the folder or a descendant already added).
    pub fn push_child(&mut self, parent: NodeIx, name: &[u8], meta: NodeMeta) -> NodeIx {
        let ix = self.push_raw(name, parent, meta, 0);
        self.pending_children.push((parent, ix));
        ix
    }

    /// Records how many children `ix` really has (when only some are loaded).
    pub fn set_total_children(&mut self, ix: NodeIx, total: u32) {
        self.tree.nodes[ix as usize].total_children = total;
    }

    pub fn meta_mut(&mut self, ix: NodeIx) -> &mut NodeMeta {
        &mut self.tree.nodes[ix as usize].meta
    }

    pub fn finish(mut self, revision: u64, is_listing: bool, source: ViewSource) -> FolderTree {
        let FolderTree { names, nodes, .. } = &self.tree;
        let name = |ix: NodeIx| {
            let n = &nodes[ix as usize];
            &names[n.name_start as usize..(n.name_start + n.name_len) as usize]
        };
        self.pending_children.sort_unstable_by(|(pa, a), (pb, b)| {
            pa.cmp(pb)
                .then_with(|| nodes[*b as usize].meta.size.cmp(&nodes[*a as usize].meta.size))
                .then_with(|| name(*a).cmp(name(*b)))
        });
        let mut children = Vec::with_capacity(self.pending_children.len());
        let mut i = 0;
        while i < self.pending_children.len() {
            let parent = self.pending_children[i].0;
            let start = children.len() as u32;
            while i < self.pending_children.len() && self.pending_children[i].0 == parent {
                children.push(self.pending_children[i].1);
                i += 1;
            }
            let node = &mut self.tree.nodes[parent as usize];
            node.children_start = start;
            node.children_len = children.len() as u32 - start;
            node.total_children = node.total_children.max(node.children_len);
        }
        self.tree.children = children;
        self.tree.revision = revision;
        self.tree.is_listing = is_listing;
        self.tree.source = source;
        self.tree
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(size: u64) -> NodeMeta {
        NodeMeta { size, ..NodeMeta::default() }
    }

    #[test]
    fn children_sorted_largest_first_then_name_and_paths_resolve() {
        let mut b = FolderTreeBuilder::new(Arc::from(&b"/r"[..]), b"/r/f".to_vec(), b"f", meta(30));
        b.push_ancestor(b"r", meta(100));
        let small = b.push_child(FolderTree::FOLDER, b"b", meta(5));
        let big = b.push_child(FolderTree::FOLDER, b"z", meta(20));
        let tie = b.push_child(FolderTree::FOLDER, b"a", meta(5));
        let deep = b.push_child(big, b"leaf", meta(20));
        let t = b.finish(1, false, ViewSource::Saved);
        assert_eq!(t.children(FolderTree::FOLDER), [big, tie, small]);
        assert_eq!(t.path(deep), b"/r/f/z/leaf");
        assert_eq!(t.path(t.ancestors[0]), b"/r");
    }
}
