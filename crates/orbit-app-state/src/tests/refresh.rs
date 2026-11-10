//! Prioritized refresh of monitor changes: what the user can see refreshes after the short
//! debounce; changes elsewhere back off quietly and are flushed as soon as they become visible.

use std::time::Duration;

use super::*;
use crate::footer::Phase;
use crate::state::{BACKGROUND_QUIET, REFRESH_DELAY};
use crate::testing::Harness;

const MS: Duration = Duration::from_millis(1);

fn changes(h: &mut Harness, directories: &[&str]) {
    let directories = directories.iter().map(|d| d.as_bytes().to_vec()).collect();
    h.mapping(b"/r", MappingEvent::Changes { directories, subtrees: Vec::new(), full_refresh: false });
}

/// The folders of the pending refresh request (listings and subtrees, sorted), if one was issued.
fn refresh_paths(h: &Harness) -> Option<Vec<String>> {
    h.requests.iter().find_map(|(_, request)| match request {
        Request::Refresh(refresh) => {
            let mut paths: Vec<String> = refresh
                .paths
                .iter()
                .chain(&refresh.subtrees)
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            paths.sort();
            paths.dedup();
            Some(paths)
        }
        _ => None,
    })
}

fn deliver_refresh(h: &mut Harness) {
    let refresh = h.take(|r| matches!(r, Request::Refresh(_))).expect("a refresh request");
    h.deliver(refresh);
}

fn phase(h: &Harness) -> Option<Phase> {
    h.state.footer().map(|f| f.phase)
}

/// `/r` mapped, showing `/r/docs`.
fn showing_docs() -> Harness {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    assert_eq!(current(&h), "/r/docs");
    h
}

#[test]
fn visible_changes_refresh_after_the_short_debounce() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.backend.add_file(b"/r/docs/deep/new.txt", 3);
    changes(&mut h, &["/r/docs/deep"]);
    assert_eq!(phase(&h), Some(Phase::Saved), "a debounced change is not an update in progress");
    h.advance(REFRESH_DELAY - MS);
    assert_eq!(refresh_paths(&h), None);
    h.advance(MS);
    assert_eq!(refresh_paths(&h), Some(vec!["/r/docs/deep".to_owned()]));
    assert_eq!(status(&h).as_deref(), Some("Updating changed folders…"));
    assert_eq!(phase(&h), Some(Phase::Updating));
    let revision = h.state.map_revision();
    deliver_refresh(&mut h);
    h.run();
    assert_eq!(status(&h).as_deref(), Some("Updated changed folders."));
    assert_ne!(h.state.map_revision(), revision, "the shown folder is reloaded");
    assert_eq!(phase(&h), Some(Phase::Saved));
}

#[test]
fn offscreen_changes_back_off_quietly_while_they_keep_arriving() {
    let mut h = showing_docs();
    let shown_status = status(&h);
    let revision = h.state.map_revision();
    let rows = h.rows();
    h.backend.add_file(b"/r/big/c.bin", 1);
    changes(&mut h, &["/r/big"]);
    h.advance(REFRESH_DELAY);
    assert_eq!(refresh_paths(&h), None, "changes outside the shown folder wait longer");
    assert_eq!(phase(&h), Some(Phase::Saved), "deferred changes are not an update in progress");
    h.advance(Duration::from_secs(5) - REFRESH_DELAY);
    assert_eq!(refresh_paths(&h), Some(vec!["/r/big".to_owned()]));
    assert_eq!(phase(&h), Some(Phase::Saved), "a background refresh shows no progress");
    assert_eq!(status(&h), shown_status, "nor a status");
    deliver_refresh(&mut h);
    h.run();
    assert!(!h.has_request(is_saved_view), "the shown folder did not change: it is not reloaded");
    assert_eq!(h.state.map_revision(), revision);
    assert_eq!(h.rows(), rows);
    assert_eq!(status(&h), shown_status);

    // Changes keep arriving: 10 s, then 30 s, and no longer.
    for delay in [10, 30, 30] {
        let wait = Duration::from_secs(delay);
        h.backend.add_file(format!("/r/big/d{delay}.bin").as_bytes(), 1);
        changes(&mut h, &["/r/big"]);
        h.advance(wait / 2);
        changes(&mut h, &["/r/big/more"]);
        h.advance(wait / 2 - MS);
        assert_eq!(refresh_paths(&h), None, "backed off to {delay} s");
        h.advance(MS);
        assert!(refresh_paths(&h).is_some(), "refreshed after {delay} s");
        deliver_refresh(&mut h);
        h.run();
    }
    // A quiet period starts over.
    h.advance(BACKGROUND_QUIET);
    changes(&mut h, &["/r/big"]);
    h.advance(Duration::from_secs(5));
    assert!(refresh_paths(&h).is_some(), "back to 5 s after a quiet period");
}

#[test]
fn offscreen_changes_flush_when_navigating_into_the_affected_folder() {
    let mut h = showing_docs();
    h.backend.add_file(b"/r/big/c.bin", 1_000);
    changes(&mut h, &["/r/big"]);
    // Unaffected folders keep them deferred.
    h.send(Event::Open { path: p("/r/docs/deep") });
    h.run();
    h.send(Event::GoUp);
    h.run();
    h.advance(Duration::ZERO);
    assert_eq!(current(&h), "/r/docs");
    assert_eq!(refresh_paths(&h), None);
    // The root shows `/r/big`: it is refreshed right away.
    h.send(Event::GoUp);
    h.run();
    assert_eq!(refresh_paths(&h), None, "issued by the flush timer");
    h.advance(Duration::ZERO);
    assert_eq!(refresh_paths(&h), Some(vec!["/r/big".to_owned()]));
    assert_eq!(phase(&h), Some(Phase::Updating), "the user is waiting for this one");
    h.settle();
    assert_eq!(current(&h), "/r");
    assert_eq!(h.rows(), ["big", "docs", "file1"]);
    assert_eq!(status(&h).as_deref(), Some("Updated changed folders."));
    h.send(Event::Open { path: p("/r/big") });
    h.run();
    assert_eq!(h.rows(), ["c.bin", "a.bin", "b.bin"]);
}

#[test]
fn offscreen_changes_flush_on_search_and_on_review() {
    let mut h = showing_docs();
    h.backend.add_file(b"/r/big/bigger.bin", 500);
    changes(&mut h, &["/r/big"]);
    h.send(Event::QueryChanged("bin".into()));
    h.advance(Duration::ZERO);
    assert_eq!(refresh_paths(&h), Some(vec!["/r/big".to_owned()]), "search results must see current data");
    h.settle();
    assert!(h.rows().contains(&"bigger.bin".to_owned()), "the search reran on the refreshed map");
    // While searching, every change is visible.
    changes(&mut h, &["/r/big"]);
    h.advance(REFRESH_DELAY);
    assert!(refresh_paths(&h).is_some());
    h.settle();
    h.send(Event::QueryChanged(String::new()));
    h.run();

    h.send(Event::Collect { paths: vec![p("/r/docs/x.txt")] });
    h.run();
    changes(&mut h, &["/r/big"]);
    h.advance(REFRESH_DELAY);
    assert_eq!(refresh_paths(&h), None);
    h.send(Event::BeginReview);
    h.advance(Duration::ZERO);
    assert_eq!(refresh_paths(&h), Some(vec!["/r/big".to_owned()]), "the review must see current data");
}

#[test]
fn a_removal_refreshes_deferred_changes_with_the_removed_items_parents() {
    let mut h = showing_docs();
    h.send(Event::Collect { paths: vec![p("/r/docs/x.txt")] });
    h.run();
    changes(&mut h, &["/r/big"]);
    h.send(Event::BeginReview);
    let review = h.state.review_token().expect("review");
    h.send(Event::ConfirmTrash { review });
    h.advance(Duration::ZERO);
    assert_eq!(refresh_paths(&h), None, "no refresh while the removal runs");
    h.run_one(|r| matches!(r, Request::Remove(_)));
    h.run();
    h.advance(REFRESH_DELAY);
    assert_eq!(refresh_paths(&h), Some(vec!["/r/big".to_owned(), "/r/docs".to_owned()]));
}

#[test]
fn every_pending_change_is_refreshed_together() {
    let mut h = showing_docs();
    changes(&mut h, &["/r/big"]);
    h.advance(Duration::from_secs(1));
    h.backend.add_file(b"/r/docs/z.txt", 1);
    changes(&mut h, &["/r/docs"]);
    h.advance(REFRESH_DELAY - MS);
    assert_eq!(refresh_paths(&h), None, "the visible change is debounced from its arrival");
    h.advance(MS);
    assert_eq!(
        refresh_paths(&h),
        Some(vec!["/r/big".to_owned(), "/r/docs".to_owned()]),
        "deferred changes ride along: none is dropped"
    );
}

#[test]
fn an_unchanged_map_is_not_reloaded() {
    let mut h = harness();
    h.open_mapped(b"/r");
    let revision = h.state.map_revision();
    // The monitor reported a folder whose listing turned out to be the same.
    changes(&mut h, &["/r/docs"]);
    h.advance(REFRESH_DELAY);
    deliver_refresh(&mut h);
    h.run();
    assert!(!h.has_request(is_saved_view));
    assert_eq!(h.state.map_revision(), revision);
    assert_eq!(status(&h).as_deref(), Some("Updated changed folders."));
}

#[test]
fn listings_and_subtrees_reach_the_refresh_separately() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.backend.add_file(b"/r/docs/new.txt", 3);
    let listing = |d: &str| MappingEvent::Changes {
        directories: vec![d.as_bytes().to_vec()],
        subtrees: Vec::new(),
        full_refresh: false,
    };
    h.mapping(b"/r", listing("/r/docs"));
    h.mapping(b"/r", listing("/r"));
    h.mapping(
        b"/r",
        MappingEvent::Changes { directories: Vec::new(), subtrees: vec![b"/r/big".to_vec()], full_refresh: false },
    );
    h.advance(REFRESH_DELAY);
    let refresh = h.take(|r| matches!(r, Request::Refresh(_))).expect("a refresh request");
    let Request::Refresh(request) = &refresh.1 else { unreachable!() };
    // A listing of `/r` is not a recursive reconciliation of `/r`: neither absorbs the other.
    assert_eq!(request.paths, [b"/r".to_vec(), b"/r/docs".to_vec()]);
    assert_eq!(request.subtrees, [b"/r/big".to_vec()]);
}

/// Re-listing a folder above the shown one (a write to `~/.zsh_history` lists the home folder)
/// does not change what is shown: it waits like an off-screen change, then refreshes quietly.
#[test]
fn a_listing_above_the_shown_folder_is_a_background_change() {
    let mut h = showing_docs();
    changes(&mut h, &["/r"]);
    h.advance(REFRESH_DELAY);
    assert_eq!(refresh_paths(&h), None, "a listing of /r is not visible from /r/docs");
    // A subtree at the shown folder is visible (a root subtree would start a rescan instead).
    h.mapping(
        b"/r",
        MappingEvent::Changes { directories: Vec::new(), subtrees: vec![b"/r/docs".to_vec()], full_refresh: false },
    );
    h.advance(REFRESH_DELAY);
    assert_eq!(
        refresh_paths(&h),
        Some(vec!["/r".to_owned(), "/r/docs".to_owned()]),
        "a subtree of /r/docs includes what is shown"
    );
}

/// Refresh on a live map flushes the pending changes and retries unreadable folders: no
/// mapping starts.
#[test]
fn refresh_of_a_live_map_updates_changes_without_rescanning() {
    let mut h = harness();
    h.open_mapped(b"/r");
    let mappings = h.mappings.len();
    h.send(Event::Refresh);
    h.advance(Duration::ZERO);
    let request = h.requests.iter().find_map(|(_, r)| match r {
        Request::Refresh(refresh) => Some(refresh.clone()),
        _ => None,
    });
    assert!(request.expect("a refresh request").retry_unreadable);
    assert_eq!(h.mappings.len(), mappings, "no mapping started");
}

/// After a pause, Refresh reopens the saved map (no forced rescan); its monitoring replays the
/// history it missed.
#[test]
fn refresh_after_a_pause_reopens_the_saved_map() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::Pause);
    h.run();
    h.send(Event::Refresh);
    h.run();
    let last = h.mappings.last().expect("a mapping");
    assert!(!last.force, "a paused map is reopened, not rescanned");
    assert!(!h.state.updates_paused());
}

/// A refresh whose reply arrives after the user moved to another location was still published:
/// the session keeps the new saved map, so returning to it (and its next update) uses that map
/// rather than the one it replaced, which the backend reported as "the saved map changed".
#[test]
fn a_refresh_published_after_moving_on_is_kept() {
    let mut h = harness();
    h.backend.add_file(b"/s/other.txt", 5);
    h.open_mapped(b"/r");
    let before = h.state.snapshot().unwrap().id;
    let session = h.state.active_session_id().unwrap();
    h.backend.add_file(b"/r/docs/new.txt", 3);
    changes(&mut h, &["/r/docs"]);
    h.advance(REFRESH_DELAY);
    let refresh = h.take(|r| matches!(r, Request::Refresh(_))).expect("a refresh request");
    h.open_mapped(b"/s");
    h.deliver(refresh);
    h.run();
    let saved = h.state.session(session).and_then(|s| s.saved.as_ref()).map(|s| s.id);
    assert!(saved.is_some_and(|id| id != before), "the session keeps the published map: {saved:?} vs {before}");
    h.send(Event::OpenLocation { path: b"/r".to_vec(), force: false });
    h.run();
    assert_eq!(h.state.snapshot().map(|s| s.id), saved, "returning shows the published map");
}

/// Without change history Refresh has nothing to replay: it rescans. Rescan Everything always
/// does.
#[test]
fn refresh_without_history_and_rescan_everything_read_everything() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.mapping(b"/r", MappingEvent::MonitorUnavailable);
    h.send(Event::Refresh);
    h.run();
    let last = h.mappings.last().expect("a mapping");
    assert!(last.force && last.keep_map, "no history: the map is read again, kept on screen");

    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::RescanEverything);
    h.run();
    let last = h.mappings.last().expect("a mapping");
    assert!(last.force && last.keep_map);
}

/// A change history that asks to reconcile the whole root starts the (fast, parallel) rescan.
#[test]
fn a_root_subtree_change_rescans_the_map() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.mapping(
        b"/r",
        MappingEvent::Changes { directories: Vec::new(), subtrees: vec![b"/r".to_vec()], full_refresh: false },
    );
    h.advance(REFRESH_DELAY);
    h.run();
    let last = h.mappings.last().expect("a mapping");
    assert!(last.force && last.keep_map, "the root is rescanned, keeping the map on screen");
}
