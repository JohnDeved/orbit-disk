//! Removal flow at the state level (Swift `removeCollection`): lock the collection, run the
//! removal off the UI thread, merge outcomes into the owning session (wherever the user is by
//! then), report status and errors, resume monitoring and schedule the refresh.

use std::sync::Arc;

use orbit_platform::FileIdentity;

use super::{AppState, Pending};
use crate::backend::{RemovalOutcome, RemovalRequest, Request};
use crate::event::Regions;
use crate::hash::{FxHashMap, FxHashSet};
use crate::ids::{RemovalId, SessionId};
use crate::model::ScanRemoval;

/// Finished receipts kept for automation.
const FINISHED_RECEIPTS: usize = 32;

#[derive(Debug)]
pub(crate) struct Running {
    pub root: Arc<[u8]>,
    pub identity: Option<FileIdentity>,
    pub items: FxHashMap<Arc<[u8]>, Option<FileIdentity>>,
    pub permanently: bool,
}

/// Progress of one removal, reported to automation (`status.lastRemoval`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub session: SessionId,
    pub running: bool,
    pub removed_count: Option<u64>,
    pub status: String,
    pub error: Option<String>,
}

fn removing_text(permanently: bool) -> &'static str {
    if permanently { "Deleting collected items permanently…" } else { "Moving collected items to Trash…" }
}

fn removed_text(count: usize, permanently: bool) -> String {
    let noun = if count == 1 { "item" } else { "items" };
    if permanently { format!("Deleted {count} {noun} permanently.") } else { format!("Moved {count} {noun} to Trash.") }
}

impl AppState {
    pub fn receipt(&self, id: RemovalId) -> Option<&Receipt> {
        self.receipts.iter().find(|(rid, _)| *rid == id).map(|(_, receipt)| receipt)
    }

    pub fn latest_receipt(&self, session: SessionId) -> Option<(RemovalId, &Receipt)> {
        self.receipts.iter().rev().find(|(_, r)| r.session == session).map(|(id, r)| (*id, r))
    }

    /// Starts removing the displayed collection. Returns the removal id, or `None` when the
    /// collection is empty, another removal of this session runs, or the map is not the active one.
    pub(crate) fn start_removal(&mut self, permanently: bool) -> Option<RemovalId> {
        if !self.displayed_matches_active() || self.is_removing() || self.is_updating_collection() {
            return None;
        }
        let limit = self.cfg.recovery_parent_limit;
        let id = RemovalId(self.token());
        let session = self.active_session()?;
        if session.collection.is_empty() {
            return None;
        }
        let items = session.collection.items().to_vec();
        let (sid, root, identity) = (session.id, Arc::clone(&session.root), session.identity);
        let text = removing_text(permanently).to_owned();
        if let Some(s) = self.session_mut(sid) {
            s.removing = true;
            s.set_removal_status(Some(text.clone()));
            s.set_removal_error(None);
        }
        self.receipts.push((
            id,
            Receipt { session: sid, running: true, removed_count: None, status: text.clone(), error: None },
        ));
        let sent = items.iter().map(|item| (Arc::clone(&item.path), item.meta.identity)).collect();
        self.removals_running
            .insert(id, super::removal::Running { root: Arc::clone(&root), identity, items: sent, permanently });
        self.clear_drag();
        self.set_error(None);
        self.set_status(Some(text));
        let request = RemovalRequest {
            removal: id,
            session: sid,
            root,
            root_identity: identity,
            items,
            permanently,
            parent_limit: limit,
        };
        self.request(Pending::Removal { removal: id }, Request::Remove(request));
        self.notify(Regions::TRAY | Regions::SHEETS | Regions::HEADER | Regions::INSPECTOR | Regions::FOOTER);
        Some(id)
    }

    fn finish_receipt(&mut self, id: RemovalId, count: usize, permanently: bool, failures: &[String]) {
        if let Some((_, receipt)) = self.receipts.iter_mut().find(|(rid, _)| *rid == id) {
            receipt.running = false;
            receipt.removed_count = Some(count as u64);
            receipt.status = removed_text(count, permanently);
            receipt.error = (!failures.is_empty()).then(|| failures.join("\n"));
        }
        let mut finished = 0;
        let mut keep = vec![true; self.receipts.len()];
        for (index, (_, receipt)) in self.receipts.iter().enumerate().rev() {
            if !receipt.running {
                finished += 1;
                if finished > FINISHED_RECEIPTS {
                    keep[index] = false;
                }
            }
        }
        let mut index = 0;
        self.receipts.retain(|_| {
            index += 1;
            keep[index - 1]
        });
    }

    pub(crate) fn on_removed(&mut self, id: RemovalId, outcome: RemovalOutcome) {
        let Some(run) = self.removals_running.remove(&id) else { return };
        let count = outcome.removed.len();
        self.finish_receipt(id, count, run.permanently, &outcome.failures);
        self.notify(Regions::HEADER | Regions::FOOTER);
        // The owner is found by root and identity: the session object may have been replaced
        // (same-root refresh) while the removal ran.
        let Some(sid) = self.sessions.iter().find(|s| *s.root == *run.root && s.identity == run.identity).map(|s| s.id)
        else {
            return;
        };
        let limit = self.cfg.recovery_parent_limit;
        let revision = self.token();
        let active = self.active == Some(sid);
        let Some(s) = self.session_mut(sid) else { return };
        let current: FxHashMap<&[u8], Option<FileIdentity>> =
            s.collection.items().iter().map(|item| (&*item.path, item.meta.identity)).collect();
        let removed: FxHashSet<&[u8]> = outcome
            .removed
            .iter()
            .map(|path| &**path)
            .filter(|path| current.get(path).copied().flatten().is_some() && current.get(path) == run.items.get(*path))
            .collect();
        let next = s.collection.without(&removed, revision);
        drop(current);
        s.collection = next;
        s.epoch += 1;
        let overlay = outcome.removed.iter().filter_map(|path| {
            run.items.get(path).copied().flatten().map(|identity| ScanRemoval { path: path.to_vec(), identity })
        });
        let merge_overflow = s.merge_removals(overlay);
        let mut combined = s.removal_parents.clone();
        combined.extend(outcome.changed_parents.iter().cloned());
        let exceeded = combined.len() > limit;
        let full = outcome.parents_overflow || exceeded;
        if full {
            s.pending_full_refresh = true;
        } else {
            s.removal_parents = combined;
            let epoch = s.epoch;
            for parent in &outcome.changed_parents {
                s.removal_versions.insert(parent.clone(), epoch);
            }
        }
        if !outcome.changed_parents.is_empty() {
            s.recovery_pending = true;
        }
        if outcome.overlay_overflow || merge_overflow || full {
            s.preview_blocked = true;
        }
        s.removing = false;
        s.set_removal_error((!outcome.failures.is_empty()).then(|| outcome.failures.join("\n")));
        s.set_removal_status(Some(removed_text(count, run.permanently)));
        let (status, error, blocked) = (s.removal_status().cloned(), s.removal_error().cloned(), s.preview_blocked);
        let parents: Vec<Vec<u8>> = s.removal_parents.iter().cloned().collect();
        if active {
            self.finish_removal_presentation(sid, status, error, blocked, full, parents, outcome.root_changed);
        }
        self.pump_collection(sid);
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_removal_presentation(
        &mut self,
        session: SessionId,
        status: Option<String>,
        error: Option<String>,
        blocked: bool,
        full: bool,
        parents: Vec<Vec<u8>>,
        root_changed: bool,
    ) {
        if self.folder.is_some() && !self.displayed_matches_active() {
            return;
        }
        if blocked {
            self.clear_presentation(false);
        }
        if full {
            self.pending_full_refresh = true;
        }
        let empty = self.active_session().is_none_or(|s| s.collection.is_empty());
        if self.show_review && empty {
            self.show_review = false;
            self.confirmation = None;
        }
        self.set_error(error);
        self.set_status(status.clone());
        if root_changed {
            self.set_updates_paused(true);
        } else {
            self.pending_dirs.subtrees.extend(parents);
            self.removal_result_status = status;
            if self.snapshot.is_some() {
                self.set_updates_paused(false);
            }
        }
        if self.filtering() {
            self.search();
        }
        if !root_changed {
            if let (Some(saved), None) = (self.snapshot.clone(), self.monitored) {
                self.begin_monitoring(session, saved);
            }
            self.schedule_refresh();
        }
        if self.partial && self.preview_session.is_some() {
            self.last_preview = None;
            self.deferred.preview = None;
            self.request_preview_read(true);
        } else if let Some(saved) = self.snapshot.clone() {
            self.folder_cache.clear();
            let route = self.requested_path.clone().unwrap_or_else(|| saved.root.to_vec());
            let selection = self.selected.as_ref().map(|item| Arc::clone(&item.path));
            self.load_folder(route, selection);
        }
        if self.gesture.is_none()
            && let Some(completion) = self.deferred.completion.take()
        {
            self.run_completion(completion);
        }
        self.refresh_volumes();
        self.notify(Regions::TRAY | Regions::SHEETS | Regions::MAP | Regions::LIST | Regions::INSPECTOR);
    }
}
