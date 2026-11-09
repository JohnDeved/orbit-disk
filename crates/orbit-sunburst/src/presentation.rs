//! Layout + per-segment collection state, cached across renders. Port of Swift
//! `SunburstPresentationCache`.

use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::color::ColorOrdinals;
use crate::layout::{Layout, LayoutSource, SegmentIndex};

/// How much of a segment is in the user's collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CollectionState {
    #[default]
    None,
    /// Every member is collected (hatched, check badge).
    Included,
    /// Some members or descendants are collected (dash badge).
    Partial,
}

static REVISIONS: AtomicU64 = AtomicU64::new(1);

fn next_revision() -> u64 {
    REVISIONS.fetch_add(1, Ordering::Relaxed)
}

/// What the chart paints. Cheap to clone.
#[derive(Debug)]
pub struct Presentation<Id> {
    pub layout: Arc<Layout<Id>>,
    pub states: Arc<[CollectionState]>,
    /// Changes whenever the layout is rebuilt; cached paths and hit data key on it.
    pub layout_revision: u64,
    /// Changes whenever anything painted changes (layout or collection states).
    pub revision: u64,
}

impl<Id> Clone for Presentation<Id> {
    fn clone(&self) -> Self {
        Self {
            layout: Arc::clone(&self.layout),
            states: Arc::clone(&self.states),
            layout_revision: self.layout_revision,
            revision: self.revision,
        }
    }
}

impl<Id> Presentation<Id> {
    pub fn state(&self, index: SegmentIndex) -> CollectionState {
        self.states.get(index).copied().unwrap_or_default()
    }
}

/// Rebuilds the layout only when the root or map revision changes. A collection-only change
/// (new `coverage_revision`) keeps the layout and recomputes the per-segment states.
#[derive(Debug)]
pub struct PresentationCache<Id> {
    key: Option<(Id, u64)>,
    coverage_revision: u64,
    current: Option<Presentation<Id>>,
}

impl<Id> Default for PresentationCache<Id> {
    fn default() -> Self {
        Self { key: None, coverage_revision: 0, current: None }
    }
}

impl<Id: Copy + Eq + Hash> PresentationCache<Id> {
    pub fn new() -> Self {
        Self::default()
    }

    /// `state` computes the collection state of one segment; it is called once per segment, and
    /// only when the layout or `coverage_revision` changed. Colors are read only on rebuild: a color
    /// mapping change must come with a new `map_revision`, as in the Swift app.
    pub fn presentation<S: LayoutSource<Id = Id>>(
        &mut self,
        source: &S,
        root: Id,
        map_revision: u64,
        colors: Option<&ColorOrdinals<Id>>,
        coverage_revision: u64,
        mut state: impl FnMut(&Layout<Id>, SegmentIndex) -> CollectionState,
    ) -> &Presentation<Id> {
        let same_layout = self.key == Some((root, map_revision));
        let current = self.current.take().filter(|_| same_layout);
        let (layout, layout_revision) = match current {
            Some(current) if self.coverage_revision == coverage_revision => return self.current.insert(current),
            Some(current) => (current.layout, current.layout_revision),
            None => (Arc::new(Layout::build(source, root, colors)), next_revision()),
        };
        let states = (0..layout.len()).map(|index| state(&layout, index)).collect();
        self.key = Some((root, map_revision));
        self.coverage_revision = coverage_revision;
        self.current.insert(Presentation { layout, states, layout_revision, revision: next_revision() })
    }

    pub fn invalidate(&mut self) {
        self.key = None;
        self.current = None;
    }
}
