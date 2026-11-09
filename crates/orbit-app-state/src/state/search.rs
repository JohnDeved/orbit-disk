//! Search (Swift `search()` ~4009-4129): debounced, preview search while mapping (re-run when
//! the preview changed), saved-index search afterwards, at most 300 rows, previous results kept
//! until their replacement lands, every result checked against the query, generation, session,
//! removal epoch and snapshot it was issued for.

use std::sync::Arc;
use std::time::Duration;

use orbit_platform::FileIdentity;

use super::{AppState, Pending};
use crate::backend::{Request, SearchReply, SearchRequest, SearchSource};
use crate::event::{Regions, TimerKind};
use crate::hash::FxHashMap;
use crate::ids::RequestId;
use crate::model::Item;

/// Rows a search returns at most (the total match count is reported separately).
pub const SEARCH_LIMIT: usize = 300;
/// Debounce when the search index is ready.
pub const SEARCH_DELAY_READY: Duration = Duration::from_millis(16);
/// Debounce while the index is being prepared or removals are overlaid.
pub const SEARCH_DELAY_PREPARING: Duration = Duration::from_millis(100);
/// A preview search whose preview changed meanwhile runs again after this.
pub const SEARCH_RERUN_DELAY: Duration = Duration::from_millis(1_500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Preview,
    Saved { snapshot: u64, stable: bool },
}

#[derive(Debug)]
pub(crate) struct SearchState {
    pub query: String,
    pub results: Arc<[Item]>,
    lookup: FxHashMap<Arc<[u8]>, u32>,
    pub count: u64,
    pub searching: bool,
    pub incomplete: bool,
    result_query: String,
    result_root: Option<(Arc<[u8]>, Option<FileIdentity>)>,
    result_epoch: u64,
    uses_preview_order: bool,
    pub request: Option<RequestId>,
    timer: Option<(u64, Target)>,
    rerun: Option<u64>,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            results: Arc::from([]),
            lookup: FxHashMap::default(),
            count: 0,
            searching: false,
            incomplete: false,
            result_query: String::new(),
            result_root: None,
            result_epoch: 0,
            uses_preview_order: false,
            request: None,
            timer: None,
            rerun: None,
        }
    }
}

impl SearchState {
    pub fn item(&self, path: &[u8]) -> Option<&Item> {
        self.lookup.get(path).map(|&index| &self.results[index as usize])
    }

    pub fn index_of(&self, path: &[u8]) -> Option<u32> {
        self.lookup.get(path).copied()
    }

    fn set_results(&mut self, results: Vec<Item>) {
        self.lookup = results.iter().enumerate().map(|(i, item)| (Arc::clone(&item.path), i as u32)).collect();
        self.results = results.into();
    }
}

impl AppState {
    pub fn query(&self) -> &str {
        &self.search.query
    }

    pub fn is_searching(&self) -> bool {
        self.search.searching
    }

    pub fn search_failed(&self) -> bool {
        self.search.incomplete
    }

    /// Total matches (the list shows at most [`SEARCH_LIMIT`]).
    pub fn search_match_count(&self) -> u64 {
        self.search.count
    }

    /// The rows do not (yet) answer the current query for the current map.
    pub fn search_outdated(&self) -> bool {
        if !self.filtering() {
            return false;
        }
        let session = self.active_session();
        let root = session.map(|s| (Arc::clone(&s.root), s.identity));
        self.search.incomplete
            || self.search.query.trim() != self.search.result_query
            || self.search.result_root != root
            || self.search.result_epoch != session.map_or(0, |s| s.epoch)
    }

    pub(crate) fn set_query(&mut self, query: String) {
        if query == self.search.query {
            return;
        }
        self.search.query = query;
        self.selected = None;
        self.clear_all_hover();
        self.schedule_name_sort();
        self.notify(Regions::LIST | Regions::INSPECTOR | Regions::HEADER);
        self.search();
    }

    pub(crate) fn retry_search(&mut self) {
        if self.search.incomplete && !self.search.searching {
            self.search();
        }
    }

    pub(crate) fn cancel_search(&mut self) {
        let request = self.search.request.take();
        self.cancel(request);
        self.search.timer = None;
        self.search.rerun = None;
    }

    fn replace_results(&mut self, results: Vec<Item>, count: u64) {
        self.search.set_results(results);
        self.search.count = count;
        if self.filtering() {
            self.schedule_name_sort();
            self.clear_hover_relations_for_new_rows();
        }
        self.notify(Regions::LIST | Regions::MAP);
    }

    fn clear_hover_relations_for_new_rows(&mut self) {
        self.hover_dirty = true;
        if let Some(path) = self.row_hover.clone()
            && self.lookup_item(&path).is_none()
        {
            self.row_hover = None;
        }
    }

    pub(crate) fn search(&mut self) {
        self.cancel_search();
        let query = self.search.query.trim().to_owned();
        let session = self.active_session().map(|s| {
            (s.id, Arc::clone(&s.root), s.identity, s.epoch, s.preview_blocked, s.search_ready, s.removals.is_empty())
        });
        if session.as_ref().is_some_and(|s| s.4) {
            self.replace_results(Vec::new(), 0);
            self.search.searching = false;
            self.search.incomplete = true;
            return;
        }
        let root = session.as_ref().map(|s| (Arc::clone(&s.1), s.2));
        let epoch = session.as_ref().map_or(0, |s| s.3);
        let same_root = self.search.result_root == root;
        let same_search = !query.is_empty() && self.search.result_query == query && same_root;
        let keep = !query.is_empty() && same_root && self.search.result_epoch == epoch;
        if !keep {
            self.replace_results(Vec::new(), 0);
        }
        self.search.searching = !query.is_empty();
        if !same_search {
            self.search.uses_preview_order = false;
        }
        self.notify(Regions::LIST | Regions::HEADER);
        let ready = session.as_ref().is_some_and(|s| s.5);
        if self.partial && !query.is_empty() && self.preview_session.is_some() {
            let delay = if ready { SEARCH_DELAY_READY } else { SEARCH_DELAY_PREPARING };
            let token = self.timer(TimerKind::Search, delay);
            self.search.timer = Some((token, Target::Preview));
            return;
        }
        match self.snapshot.as_ref().filter(|_| !self.partial && !query.is_empty()) {
            Some(snapshot) => {
                let removals_empty = session.as_ref().is_none_or(|s| s.6);
                let delay = if removals_empty && ready { SEARCH_DELAY_READY } else { SEARCH_DELAY_PREPARING };
                let target =
                    Target::Saved { snapshot: snapshot.id, stable: same_search && self.search.uses_preview_order };
                let token = self.timer(TimerKind::Search, delay);
                self.search.timer = Some((token, target));
            }
            None => {
                self.replace_results(Vec::new(), 0);
                self.search.searching = false;
                self.search.incomplete = false;
            }
        }
    }

    pub(crate) fn on_search_timer(&mut self, token: u64) {
        let Some((expected, target)) = self.search.timer else { return };
        if expected != token {
            return;
        }
        self.search.timer = None;
        let Some(session) = self.active_session() else { return };
        let query = self.search.query.trim().to_owned();
        let (source, snapshot, stable) = match target {
            Target::Preview => (SearchSource::Preview, None, false),
            Target::Saved { snapshot, stable } => {
                (SearchSource::Saved { snapshot, stable_order: stable }, Some(snapshot), stable)
            }
        };
        let request = SearchRequest {
            session: session.id,
            root: Arc::clone(&session.root),
            source,
            query: query.clone(),
            removals: Arc::clone(&session.removals),
            limit: SEARCH_LIMIT,
        };
        let pending = Pending::Search {
            generation: self.generation,
            session: Some(session.id),
            epoch: session.epoch,
            query,
            snapshot,
            stable,
        };
        self.search.request = Some(self.request(pending, Request::Search(request)));
    }

    pub(crate) fn on_search(&mut self, id: RequestId, pending: Pending, result: Result<SearchReply, String>) {
        let Pending::Search { generation, session, epoch, query, snapshot, stable } = pending else { return };
        if self.search.request == Some(id) {
            self.search.request = None;
        }
        if query != self.search.query.trim() || generation != self.generation || self.active != session {
            return;
        }
        let preview = snapshot.is_none();
        if preview {
            if !self.partial || self.preview_session != session {
                return;
            }
        } else if self.snapshot.as_ref().map(|s| s.id) != snapshot {
            self.search();
            return;
        }
        match result {
            Ok(reply) => {
                if self.active_epoch() != epoch || reply.removals_changed {
                    self.search();
                    return;
                }
                let root = self.active_session().map(|s| (Arc::clone(&s.root), s.identity));
                let preview_revision = self.active_session().map_or(0, |s| s.preview_revision);
                self.replace_results(reply.hits, reply.total);
                self.search.searching = false;
                self.search.incomplete = false;
                self.search.result_query = query;
                self.search.result_root = root;
                self.search.result_epoch = epoch;
                self.search.uses_preview_order = preview || stable;
                // Re-run when the preview is known to have changed since the search read it.
                if preview && reply.preview_revision < preview_revision {
                    self.search.rerun = Some(self.timer(TimerKind::SearchRerun, SEARCH_RERUN_DELAY));
                }
                if let Some(session) = self.active_session_mut() {
                    session.preview_revision = session.preview_revision.max(reply.preview_revision);
                }
            }
            Err(_) => {
                self.search.searching = false;
                self.search.incomplete = true;
                self.notify(Regions::LIST | Regions::HEADER);
            }
        }
    }

    pub(crate) fn on_search_rerun(&mut self, token: u64) {
        if self.search.rerun != Some(token) {
            return;
        }
        self.search.rerun = None;
        if self.partial && self.preview_session.is_some() && self.filtering() {
            self.search();
        }
    }
}
