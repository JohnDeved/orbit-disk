//! Collection contents and eligibility. Port of Swift `CollectionSnapshot`,
//! `canBeginGroupedCollection` and `prepareCollectionApplication`.

use std::collections::VecDeque;
use std::sync::Arc;

use orbit_core::paths::is_within;

use orbit_core::view::NodeIx;
use orbit_sunburst::Members;

use crate::coverage::CollectionCoverage;
use crate::folder::PreparedFolder;
use crate::hash::FxHashSet;
use crate::ids::RequestId;
use crate::model::Item;
use crate::paths::{has_collected_ancestor, has_component};

/// Validation sends at most this many items per request (Swift: 128).
pub const VALIDATION_SLICE: usize = 128;
/// Drops larger than one slice buffer validated slices and commit in growing batches up to this.
pub const MAX_COMMIT_BATCH: usize = 8_192;
/// Drops of more than this many items validate eligibility per slice in the background.
pub const LARGE_DROP: usize = 64;

/// Immutable collection state; a new revision for every change.
#[derive(Clone, Debug)]
pub struct CollectionSnapshot {
    items: Arc<[Item]>,
    paths: Arc<FxHashSet<Arc<[u8]>>>,
    coverage: Arc<CollectionCoverage>,
    size: u64,
    revision: u64,
}

impl CollectionSnapshot {
    pub fn empty(revision: u64) -> Self {
        Self::new(Vec::new(), revision)
    }

    pub fn new(items: Vec<Item>, revision: u64) -> Self {
        let paths: FxHashSet<Arc<[u8]>> = items.iter().map(|item| Arc::clone(&item.path)).collect();
        let coverage = CollectionCoverage::new(items.iter().map(|item| &*item.path));
        let size = total(&items);
        Self { items: items.into(), paths: Arc::new(paths), coverage: Arc::new(coverage), size, revision }
    }

    pub fn items(&self) -> &Arc<[Item]> {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn paths(&self) -> &Arc<FxHashSet<Arc<[u8]>>> {
        &self.paths
    }

    pub fn coverage(&self) -> &Arc<CollectionCoverage> {
        &self.coverage
    }

    /// Total bytes; saturates instead of overflowing.
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn contains(&self, path: &[u8]) -> bool {
        self.paths.contains(path)
    }

    /// The same contents under a new revision (switching back to a session).
    pub(crate) fn renewed(&self, revision: u64) -> Self {
        Self { revision, ..self.clone() }
    }

    /// Without the items in `remove`.
    pub(crate) fn without(&self, remove: &FxHashSet<&[u8]>, revision: u64) -> Self {
        Self::new(self.items.iter().filter(|item| !remove.contains(&*item.path)).cloned().collect(), revision)
    }
}

fn total(items: &[Item]) -> u64 {
    items.iter().fold(0u64, |sum, item| sum.saturating_add(item.meta.size))
}

/// What a drag or collect may include (Swift `GroupContext`).
#[derive(Clone, Debug)]
pub struct GroupContext {
    pub root: Arc<[u8]>,
    pub collected: Arc<FxHashSet<Arc<[u8]>>>,
    pub home: Arc<[u8]>,
    pub protected: Arc<[Vec<u8>]>,
}

/// Cheap eligibility check before validation: never the mapped root, an ancestor of home,
/// protected system folders, trash folders, hidden folders, symlinks, inaccessible or
/// identity-less items, or anything already included in the collection.
pub fn can_begin_collection(item: &Item, context: &GroupContext) -> bool {
    let path = &*item.path;
    let flags = item.meta.flags;
    if !flags.accessible || flags.is_symlink || item.meta.identity.is_none() {
        return false;
    }
    if path == &*context.root || !is_within(path, &context.root) {
        return false;
    }
    if has_collected_ancestor(path, &context.collected, true) || is_within(&context.home, path) {
        return false;
    }
    if context.protected.iter().any(|protected| is_within(path, protected)) {
        return false;
    }
    if has_component(path, b".Trash") || has_component(path, b".Trashes") {
        return false;
    }
    let mut components: Vec<&[u8]> = path.split(|b| *b == b'/').filter(|c| !c.is_empty()).collect();
    if !flags.is_dir {
        components.pop();
    }
    !components.iter().any(|component| component.first() == Some(&b'.'))
}

/// Protected folders (Swift `canCollect`): system locations and `~/Library`.
pub fn default_protected(home: &[u8]) -> Vec<Vec<u8>> {
    let mut library = home.to_vec();
    library.extend_from_slice(b"/Library");
    let mut list: Vec<Vec<u8>> = ["/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc"]
        .iter()
        .map(|p| p.as_bytes().to_vec())
        .collect();
    list.push(library);
    list
}

/// Result of merging validated items into a collection.
#[derive(Debug)]
pub(crate) struct Application {
    /// `None` when nothing was added (collection unchanged).
    pub items: Option<Vec<Item>>,
    pub added_count: usize,
    pub added_name: Option<Vec<u8>>,
    pub included_count: usize,
}

/// Adds `validated` to `existing`: items under a collected (or co-added) ancestor count as
/// already included; existing items under a newly added folder are replaced by it.
pub(crate) fn apply_validated(
    validated: &[Item],
    existing: &[Item],
    collected: &FxHashSet<Arc<[u8]>>,
    included: usize,
) -> Application {
    let candidates: FxHashSet<Arc<[u8]>> = validated.iter().map(|item| Arc::clone(&item.path)).collect();
    let mut included_count = included;
    let mut added: Vec<Item> = Vec::new();
    for item in validated {
        if has_collected_ancestor(&item.path, collected, true) || has_collected_ancestor(&item.path, &candidates, false)
        {
            included_count += 1;
        } else {
            added.push(item.clone());
        }
    }
    if added.is_empty() {
        return Application { items: None, added_count: 0, added_name: None, included_count };
    }
    let added_paths: FxHashSet<Arc<[u8]>> = added.iter().map(|item| Arc::clone(&item.path)).collect();
    let mut result: Vec<Item> =
        existing.iter().filter(|item| !has_collected_ancestor(&item.path, &added_paths, true)).cloned().collect();
    let added_count = added.len();
    let added_name = (added_count == 1).then(|| added[0].name().to_vec());
    result.extend(added);
    Application { items: Some(result), added_count, added_name, included_count }
}

/// Validated slices of a large drop waiting to be committed together.
#[derive(Debug)]
pub(crate) struct ChunkBuffer {
    pub chunks: Vec<Vec<Item>>,
    pub processed: usize,
    pub target: usize,
    pub recheck_index: usize,
    pub recheck_revision: Option<u64>,
    pub recheck_epoch: Option<u64>,
    pub ready: bool,
}

impl ChunkBuffer {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            processed: 0,
            target: VALIDATION_SLICE,
            recheck_index: 0,
            recheck_revision: None,
            recheck_epoch: None,
            ready: false,
        }
    }
}

/// Items of a collect request, materialized one validation slice at a time (a group drop of a
/// million-child folder keeps sharing the folder's child list).
#[derive(Clone, Debug)]
pub(crate) enum Candidates {
    Items(Arc<[Item]>),
    Members { folder: Arc<PreparedFolder>, members: Members<NodeIx> },
}

impl Candidates {
    pub fn len(&self) -> usize {
        match self {
            Self::Items(items) => items.len(),
            Self::Members { members, .. } => members.len(),
        }
    }

    pub fn slice(&self, range: std::ops::Range<usize>) -> Vec<Item> {
        match self {
            Self::Items(items) => items[range].to_vec(),
            Self::Members { folder, members } => members[range].iter().map(|&ix| folder.item(ix)).collect(),
        }
    }

    pub fn all(&self) -> Vec<Item> {
        self.slice(0..self.len())
    }
}

/// One accepted collect request (a drop or the Add button).
#[derive(Debug)]
pub(crate) struct CollectRequest {
    pub items: Candidates,
    pub group: Option<Arc<GroupContext>>,
    pub offset: usize,
    pub buffer: Option<ChunkBuffer>,
}

impl CollectRequest {
    pub fn new(items: Candidates, group: Option<Arc<GroupContext>>) -> Self {
        let buffer = (items.len() > VALIDATION_SLICE).then(ChunkBuffer::new);
        Self { items, group, offset: 0, buffer }
    }
}

/// A session's collection queue (Swift `CollectionValidationJob`): requests validate one slice
/// at a time; a request's remainder goes to the back so small requests overtake large drops.
#[derive(Debug)]
pub(crate) struct CollectJob {
    pub id: u64,
    pub requests: VecDeque<CollectRequest>,
    pub inflight: Option<RequestId>,
    pub did_chunk: bool,
    pub added_count: usize,
    pub added_name: Option<Vec<u8>>,
    pub included_count: usize,
    pub rejection_count: usize,
    pub rejection_examples: Vec<String>,
    pub initial_status_revision: u64,
    pub cancelled: bool,
}

impl CollectJob {
    pub fn new(id: u64, initial_status_revision: u64) -> Self {
        Self {
            id,
            requests: VecDeque::new(),
            inflight: None,
            did_chunk: false,
            added_count: 0,
            added_name: None,
            included_count: 0,
            rejection_count: 0,
            rejection_examples: Vec::new(),
            initial_status_revision,
            cancelled: false,
        }
    }

    pub fn note_rejections(&mut self, rejections: &[String]) {
        self.rejection_count += rejections.len();
        let room = 5usize.saturating_sub(self.rejection_examples.len());
        self.rejection_examples.extend(rejections.iter().take(room).cloned());
    }

    /// The user-visible summary and rejection detail (Swift wording).
    pub fn messages(&self) -> (Vec<String>, Option<String>) {
        let mut messages = Vec::new();
        if self.did_chunk && self.added_count > 0 {
            messages.push("Collection updated.".to_owned());
        } else if self.added_count == 1 {
            if let Some(name) = &self.added_name {
                messages.push(format!("Added {} to collection.", String::from_utf8_lossy(name)));
            }
        } else if self.added_count > 0 {
            messages.push(format!("Added {} items to collection.", self.added_count));
        }
        if self.included_count > 0 {
            let noun = if self.included_count == 1 { "item already included" } else { "items already included" };
            messages.push(format!("{} {noun}.", self.included_count));
        }
        let mut rejection = None;
        if self.rejection_count > 0 {
            let noun = if self.rejection_count == 1 { "item" } else { "items" };
            messages.push(format!("Skipped {} {noun}.", self.rejection_count));
            let mut detail = self.rejection_examples.join("\n");
            if self.rejection_count > 5 {
                detail.push_str(&format!("\n…and {} more.", self.rejection_count - 5));
            }
            rejection = Some(detail);
        }
        (messages, rejection)
    }
}
