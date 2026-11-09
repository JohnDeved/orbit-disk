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
    h.mapping(b"/r", MappingEvent::Changes { directories, full_refresh: false });
}

/// The paths of the pending refresh request, if one was issued.
fn refresh_paths(h: &Harness) -> Option<Vec<String>> {
    h.requests.iter().find_map(|(_, request)| match request {
        Request::Refresh(refresh) => {
            Some(refresh.paths.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect())
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
