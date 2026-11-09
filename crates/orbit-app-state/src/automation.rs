//! [`orbit_automation::AutomationModel`] over [`AppState`], so automation runs headlessly.
//!
//! The host keeps an `orbit_automation::Controller<AppState>`, runs `controller.handle(&mut
//! state, request)` on the UI thread, then performs `state.take_automation_effects()`. Commands
//! use the time of the last handled event.

use std::path::Path;
use std::sync::Arc;

use orbit_automation::{
    AutomationModel, ItemInfo, ModelState, RemovalMode, RemovalReceipt, RemovalStart, ScanState, SessionId,
};
use orbit_core::types::ScanState as CoreScanState;
use orbit_core::view::FolderTree;

use crate::event::{Effect, Event};
use crate::ids::RemovalId;
use crate::model::Item;
use crate::review::ReviewToken;
use crate::state::AppState;

impl AppState {
    /// Effects produced by automation commands (the controller calls the model directly).
    /// The host performs them after each `Controller::handle`.
    pub fn take_automation_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.automation_effects)
    }

    fn apply(&mut self, event: Event) {
        let effects = self.handle(event, self.now);
        self.automation_effects.extend(effects);
    }

    fn command<R>(&mut self, command: impl FnOnce(&mut AppState) -> R) -> R {
        let (result, effects) = self.step(self.now, command);
        self.automation_effects.extend(effects);
        result
    }
}

/// `dev:ino`, the identity string clients echo back.
pub fn identity_string(item: &Item) -> Option<String> {
    item.meta.identity.map(|id| format!("{}:{}", id.dev, id.ino))
}

impl AutomationModel for AppState {
    type Node = Item;
    type Review = ReviewToken;
    type Receipt = RemovalId;

    fn session(&self) -> Option<SessionId> {
        (self.collection().is_some() && !self.is_opening()).then(|| self.active_session_id()).flatten()
    }

    fn state(&self) -> ModelState {
        let state = self;
        let collection = state.collection();
        ModelState {
            opening: state.is_opening(),
            scanning: state.is_scanning(),
            refreshing: state.is_refreshing(),
            partial_map: state.is_partial(),
            discovered_files: state.footer().map_or(0, |f| f.discovered_files),
            discovered_bytes: state.folder().map_or(0, |f| f.root_meta().size),
            query: state.query().to_owned(),
            filtering: state.filtering(),
            searching: state.is_searching(),
            search_outdated: state.search_outdated(),
            search_failed: state.search_failed(),
            search_match_count: state.search_match_count(),
            sorting_rows: state.is_sorting_rows(),
            can_go_back: state.can_go_up(),
            collection_count: collection.map_or(0, |c| c.len() as u64),
            collection_bytes: collection.map_or(0, |c| c.size()),
            validating_collection: state.is_validating_collection(),
            updating_collection: state.is_updating_collection(),
            removing: state.is_removing(),
            active_removals: state.removal_active(),
            updating_sizes: state.updating_removal_sizes(),
            message: state.status().map(str::to_owned),
            error: state.error().map(str::to_owned),
        }
    }

    fn root(&self) -> Option<Item> {
        let folder = self.folder()?;
        Some(Item::new(folder.root_path().to_vec(), *folder.root_meta()))
    }

    fn current_folder(&self) -> Option<Item> {
        self.folder().map(|folder| folder.item(FolderTree::FOLDER))
    }

    fn describe(&self, node: &Item) -> ItemInfo {
        let scan_state = match node.meta.state {
            CoreScanState::Pending => ScanState::Pending,
            CoreScanState::Scanning => ScanState::Scanning,
            CoreScanState::Complete => ScanState::Complete,
        };
        ItemInfo {
            path: node.path.to_vec(),
            identity: identity_string(node),
            name: node.name().to_vec(),
            directory: node.is_dir(),
            symbolic_link: node.meta.flags.is_symlink,
            accessible: node.meta.flags.accessible,
            logical_bytes: node.meta.size,
            allocated_bytes: node.meta.allocated,
            file_count: node.meta.file_count,
            scan_state,
            sizes_complete: scan_state == ScanState::Complete && !(node.is_dir() && self.updating_removal_sizes()),
            can_collect: self.can_collect(node),
        }
    }

    fn displayed_rows(&self) -> Vec<Item> {
        (0..self.row_count()).filter_map(|i| self.row(i)).map(|row| row.item()).collect()
    }

    fn collection(&self) -> Vec<Item> {
        self.collection().map(|c| c.items().to_vec()).unwrap_or_default()
    }

    fn find(&self, path: &[u8]) -> Option<Item> {
        self.lookup_item(path)
            .or_else(|| self.collection().and_then(|c| c.items().iter().find(|item| *item.path == *path).cloned()))
    }

    fn removal_receipt(&self, receipt: &RemovalId) -> Option<RemovalReceipt> {
        self.receipt(*receipt).map(|r| RemovalReceipt {
            running: r.running,
            removed_count: r.removed_count,
            message: r.status.clone(),
            error: r.error.clone(),
        })
    }

    fn review_matches(&self, review: &ReviewToken) -> bool {
        self.review_matches(review)
    }

    fn scan(&mut self, path: &Path) {
        self.apply(Event::OpenLocation { path: path.as_os_str().as_encoded_bytes().to_vec(), force: false });
    }

    fn set_query(&mut self, query: &str) {
        self.apply(Event::QueryChanged(query.to_owned()));
    }

    fn retry_search(&mut self) {
        self.apply(Event::RetrySearch);
    }

    fn open(&mut self, folder: &Item) {
        self.apply(Event::Open { path: Arc::clone(&folder.path) });
    }

    fn back(&mut self) {
        self.apply(Event::GoUp);
    }

    fn select(&mut self, node: &Item) -> bool {
        self.apply(Event::Select { path: Some(Arc::clone(&node.path)) });
        self.selected().is_some_and(|s| s.path == node.path && s.meta.identity == node.meta.identity)
    }

    fn reveal_selection(&mut self) {
        self.apply(Event::RevealSelection);
    }

    fn preview_selection(&mut self) {
        self.apply(Event::PreviewSelection);
    }

    fn refresh(&mut self) {
        self.apply(Event::Refresh);
    }

    fn stop_scan(&mut self) {
        self.apply(Event::Pause);
    }

    fn collect(&mut self, nodes: &[Item]) -> bool {
        let paths: Vec<Arc<[u8]>> = nodes.iter().map(|n| Arc::clone(&n.path)).collect();
        self.command(|state| state.collect_paths(&paths))
    }

    fn uncollect(&mut self, node: &Item) {
        self.apply(Event::Uncollect { path: Arc::clone(&node.path), review: None });
    }

    fn collection_review(&mut self) -> Option<ReviewToken> {
        if let Some(token) = self.review_token() {
            return Some(token);
        }
        self.command(AppState::begin_review_command)
    }

    fn clear_collection(&mut self, review: &ReviewToken) {
        self.apply(Event::ClearCollection { review: review.clone() });
    }

    fn start_removal(
        &mut self,
        review: &ReviewToken,
        mode: RemovalMode,
        session: SessionId,
    ) -> RemovalStart<RemovalId> {
        if self.active_session_id() != Some(session) {
            return RemovalStart::NotStarted;
        }
        if self.is_removing() || self.confirmation_lease().is_some() {
            return RemovalStart::Busy;
        }
        if !self.review_matches(review) {
            return RemovalStart::NotStarted;
        }
        let permanently = mode == RemovalMode::Permanent;
        match self.command(|state| state.start_removal_command(permanently)) {
            Some(id) => RemovalStart::Started(id),
            None => RemovalStart::NotStarted,
        }
    }
}
