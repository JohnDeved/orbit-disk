//! Removal: locking, owner-session merges, partial failures, root changes, overflow, refresh.

use std::time::Duration;

use super::*;
use crate::state::Config;

fn is_remove(request: &Request) -> bool {
    matches!(request, Request::Remove(_))
}

fn review_and_confirm(h: &mut Harness, permanently: bool) {
    h.send(Event::BeginReview);
    let review = h.state.review_token().expect("review");
    if permanently {
        h.send(Event::ConfirmPermanentDelete { review });
    } else {
        h.send(Event::ConfirmTrash { review });
    }
}

fn collect(h: &mut Harness, paths: &[&str]) {
    h.send(Event::Collect { paths: paths.iter().map(|path| p(path)).collect() });
    h.run();
}

#[test]
fn removal_locks_the_collection_and_merges_into_its_owner_after_a_switch() {
    let mut h = harness();
    h.open_mapped(b"/s");
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/docs/x.txt", "/r/docs/y.txt"]);
    review_and_confirm(&mut h, true);
    assert!(h.state.removal_active() && h.state.is_removing());
    assert_eq!(status(&h).as_deref(), Some("Deleting collected items permanently…"));
    let removal = h.take(is_remove).unwrap();
    // Collection editing is locked while removal runs; additions queue without validating.
    h.send(Event::Uncollect { path: p("/r/docs/x.txt"), review: None });
    assert_eq!(h.collection().len(), 2);
    h.send(Event::Collect { paths: vec![p("/r/file1")] });
    assert!(!h.has_request(|r| matches!(r, Request::Validate(_))));
    // Switch away; the removal stays owned by /r.
    h.open(b"/s");
    assert!(h.state.removal_active() && !h.state.is_removing());
    h.deliver(removal);
    h.run();
    assert!(!h.state.removal_active());
    assert_eq!(status(&h), None, "/s feedback is untouched");
    assert!(h.backend.identity_of(b"/r/docs/x.txt").is_none());
    // Back in /r: removal outcome, the queued addition, and the refreshed folder.
    h.open(b"/r");
    assert_eq!(h.collection(), ["/r/file1"]);
    assert!(status(&h).is_some_and(|s| s.starts_with("Deleted 2 items permanently.")));
    h.settle();
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    assert_eq!(h.rows(), ["deep"]);
    assert_eq!(status(&h).as_deref(), Some("Deleted 2 items permanently."));
}

#[test]
fn partial_failures_keep_survivors_reviewable_and_refresh_their_parents() {
    let mut h = harness();
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/docs/x.txt", "/r/file1"]);
    h.backend.replace(b"/r/file1");
    review_and_confirm(&mut h, false);
    h.run();
    assert_eq!(h.collection(), ["/r/file1"], "the refused item stays reviewable");
    assert_eq!(status(&h).as_deref(), Some("Moved 1 item to Trash."));
    assert_eq!(error(&h).as_deref(), Some("file1: the item changed. Remove it from the collection and add it again."));
    assert!(h.backend.identity_of(b"/r/file1").is_some(), "the replacement is untouched");
    // The removed row is hidden right away; the refresh of changed parents follows.
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    assert_eq!(h.rows(), ["y.txt", "deep"]);
    h.advance(Duration::from_millis(1_500));
    assert!(h.has_request(|r| matches!(r, Request::Refresh(req) if req.paths.contains(&b"/r/docs".to_vec()))));
    h.settle();
    assert_eq!(status(&h).as_deref(), Some("Moved 1 item to Trash."), "the outcome survives the map update");
    assert!(error(&h).is_some());
    assert_eq!(h.rows(), ["y.txt", "deep"]);
}

#[test]
fn a_replaced_root_stops_removal_and_pauses_updates() {
    let mut h = harness();
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/docs/x.txt"]);
    let identity = h.backend.identity_of(b"/r").unwrap();
    h.backend.replace(b"/r");
    h.backend.add_file(b"/r/docs/x.txt", 30);
    assert_ne!(h.backend.identity_of(b"/r"), Some(identity));
    review_and_confirm(&mut h, false);
    h.run();
    assert!(h.backend.identity_of(b"/r/docs/x.txt").is_some());
    assert_eq!(
        error(&h).as_deref(),
        Some("The mapped folder changed. Refresh the map before removing any more items.")
    );
    assert!(h.state.updates_paused());
}

#[test]
fn too_many_changed_parents_fall_back_to_a_full_refresh() {
    let mut config = Config::new(HOME);
    config.recovery_parent_limit = 1;
    let mut h = Harness::new(config);
    h.backend.add_file(b"/r/a/one", 1);
    h.backend.add_file(b"/r/b/two", 1);
    h.backend.add_file(b"/r/c/three", 1);
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/a/one", "/r/b/two"]);
    let starts = h.mappings.len();
    review_and_confirm(&mut h, false);
    h.run();
    assert!(h.state.folder().is_none(), "the overlay cannot hold the change: the map is withheld");
    h.advance(Duration::from_millis(1_500));
    h.run();
    assert_eq!(h.mappings.len(), starts + 1);
    assert!(h.mappings.last().unwrap().force);
}

#[test]
fn removals_in_two_sessions_complete_independently() {
    let mut h = harness();
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/file1"]);
    review_and_confirm(&mut h, false);
    let first = h.take(is_remove).unwrap();
    h.open_mapped(b"/s");
    collect(&mut h, &["/s/one"]);
    review_and_confirm(&mut h, false);
    let second = h.take(is_remove).unwrap();
    assert!(h.state.is_removing());
    h.deliver(first);
    assert!(h.state.removal_active(), "the /s removal still runs");
    assert!(h.state.is_removing());
    h.deliver(second);
    assert!(!h.state.removal_active());
    assert_eq!(status(&h).as_deref(), Some("Moved 1 item to Trash."));
    h.run();
    h.open(b"/r");
    assert!(h.collection().is_empty());
    assert!(status(&h).is_some_and(|s| s.starts_with("Moved 1 item to Trash.")));
}

#[test]
fn confirmed_removal_deletes_only_the_reviewed_items() {
    let mut h = harness();
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/docs/x.txt"]);
    h.send(Event::BeginReview);
    let review = h.state.review_token().unwrap();
    h.send(Event::BeginConfirmation { review: review.clone() });
    // An addition accepted during the confirmation is held, not slipped into the removal.
    h.send(Event::Collect { paths: vec![p("/r/file1")] });
    h.run();
    h.send(Event::ConfirmPermanentDelete { review });
    let lease = h.state.confirmation_lease().unwrap();
    h.send(Event::EndConfirmation { lease });
    let removal = h.take(is_remove).unwrap();
    let Request::Remove(request) = &removal.1 else { unreachable!() };
    assert_eq!(request.items.len(), 1);
    assert_eq!(&*request.items[0].path, b"/r/docs/x.txt");
    h.deliver(removal);
    h.run();
    assert!(h.backend.identity_of(b"/r/file1").is_some());
    assert_eq!(h.collection(), ["/r/file1"], "the held addition applies after the removal");
}

#[test]
fn publication_during_a_held_removal_keeps_the_removal_status() {
    let mut h = harness();
    h.open(b"/r");
    collect(&mut h, &["/r/file1"]);
    review_and_confirm(&mut h, false);
    let removal = h.take(is_remove).unwrap();
    h.publish(b"/r");
    h.run();
    assert_eq!(status(&h).as_deref(), Some("Moving collected items to Trash…"));
    assert_eq!(h.state.footer().map(|f| f.phase), Some(crate::footer::Phase::Updating));
    h.deliver(removal);
    h.settle();
    assert_eq!(status(&h).as_deref(), Some("Moved 1 item to Trash."));
    assert_eq!(h.state.footer().map(|f| f.phase), Some(crate::footer::Phase::Saved));
}
