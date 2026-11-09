//! Collection: eligibility, typed in-app drags, the per-session validation queue, review tokens
//! and confirmation leases. Port of Swift `beginCollectionDrag`, `collectDrag`,
//! `collectInBackground`, `attach/detachCollectionValidation`, `beginCollectionReview` and
//! `begin/endCollectionConfirmation`.
//!
//! Each session owns its collection and its queue, so a job can only ever publish into its own
//! root (the cross-root guards of the Swift code hold by construction).

use std::sync::Arc;

use super::{AppState, AttachedValidation, Drag, Pending, STATUS_CHECKING_COLLECTION};
use crate::backend::{Request, ValidateRequest, Validation};
use crate::collection::{
    Candidates, CollectJob, CollectRequest, CollectionSnapshot, GroupContext, LARGE_DROP, MAX_COMMIT_BATCH,
    VALIDATION_SLICE, apply_validated, can_begin_collection,
};
use crate::event::{ComputeJob, DragSource, Effect, Regions};
use crate::hash::FxHashSet;
use crate::ids::{DragId, LeaseId, RequestId, SessionId};
use crate::model::Item;
use crate::paths::has_collected_ancestor;
use crate::review::ReviewToken;

/// Removing an item from a collection of at least this many items rebuilds it off the UI thread
/// (Swift: more than 1,000).
pub const BACKGROUND_EDIT_ITEMS: usize = 1_000;

const MAP_CHANGED_DRAG_ONE: &str = "The map changed. Drag the item again from the current map.";
const MAP_CHANGED_DRAG_MANY: &str = "The map changed. Drag the items again from the current map.";

#[derive(Clone, Copy, Debug)]
pub(crate) enum ValidationKind {
    /// Re-check buffered chunk `index` before committing a large drop.
    Recheck { index: usize },
    /// Validate one slice ending at `end`; `commit` publishes it with the buffered chunks.
    Slice { end: usize, commit: bool },
}

/// Items being removed from a large collection (Swift `CollectionEditQueue`). The rebuilt
/// snapshot is computed in the background from the collection revision it started from; when
/// the collection changed meanwhile, the same removals are applied again to the new one.
#[derive(Debug)]
pub(crate) struct CollectionEdit {
    pub request: RequestId,
    pub remove: Arc<FxHashSet<Arc<[u8]>>>,
}

#[derive(Debug)]
pub(crate) struct PendingValidation {
    pub job: u64,
    pub kind: ValidationKind,
    pub revision: u64,
    pub epoch: u64,
    /// Already-included and duplicate items found while preparing the slice.
    pub included: usize,
    pub count: usize,
}

impl AppState {
    // -------------------------------------------------------------------------------------
    // Eligibility.

    pub(crate) fn group_context(&self) -> Option<GroupContext> {
        let (root, _) = self.displayed_root()?;
        let collected = self.collection().map(|c| Arc::clone(c.paths())).unwrap_or_default();
        Some(GroupContext {
            root: Arc::from(root),
            collected,
            home: Arc::from(&self.cfg.home[..]),
            protected: Arc::from(self.cfg.protected.as_slice()),
        })
    }

    /// The inspector's "Add to Collection" is enabled for this item.
    pub fn can_collect(&self, item: &Item) -> bool {
        self.displayed_matches_active()
            && self.group_context().is_some_and(|context| can_begin_collection(item, &context))
    }

    pub fn can_collect_selection(&self) -> bool {
        self.selected.as_ref().is_some_and(|item| self.can_collect(item))
    }

    /// Validation of the displayed collection queue is running.
    pub fn is_validating_collection(&self) -> bool {
        self.validation.is_some()
    }

    // -------------------------------------------------------------------------------------
    // Drag and drop (typed, single-use ids instead of pasteboard tokens).

    pub fn drag_id(&self) -> Option<DragId> {
        self.drag.as_ref().map(|drag| drag.id)
    }

    pub fn drag_label(&self) -> Option<&str> {
        self.drag.as_ref().map(|drag| drag.label.as_str())
    }

    /// The tray shows the drop target ("Release to collect") for the current drag.
    pub fn drop_target_active(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.over_tray) && self.can_accept_drop()
    }

    /// Swift `canAcceptCollectionDrop`: the drag belongs to the displayed, active root.
    pub fn can_accept_drop(&self) -> bool {
        let (Some(drag), Some((root, identity))) = (self.drag.as_ref(), self.displayed_root()) else { return false };
        self.displayed_matches_active() && *drag.root == *root && drag.root_identity == identity
    }

    pub(crate) fn clear_drag(&mut self) {
        if self.drag.take().is_some() {
            self.notify(Regions::TRAY);
        }
    }

    pub(crate) fn begin_drag(&mut self, id: DragId, source: DragSource, label: String) {
        self.clear_drag();
        if !self.displayed_matches_active() {
            return;
        }
        let Some(context) = self.group_context() else { return };
        let Some(folder) = self.folder.clone() else { return };
        let candidates = match source {
            DragSource::Segment { segment, layout_revision } => {
                let Some(presentation) = self.presentation() else { return };
                if presentation.layout_revision != layout_revision {
                    return;
                }
                let layout = &presentation.layout;
                match layout.segments().get(segment) {
                    Some(found) => match found.node {
                        Some(ix) => Candidates::Items(Arc::from(vec![folder.item(ix)])),
                        None => match layout.group(segment) {
                            Some(group) => {
                                Candidates::Members { folder: Arc::clone(&folder), members: group.members.clone() }
                            }
                            None => return,
                        },
                    },
                    None => return,
                }
            }
            DragSource::Items(paths) => {
                let items: Vec<Item> = paths.iter().filter_map(|path| self.lookup_item(path)).collect();
                Candidates::Items(items.into())
            }
        };
        let count = candidates.len();
        if count == 0 {
            return;
        }
        let large = count > LARGE_DROP;
        let (candidates, group) = if large {
            // Large groups validate eligibility per slice in the background.
            (candidates, Some(Arc::new(context)))
        } else {
            let all = candidates.all();
            let (eligible, omitted): (Vec<Item>, Vec<Item>) =
                all.into_iter().partition(|item| can_begin_collection(item, &context));
            if eligible.is_empty() {
                let included = omitted.iter().all(|item| has_collected_ancestor(&item.path, &context.collected, true));
                self.set_status(Some(if included {
                    "These items are already included in the collection.".into()
                } else {
                    "These items cannot be collected. Refresh the map if they have changed.".into()
                }));
                return;
            }
            let mut ordered = eligible;
            ordered.extend(omitted);
            (Candidates::Items(ordered.into()), None)
        };
        let Some((root, identity)) = self.displayed_root() else { return };
        self.drag = Some(Drag {
            id,
            root: Arc::from(root),
            root_identity: identity,
            candidates,
            group,
            label,
            over_tray: false,
        });
        self.notify(Regions::TRAY);
    }

    pub(crate) fn set_drag_over_tray(&mut self, id: DragId, over: bool) {
        if let Some(drag) = self.drag.as_mut().filter(|drag| drag.id == id)
            && drag.over_tray != over
        {
            drag.over_tray = over;
            self.notify(Regions::TRAY);
        }
    }

    /// Swift `collectDrag`: consumes the drag; a stale or foreign drag collects nothing.
    pub(crate) fn drop_drag(&mut self, id: DragId) {
        if self.drag.as_ref().is_none_or(|drag| drag.id != id) {
            return;
        }
        let accept = self.can_accept_drop();
        let Some(drag) = self.drag.take() else { return };
        self.notify(Regions::TRAY);
        let session = self.active.filter(|_| accept);
        let Some(session) = session else {
            self.set_status(Some(MAP_CHANGED_DRAG_ONE.into()));
            return;
        };
        self.collect_in_background(session, drag.candidates, drag.group);
    }

    pub(crate) fn end_drag(&mut self, id: DragId) {
        if self.drag.as_ref().is_some_and(|drag| drag.id == id) {
            self.clear_drag();
        }
    }

    /// "Add to Collection" for the selection (or automation).
    pub(crate) fn collect_paths(&mut self, paths: &[Arc<[u8]>]) -> bool {
        if !self.displayed_matches_active() || paths.is_empty() {
            return false;
        }
        let items: Vec<Item> = paths.iter().filter_map(|path| self.lookup_item(path)).collect();
        if items.is_empty() {
            return false;
        }
        let Some(session) = self.active else { return false };
        self.collect_in_background(session, Candidates::Items(items.into()), None)
    }

    // -------------------------------------------------------------------------------------
    // The validation queue.

    fn collect_in_background(
        &mut self,
        session: SessionId,
        items: Candidates,
        group: Option<Arc<GroupContext>>,
    ) -> bool {
        if items.len() == 0 {
            return false;
        }
        let job_id = self.token();
        let previous_status = self.status_revision;
        let active = self.active == Some(session);
        let Some(s) = self.session_mut(session) else { return false };
        if s.identity.is_none() {
            return false;
        }
        if let Some(job) = &mut s.job {
            if job.cancelled {
                return false;
            }
            job.requests.push_back(CollectRequest::new(items, group));
            return true;
        }
        let mut job = CollectJob::new(job_id, s.collection_status_revision);
        job.requests.push_back(CollectRequest::new(items, group));
        s.job = Some(job);
        if active {
            self.attach_validation(Some(previous_status));
        }
        self.pump_collection(session);
        true
    }

    /// The session's queue waits while it removes files or a confirmation holds publication.
    fn queue_blocked(&self, session: SessionId) -> bool {
        self.session(session).is_none_or(|s| s.removing)
    }

    /// Starts the next validation step of `session`'s queue, if idle.
    pub(crate) fn pump_collection(&mut self, session: SessionId) {
        if self.queue_blocked(session) {
            return;
        }
        let Some(s) = self.session(session) else { return };
        let Some(job) = s.job.as_ref().filter(|job| job.inflight.is_none() && !job.cancelled) else { return };
        let Some(request) = job.requests.front() else { return };
        let (Some(root_identity), root) = (s.identity, Arc::clone(&s.root)) else { return };
        let job_id = job.id;
        let revision = s.collection.revision();
        let epoch = s.epoch;
        let total = request.items.len();
        let end = request.offset + VALIDATION_SLICE.min(total - request.offset);
        let count = end - request.offset;
        let commit = request.buffer.as_ref().is_none_or(|b| b.processed + count >= b.target || end == total);
        // A commit of buffered chunks first re-checks each chunk against the current collection.
        let mut recheck = None;
        if commit && let Some(buffer) = request.buffer.as_ref().filter(|b| !b.chunks.is_empty()) {
            let fresh = buffer.recheck_revision != Some(revision) || buffer.recheck_epoch != Some(epoch);
            let index = if fresh { 0 } else { buffer.recheck_index };
            if fresh || !buffer.ready {
                recheck = Some((index, buffer.chunks[index].clone(), fresh));
            }
        }
        let collected = Arc::clone(s.collection.paths());
        let fresh_recheck = recheck.as_ref().is_some_and(|r| r.2);
        let (items, kind, included) = match recheck {
            Some((index, chunk, _)) => (chunk, ValidationKind::Recheck { index }, 0),
            None => {
                let slice = request.items.slice(request.offset..end);
                let ordered = match &request.group {
                    Some(context) => {
                        let (eligible, omitted): (Vec<Item>, Vec<Item>) =
                            slice.into_iter().partition(|item| can_begin_collection(item, context));
                        eligible.into_iter().chain(omitted).collect()
                    }
                    None => slice,
                };
                let mut seen: FxHashSet<Arc<[u8]>> = FxHashSet::default();
                let mut included = 0;
                let mut candidates = Vec::with_capacity(ordered.len());
                for item in ordered {
                    if !seen.insert(Arc::clone(&item.path)) || has_collected_ancestor(&item.path, &collected, true) {
                        included += 1;
                    } else {
                        candidates.push(item);
                    }
                }
                (candidates, ValidationKind::Slice { end, commit }, included)
            }
        };
        if fresh_recheck
            && let Some(buffer) = self
                .session_mut(session)
                .and_then(|s| s.job.as_mut())
                .and_then(|job| job.requests.front_mut())
                .and_then(|request| request.buffer.as_mut())
        {
            buffer.recheck_revision = Some(revision);
            buffer.recheck_epoch = Some(epoch);
            buffer.recheck_index = 0;
            buffer.ready = false;
        }
        let pending = Pending::Validate(PendingValidation { job: job_id, kind, revision, epoch, included, count });
        let request = Request::Validate(ValidateRequest { session, root, root_identity, items });
        let id = self.request(pending, request);
        if let Some(job) = self.session_mut(session).and_then(|s| s.job.as_mut()) {
            job.inflight = Some(id);
        }
    }

    fn session_of_job(&self, job: u64) -> Option<SessionId> {
        self.sessions.iter().find(|s| s.job.as_ref().is_some_and(|j| j.id == job)).map(|s| s.id)
    }

    /// Whether an open permanent-delete confirmation of `session` holds collection changes.
    fn confirmation_holds(&self, session: SessionId) -> bool {
        self.confirmation.as_ref().is_some_and(|(_, token)| token.session == session && self.review_has_owner(token))
    }

    pub(crate) fn on_validation_lost(&mut self, id: RequestId, pending: PendingValidation) {
        let Some(session) = self.session_of_job(pending.job) else { return };
        if let Some(job) = self.session_mut(session).and_then(|s| s.job.as_mut())
            && job.inflight == Some(id)
        {
            job.inflight = None;
        }
        self.pump_collection(session);
    }

    pub(crate) fn on_validated(
        &mut self,
        id: RequestId,
        pending: PendingValidation,
        result: Result<Validation, String>,
    ) {
        let Some(session) = self.session_of_job(pending.job) else { return };
        {
            let Some(job) = self.session_mut(session).and_then(|s| s.job.as_mut()) else { return };
            if job.inflight != Some(id) || job.cancelled {
                return;
            }
            job.inflight = None;
        }
        let Some(s) = self.session(session) else { return };
        if s.removing {
            // Resumes when the removal finishes.
            return;
        }
        if s.collection.revision() != pending.revision || s.epoch != pending.epoch {
            self.pump_collection(session);
            return;
        }
        let validation = match result {
            Ok(validation) if validation.root_matches => validation,
            _ => {
                self.fail_job(session);
                return;
            }
        };
        let holds = self.confirmation_holds(session);
        let limit = MAX_COMMIT_BATCH;
        let Some(s) = self.session_mut(session) else { return };
        let collection = s.collection.clone();
        let Some(job) = s.job.as_mut() else { return };
        match pending.kind {
            ValidationKind::Recheck { index } => {
                let Some(mut request) = job.requests.pop_front() else { return };
                if let Some(buffer) = request.buffer.as_mut() {
                    buffer.chunks[index] = validation.accepted;
                    buffer.recheck_index = index + 1;
                    buffer.ready = buffer.recheck_index == buffer.chunks.len();
                }
                job.note_rejections(&validation.rejections);
                // The re-checked request goes to the back: small requests overtake it.
                job.requests.push_back(request);
                self.pump_collection(session);
            }
            ValidationKind::Slice { end, commit: false } => {
                let Some(mut request) = job.requests.pop_front() else { return };
                if let Some(buffer) = request.buffer.as_mut() {
                    buffer.chunks.push(validation.accepted);
                    buffer.processed += pending.count;
                    buffer.ready = false;
                }
                job.included_count += pending.included;
                job.note_rejections(&validation.rejections);
                job.did_chunk = true;
                request.offset = end;
                job.requests.push_back(request);
                self.pump_collection(session);
            }
            ValidationKind::Slice { end, commit: true } => {
                let Some(request) = job.requests.front() else { return };
                let mut combined: Vec<Item> = Vec::new();
                let mut distinct: FxHashSet<Arc<[u8]>> = FxHashSet::default();
                let mut duplicates = 0;
                let buffered = request.buffer.iter().flat_map(|b| b.chunks.iter().flatten());
                for item in buffered.chain(validation.accepted.iter()) {
                    if distinct.insert(Arc::clone(&item.path)) {
                        combined.push(item.clone());
                    } else {
                        duplicates += 1;
                    }
                }
                let application =
                    apply_validated(&combined, collection.items(), collection.paths(), pending.included + duplicates);
                if application.items.is_some() && holds {
                    // Held until the confirmation ends; then this slice validates again.
                    return;
                }
                if job.added_count == 0 {
                    job.added_name = application.added_name.clone();
                }
                job.added_count += application.added_count;
                job.included_count += application.included_count;
                job.note_rejections(&validation.rejections);
                let Some(mut request) = job.requests.pop_front() else { return };
                if let Some(buffer) = request.buffer.as_mut() {
                    buffer.chunks.clear();
                    buffer.processed = 0;
                    buffer.recheck_index = 0;
                    buffer.recheck_revision = None;
                    buffer.recheck_epoch = None;
                    buffer.ready = false;
                    buffer.target = (buffer.target * 2).min(limit);
                }
                if end < request.items.len() {
                    job.did_chunk = true;
                    request.offset = end;
                    job.requests.push_back(request);
                }
                let more = !job.requests.is_empty();
                if let Some(items) = application.items {
                    let revision = self.token();
                    if let Some(s) = self.session_mut(session) {
                        s.collection = CollectionSnapshot::new(items, revision);
                    }
                    if self.active == Some(session) {
                        self.notify(
                            Regions::TRAY | Regions::MAP | Regions::LIST | Regions::INSPECTOR | Regions::SHEETS,
                        );
                    }
                }
                if more {
                    self.pump_collection(session);
                } else {
                    self.finish_job(session);
                }
            }
        }
    }

    fn finish_job(&mut self, session: SessionId) {
        let attached = self.validation.filter(|v| v.session == session && self.active == Some(session));
        let Some(s) = self.session_mut(session) else { return };
        let Some(job) = s.job.take() else { return };
        let (messages, rejection) = job.messages();
        let attached = attached.filter(|v| v.job == job.id);
        if attached.is_none() {
            if !messages.is_empty() && s.collection_status_revision == job.initial_status_revision {
                s.collection_status = Some(messages.join(" "));
            }
            if let Some(rejection) = &rejection {
                s.enqueue_collection_error(rejection);
            }
        }
        if self.active != Some(session) {
            return;
        }
        if let Some(attached) = attached {
            self.validation = None;
            self.notify(Regions::TRAY);
            if let Some(rejection) = rejection {
                let free = self.error.is_none()
                    && self
                        .session(session)
                        .is_some_and(|s| s.collection_error.is_none() && s.presented_collection_error.is_none());
                if free {
                    if let Some(s) = self.session_mut(session) {
                        s.presented_collection_error = Some(rejection.clone());
                    }
                    self.set_error_quietly(Some(rejection));
                } else if let Some(s) = self.session_mut(session) {
                    s.enqueue_collection_error(&rejection);
                }
            }
            if attached.status_revision == Some(self.status_revision)
                && self.status.as_deref() == Some(STATUS_CHECKING_COLLECTION)
                && !messages.is_empty()
            {
                self.set_status(Some(messages.join(" ")));
            }
        }
        if self.error.is_none() {
            self.present_pending_error = true;
        }
    }

    fn fail_job(&mut self, session: SessionId) {
        let attached = self.validation.filter(|v| v.session == session && self.active == Some(session));
        let Some(s) = self.session_mut(session) else { return };
        let Some(job) = s.job.take() else { return };
        let attached = attached.filter(|v| v.job == job.id);
        let mut progress_revision = None;
        if let Some(attached) = attached {
            progress_revision = attached.status_revision;
            self.detach_validation();
        }
        let can_present = self.active == Some(session) && self.displayed_matches_active();
        let detached_progress = progress_revision.is_some_and(|r| self.status_revision == r.wrapping_add(1));
        if can_present && detached_progress && self.status.is_none() {
            self.set_status(Some(MAP_CHANGED_DRAG_MANY.into()));
        } else {
            if let Some(s) = self.session_mut(session) {
                s.enqueue_collection_error(MAP_CHANGED_DRAG_MANY);
            }
            if can_present && self.error.is_none() {
                self.present_pending_error = true;
            }
        }
    }

    /// Shows the displayed session's running queue in the tray (Swift `attachCollectionValidation`).
    pub(crate) fn attach_validation(&mut self, previous_status: Option<u64>) {
        if !self.displayed_matches_active() {
            return;
        }
        let Some(session) = self.active else { return };
        let Some(s) = self.session(session) else { return };
        let Some(job) = s.job.as_ref() else { return };
        if self.validation.is_some_and(|v| v.job == job.id) {
            return;
        }
        let show_progress = self.status_revision == previous_status.unwrap_or(self.status_revision)
            && s.collection_status_revision == job.initial_status_revision;
        let job_id = job.id;
        let mut attached = AttachedValidation { session, job: job_id, status_revision: None };
        if show_progress {
            self.set_status_quietly(Some(STATUS_CHECKING_COLLECTION.into()));
            attached.status_revision = Some(self.status_revision);
        }
        self.validation = Some(attached);
        self.notify(Regions::TRAY);
    }

    /// Swift `detachCollectionValidation`: the queue keeps running in its session.
    pub(crate) fn detach_validation(&mut self) {
        let Some(attached) = self.validation.take() else { return };
        if attached.status_revision == Some(self.status_revision) {
            self.set_status_quietly(None);
        }
        self.notify(Regions::TRAY);
    }

    fn cancel_job(&mut self, session: SessionId) {
        let inflight = self.session_mut(session).and_then(|s| s.job.take()).and_then(|job| job.inflight);
        self.cancel(inflight);
        if self.validation.is_some_and(|v| v.session == session) {
            self.detach_validation();
        }
    }

    // -------------------------------------------------------------------------------------
    // Review and confirmation.

    pub fn show_review(&self) -> bool {
        self.show_review
    }

    /// The current review token while the review sheet is open.
    pub fn review_token(&self) -> Option<ReviewToken> {
        if !self.show_review || !self.displayed_matches_active() {
            return None;
        }
        let (root, identity) = self.displayed_root()?;
        let session = self.active_session()?;
        Some(ReviewToken {
            session: session.id,
            root: Arc::from(root),
            root_identity: identity?,
            review: self.review_id,
            collection_revision: session.collection.revision(),
            error_revision: self.error_revision,
        })
    }

    pub fn review_has_owner(&self, token: &ReviewToken) -> bool {
        self.review_token().is_some_and(|current| {
            current.session == token.session
                && current.root == token.root
                && current.root_identity == token.root_identity
                && current.review == token.review
        })
    }

    /// The token's owner is current and the collection is unchanged since it was issued.
    pub fn review_matches(&self, token: &ReviewToken) -> bool {
        self.review_has_owner(token)
            && self.active_session().is_some_and(|s| s.collection.revision() == token.collection_revision)
    }

    pub fn confirmation_lease(&self) -> Option<LeaseId> {
        self.confirmation.as_ref().map(|(lease, _)| *lease)
    }

    fn can_review(&self) -> bool {
        self.collection().is_some_and(|c| !c.is_empty())
    }

    /// Opens the review sheet; returns its token.
    pub(crate) fn begin_review(&mut self) -> Option<ReviewToken> {
        if self.show_review || !self.can_review() || self.is_removing() || self.is_updating_collection() {
            return None;
        }
        self.review_id = self.token();
        self.show_review = true;
        self.notify(Regions::SHEETS);
        self.review_token()
    }

    pub(crate) fn begin_confirmation(&mut self, token: &ReviewToken) -> Option<LeaseId> {
        if !self.review_matches(token)
            || self.is_removing()
            || self.is_updating_collection()
            || self.confirmation.is_some()
        {
            return None;
        }
        let lease = LeaseId(self.token());
        self.confirmation = Some((lease, token.clone()));
        self.notify(Regions::SHEETS);
        Some(lease)
    }

    pub(crate) fn end_confirmation(&mut self, lease: LeaseId) {
        if self.confirmation.as_ref().is_none_or(|(current, _)| *current != lease) {
            return;
        }
        let (_, token) = self.confirmation.take().expect("checked");
        self.notify(Regions::SHEETS);
        self.pump_collection(token.session);
    }

    fn close_review(&mut self) {
        self.show_review = false;
        if let Some((_, token)) = self.confirmation.take() {
            self.pump_collection(token.session);
        }
        self.notify(Regions::SHEETS);
    }

    pub(crate) fn dismiss_review(&mut self, token: &ReviewToken) {
        if !self.show_review || token.review != self.review_id {
            return;
        }
        if self.review_has_owner(token) && self.error_revision == token.error_revision {
            self.set_error(None);
        }
        self.close_review();
    }

    pub(crate) fn dismiss_collection_error(&mut self, token: &ReviewToken) {
        if self.review_has_owner(token) && self.error_revision == token.error_revision {
            self.set_error(None);
        }
    }

    /// Empties the collection, cancelling queued additions.
    pub(crate) fn clear_collection(&mut self, token: &ReviewToken) {
        if !self.review_matches(token) || self.is_removing() {
            return;
        }
        let Some(session) = self.active else { return };
        self.cancel_job(session);
        let edit = self.session_mut(session).and_then(|s| s.edit.take()).map(|edit| edit.request);
        self.cancel(edit);
        let revision = self.token();
        if let Some(s) = self.session_mut(session) {
            s.collection = CollectionSnapshot::empty(revision);
        }
        if self.error_revision == token.error_revision {
            self.set_error(None);
        }
        self.close_review();
        self.notify(Regions::TRAY | Regions::MAP | Regions::LIST | Regions::INSPECTOR);
    }

    /// Removes one item from the collection (tray row or review sheet).
    pub(crate) fn uncollect(&mut self, path: &[u8], review: Option<&ReviewToken>) {
        if !self.displayed_matches_active() || self.is_removing() {
            return;
        }
        if review.is_some_and(|token| !self.review_matches(token)) {
            return;
        }
        let Some(session) = self.active else { return };
        let Some(s) = self.session(session) else { return };
        if !s.collection.contains(path) || s.edit.as_ref().is_some_and(|edit| edit.remove.contains(path)) {
            return;
        }
        if s.edit.is_none() && s.collection.len() < BACKGROUND_EDIT_ITEMS {
            let revision = self.token();
            let Some(s) = self.session_mut(session) else { return };
            let remove: FxHashSet<&[u8]> = std::iter::once(path).collect();
            s.collection = s.collection.without(&remove, revision);
            self.notify(Regions::TRAY | Regions::MAP | Regions::LIST | Regions::INSPECTOR | Regions::SHEETS);
            return;
        }
        let mut remove = s.edit.as_ref().map(|edit| (*edit.remove).clone()).unwrap_or_default();
        remove.insert(Arc::from(path));
        self.schedule_collection_edit(session, Arc::new(remove));
    }

    /// (Re)starts rebuilding `session`'s collection without `remove` in the background.
    fn schedule_collection_edit(&mut self, session: SessionId, remove: Arc<FxHashSet<Arc<[u8]>>>) {
        let previous = self.session_mut(session).and_then(|s| s.edit.take()).map(|edit| edit.request);
        self.cancel(previous);
        let Some(s) = self.session(session) else { return };
        let base = s.collection.clone();
        let id = RequestId(self.token());
        let revision = self.token();
        self.pending.insert(id, Pending::CollectionEdit { session, base: base.revision() });
        self.out.push(Effect::Compute {
            id,
            job: ComputeJob::CollectionWithout { collection: base, remove: Arc::clone(&remove), revision },
        });
        if let Some(s) = self.session_mut(session) {
            s.edit = Some(CollectionEdit { request: id, remove });
        }
        self.notify(Regions::TRAY | Regions::SHEETS | Regions::INSPECTOR);
    }

    /// A background collection edit finished: install it unless the collection changed since it
    /// started, in which case the same removals are applied to the current collection.
    pub(crate) fn on_collection_edited(
        &mut self,
        id: RequestId,
        session: SessionId,
        base: u64,
        collection: CollectionSnapshot,
    ) {
        let Some(s) = self.session_mut(session) else { return };
        let Some(edit) = s.edit.take_if(|edit| edit.request == id) else { return };
        if s.collection.revision() != base {
            self.schedule_collection_edit(session, edit.remove);
            return;
        }
        s.collection = collection;
        self.notify(Regions::TRAY | Regions::MAP | Regions::LIST | Regions::INSPECTOR | Regions::SHEETS);
    }

    /// Items are being removed from the displayed collection in the background (the tray says
    /// "Removing from collection…"; review, removal and other edits wait).
    pub fn is_updating_collection(&self) -> bool {
        self.displayed_matches_active() && self.active_session().is_some_and(|s| s.edit.is_some())
    }

    /// `path` is being removed from the displayed collection.
    pub fn is_removing_from_collection(&self, path: &[u8]) -> bool {
        self.displayed_matches_active()
            && self.active_session().and_then(|s| s.edit.as_ref()).is_some_and(|edit| edit.remove.contains(path))
    }

    pub(crate) fn confirm_removal(&mut self, token: &ReviewToken, permanently: bool) {
        if self.review_matches(token) {
            self.start_removal(permanently);
        }
    }
}
