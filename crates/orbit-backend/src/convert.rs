//! Conversions between orbit-core results and the app state's types, with the user-facing
//! messages of the Swift app.

use std::sync::Arc;

use orbit_app_state::Item;
use orbit_app_state::backend::{RemovalOutcome, SearchReply};
use orbit_app_state::model::{EstimateBasis, ProgressSnapshot};
use orbit_core::ScanRemoval;
use orbit_core::journal::{FailureReason, RemovalFailure, RemovalOutcome as CoreOutcome};
use orbit_core::paths::file_name;
use orbit_core::progress::{EstimateBasis as CoreBasis, MappingProgress};
use orbit_core::query::SearchResults;
use orbit_core::schema::SnapshotInfo as CoreSnapshot;
use orbit_core::{CoreError, mailbox::DiscoveryProgress};

pub(crate) const MAP_UNAVAILABLE: &str = "The saved map is unavailable.";
pub(crate) const FOLDER_UNAVAILABLE: &str = "The indexed folder is unavailable.";
pub(crate) const MAP_UPDATING: &str = "The map is being updated.";
pub(crate) const ROOT_CHANGED: &str = "The mapped folder changed. Refresh the map before removing any more items.";

pub(crate) fn message(error: &CoreError) -> String {
    error.to_string()
}

/// Swift `needsBootTraversalPreview`: a saved map of `/` made by a traversal that did not yet
/// skip the boot volume's aliases must be re-mapped with a visible preview.
pub(crate) fn needs_boot_upgrade(snapshot: &CoreSnapshot) -> bool {
    if snapshot.root_path != b"/" {
        return false;
    }
    let Ok(serde_json::Value::Object(identity)) = serde_json::from_str(&snapshot.event_identity) else {
        return false;
    };
    let text = |key: &str| identity.get(key).and_then(serde_json::Value::as_str);
    text("rootPath") == Some("/")
        && text("mode") == Some("host")
        && identity.get("bootTraversal").is_none_or(|v| v.as_i64() == Some(1))
}

pub(crate) fn progress(progress: &MappingProgress, work: &DiscoveryProgress, root: &[u8]) -> ProgressSnapshot {
    ProgressSnapshot {
        revision: progress.revision,
        item_count: work.records,
        bytes_scanned: work.logical_size,
        current_path: root.to_vec(),
        discovery_complete: progress.discovery_complete,
        discovery_fraction: progress.discovery_fraction,
        estimate_basis: match progress.estimate_basis {
            CoreBasis::Estimating | CoreBasis::CurrentScan => EstimateBasis::ThisScan,
            CoreBasis::PreviousMap => EstimateBasis::PreviousMap,
            CoreBasis::Revised => EstimateBasis::Revised,
        },
        cache_fraction: progress.cache_fraction,
        cache_finishing: progress.cache_finishing,
        cached_file_count: progress.cached_file_count,
    }
}

pub(crate) fn search_reply(results: SearchResults, preview_revision: u64) -> SearchReply {
    SearchReply {
        total: results.total as u64,
        hits: results.hits.into_iter().map(|hit| Item::new(hit.path, hit.meta)).collect(),
        preview_revision,
        removals_changed: false,
    }
}

fn display_name(path: &[u8]) -> String {
    let name = file_name(path);
    String::from_utf8_lossy(if name.is_empty() { path } else { name }).into_owned()
}

/// Swift `removeCollection` failure lines.
fn failure_line(failure: &RemovalFailure) -> String {
    let name = failure.path.as_deref().map(display_name);
    let named = |text: String| match &name {
        Some(name) => format!("{name}: {text}"),
        None => text,
    };
    match &failure.reason {
        FailureReason::NotRecorded(error) => format!("Removal could not be recorded: {error}"),
        FailureReason::RootChanged => ROOT_CHANGED.to_owned(),
        FailureReason::ItemChanged => named("the item changed. Remove it from the collection and add it again.".into()),
        FailureReason::ParentChanged => {
            named("the containing folder changed. Refresh the map before removing it.".into())
        }
        FailureReason::Policy(reason) => named(reason.clone()),
        FailureReason::Failed(error) => named(error.to_string()),
        FailureReason::Tracking(error) => format!("Removal tracking could not be completed: {error}"),
    }
}

pub(crate) fn removal_outcome(outcome: CoreOutcome, parent_limit: usize, mut failures: Vec<String>) -> RemovalOutcome {
    failures.extend(outcome.failures.iter().map(failure_line));
    let parents_overflow = outcome.parents_overflow || outcome.changed_parents.len() > parent_limit;
    RemovalOutcome {
        removed: outcome.removed.into_iter().map(Arc::from).collect(),
        failures,
        changed_parents: if parents_overflow { Vec::new() } else { outcome.changed_parents.into_iter().collect() },
        parents_overflow,
        root_changed: outcome.root_changed,
        overlay_overflow: outcome.overlay_overflow,
    }
}

/// Items without a mapped identity cannot be removed safely.
pub(crate) fn removal_items(items: &[Item]) -> (Vec<ScanRemoval>, Vec<String>) {
    let mut removals = Vec::with_capacity(items.len());
    let mut failures = Vec::new();
    for item in items {
        match item.meta.identity {
            Some(identity) => removals.push(ScanRemoval { path: item.path.to_vec(), identity }),
            None => failures.push(format!(
                "{}: the item changed. Remove it from the collection and add it again.",
                item.display_name()
            )),
        }
    }
    (removals, failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(root: &[u8], identity: &str) -> CoreSnapshot {
        CoreSnapshot {
            id: "s".into(),
            root_path: root.to_vec(),
            database: "/tmp/x.sqlite".into(),
            scanned_at: 0.0,
            elapsed: 0.0,
            skipped: 0,
            issues: Vec::new(),
            event_id: 1,
            event_identity: identity.into(),
        }
    }

    /// Only host-mode maps of `/` from the first boot traversal are upgraded (Swift
    /// `needsBootTraversalPreview`).
    #[test]
    fn boot_traversal_upgrade_needs_a_host_map_of_the_boot_root() {
        let old = r#"{"canonicalRoot":"/","mode":"host","rootFileID":2,"rootPath":"/","version":1,"volumes":[]}"#;
        let first = r#"{"bootTraversal":1,"mode":"host","rootPath":"/"}"#;
        let current = r#"{"bootTraversal":2,"mode":"host","rootPath":"/"}"#;
        let device = r#"{"mode":"device","rootPath":"/"}"#;
        assert!(needs_boot_upgrade(&snapshot(b"/", old)));
        assert!(needs_boot_upgrade(&snapshot(b"/", first)));
        assert!(!needs_boot_upgrade(&snapshot(b"/", current)));
        assert!(!needs_boot_upgrade(&snapshot(b"/", device)));
        assert!(!needs_boot_upgrade(&snapshot(b"/Users", old)));
        assert!(!needs_boot_upgrade(&snapshot(b"/", "not json")));
    }
}
