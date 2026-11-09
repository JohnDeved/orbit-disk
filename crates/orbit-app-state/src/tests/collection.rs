//! Collection: drags, eligibility, the validation queue and its races, review tokens and leases.

use super::*;
use crate::collection::CollectionSnapshot;
use crate::coverage::CollectionState;
use crate::ids::LeaseId;
use crate::model::Item;
use orbit_core::view::NodeMeta;

fn is_validate(request: &Request) -> bool {
    matches!(request, Request::Validate(_))
}

fn collect(h: &mut Harness, paths: &[&str]) {
    h.send(Event::Collect { paths: paths.iter().map(|path| p(path)).collect() });
}

#[test]
fn drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes() {
    let mut h = harness();
    h.open_mapped(b"/r");
    let first = drag_items(&mut h, 1, &["/r/docs/x.txt"]);
    assert!(h.state.can_accept_drop());
    // Starting, hovering and ending other drags never collects.
    h.send(Event::DragOverTray { drag: first, over: true });
    assert!(h.state.drop_target_active());
    h.send(Event::DragEnd { drag: DragId(42) });
    assert_eq!(h.state.drag_id(), Some(first));
    let second = drag_items(&mut h, 2, &["/r/file1"]);
    // The first payload is invalid once a new drag started.
    h.send(Event::DragDrop { drag: first });
    h.run();
    assert!(h.collection().is_empty());
    h.send(Event::DragDrop { drag: second });
    h.run();
    assert_eq!(h.collection(), ["/r/file1"]);
    // Consumed: a second drop of the same payload does nothing.
    h.send(Event::DragDrop { drag: second });
    h.run();
    assert_eq!(h.collection(), ["/r/file1"]);
    // A root change invalidates a pending drag; it never crosses into another root.
    let third = drag_items(&mut h, 3, &["/r/docs/y.txt"]);
    h.open_mapped(b"/s");
    assert_eq!(h.state.drag_id(), None);
    h.send(Event::DragDrop { drag: third });
    h.run();
    assert!(h.collection().is_empty());
    h.open(b"/r");
    assert_eq!(h.collection(), ["/r/file1"]);
}

#[test]
fn drops_collapse_descendants_report_included_and_reject_ineligible_items() {
    let mut h = harness();
    h.backend.protect(b"/r/big");
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/docs/x.txt"]);
    h.run();
    assert_eq!(status(&h).as_deref(), Some("Added x.txt to collection."));
    let drag = drag_items(&mut h, 1, &["/r/docs", "/r/docs/x.txt", "/r/file1"]);
    h.send(Event::DragDrop { drag });
    assert!(h.state.is_validating_collection());
    assert_eq!(status(&h).as_deref(), Some("Checking items for collection…"));
    h.run();
    assert_eq!(h.collection(), ["/r/docs", "/r/file1"], "the folder replaces its collected descendant");
    assert_eq!(status(&h).as_deref(), Some("Added 2 items to collection. 1 item already included."));
    assert_eq!(h.state.collection_state(b"/r/docs/y.txt"), CollectionState::Included);
    assert_eq!(h.state.collection_state(b"/r"), CollectionState::Partial);
    // Policy rejections are reported, not collected.
    let drag = drag_items(&mut h, 2, &["/r/big/a.bin"]);
    h.send(Event::DragDrop { drag });
    h.run();
    assert_eq!(status(&h).as_deref(), Some("Skipped 1 item."));
    assert_eq!(error(&h).as_deref(), Some("a.bin: System and Library folders are protected."));
    // Already included or never collectable: nothing starts.
    h.send(Event::DismissError);
    drag_items(&mut h, 3, &["/r/docs/y.txt"]);
    assert_eq!(h.state.drag_id(), None);
    assert_eq!(status(&h).as_deref(), Some("These items are already included in the collection."));
    drag_items(&mut h, 4, &["/r"]);
    assert_eq!(status(&h).as_deref(), Some("These items cannot be collected. Refresh the map if they have changed."));
    assert!(!h.state.can_collect(&h.state.lookup_item(b"/r").unwrap()));
}

#[test]
fn replaced_items_are_rejected_instead_of_collected_under_their_old_identity() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.backend.replace(b"/r/docs/y.txt");
    h.backend.replace(b"/r/big");
    collect(&mut h, &["/r/docs/y.txt", "/r/big"]);
    h.run();
    assert!(h.collection().is_empty());
    assert_eq!(status(&h).as_deref(), Some("Skipped 2 items."));
    let error = error(&h).unwrap();
    assert!(error.contains("y.txt: changed since indexing. Refresh the map before collecting it."));
    assert!(error.contains("big: changed since indexing."));
}

#[test]
fn collection_changes_during_validation_retry_against_the_new_revision() {
    let mut h = harness();
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/docs/y.txt"]);
    h.run();
    collect(&mut h, &["/r/docs/x.txt"]);
    let held = h.take(is_validate).unwrap();
    // A direct edit lands while the addition validates.
    h.send(Event::Uncollect { path: p("/r/docs/y.txt"), review: None });
    h.deliver(held);
    assert!(h.has_request(is_validate), "the stale validation is retried");
    h.run();
    assert_eq!(h.collection(), ["/r/docs/x.txt"], "both the edit and the addition apply");
    assert_eq!(h.state.collection().unwrap().size(), 30);
}

#[test]
fn large_drops_buffer_slices_recheck_before_commit_and_small_requests_overtake() {
    let mut h = harness();
    for i in 0..300 {
        h.backend.add_file(format!("/r/many/f{i:04}").as_bytes(), 1_000 - i as u64);
    }
    h.backend.add_file(b"/r/many/zz", 1);
    h.open_mapped(b"/r");
    h.send(Event::Open { path: p("/r/many") });
    h.run();
    let paths: Vec<String> = (0..300).map(|i| format!("/r/many/f{i:04}")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let drag = drag_items(&mut h, 1, &refs);
    h.send(Event::DragDrop { drag });
    // First slice commits alone (target 128).
    h.run_one(is_validate);
    assert_eq!(h.state.collection().unwrap().len(), 128);
    // Second slice is buffered (target 256); meanwhile a small request arrives.
    h.run_one(is_validate);
    assert_eq!(h.state.collection().unwrap().len(), 128);
    collect(&mut h, &["/r/many/zz"]);
    // A buffered item vanishes before the commit.
    h.backend.delete(b"/r/many/f0130");
    h.run();
    let collection = h.collection();
    assert_eq!(collection.len(), 300, "299 drop items and zz");
    assert_eq!(collection[128], "/r/many/zz", "the small request overtook the remainder");
    assert!(!collection.contains(&"/r/many/f0130".to_owned()), "a deleted buffered item never reappears");
    assert_eq!(status(&h).as_deref(), Some("Collection updated. Skipped 1 item."));
    assert!(error(&h).unwrap().starts_with("f0130: changed since indexing."));
}

#[test]
fn validation_belongs_to_its_root_across_switches() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.open_mapped(b"/s");
    h.open(b"/r");
    collect(&mut h, &["/r/docs/x.txt"]);
    h.backend.replace(b"/r/file1");
    collect(&mut h, &["/r/file1"]);
    let held = h.take(is_validate).unwrap();
    h.open(b"/s");
    assert!(!h.state.is_validating_collection(), "the tray of /s shows no foreign validation");
    // /r's job completes while /s is shown: nothing is published into /s.
    h.deliver(held);
    h.run();
    assert!(h.collection().is_empty());
    assert_eq!(status(&h), None);
    assert_eq!(error(&h), None);
    // Returning shows the outcome and the queued rejection.
    h.open(b"/r");
    assert_eq!(h.collection(), ["/r/docs/x.txt"]);
    assert!(error(&h).is_some_and(|e| e.starts_with("file1: changed since indexing.")));
}

#[test]
fn completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.backend.replace(b"/r/file1");
    collect(&mut h, &["/r/docs/x.txt", "/r/file1"]);
    let held = h.take(is_validate).unwrap();
    h.send(Event::Pause);
    let paused = status(&h);
    assert_eq!(paused.as_deref(), Some("Updates paused. Refresh to resume."));
    h.open(b"/nope");
    let newer = error(&h);
    assert!(newer.is_some());
    h.deliver(held);
    h.run();
    assert_eq!(h.collection(), ["/r/docs/x.txt"], "pausing keeps accepted collection work");
    assert_eq!(status(&h), paused, "a newer status is not overwritten");
    assert_eq!(error(&h), newer, "a newer error is not overwritten");
    h.send(Event::DismissError);
    assert!(error(&h).is_some_and(|e| e.starts_with("file1: changed since indexing.")), "the rejection follows");
}

#[test]
fn review_tokens_reject_stale_actions_and_confirmation_holds_publication() {
    let mut h = harness();
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/docs/x.txt", "/r/docs/y.txt"]);
    h.run();
    h.send(Event::BeginReview);
    let first = h.state.review_token().unwrap();
    h.send(Event::Uncollect { path: p("/r/docs/y.txt"), review: Some(first.clone()) });
    assert_eq!(h.collection(), ["/r/docs/x.txt"]);
    // The collection changed since `first`: removal with it is a no-op.
    h.send(Event::ConfirmTrash { review: first.clone() });
    assert!(!h.state.removal_active());
    let second = h.state.review_token().unwrap();
    h.send(Event::DismissReview { review: second.clone() });
    h.send(Event::BeginReview);
    let third = h.state.review_token().unwrap();
    assert_ne!(second, third, "a reopened review is a distinct instance");
    // A row action from the dismissed review leaves the reopened one alone.
    h.send(Event::Uncollect { path: p("/r/docs/x.txt"), review: Some(second) });
    assert_eq!(h.collection(), ["/r/docs/x.txt"]);
    // An open confirmation holds collection changes until its lease ends.
    h.send(Event::BeginConfirmation { review: third.clone() });
    let lease = h.state.confirmation_lease().unwrap();
    collect(&mut h, &["/r/file1"]);
    h.run();
    assert_eq!(h.collection(), ["/r/docs/x.txt"]);
    h.send(Event::EndConfirmation { lease: LeaseId(lease.0 + 100) });
    h.run();
    assert_eq!(h.collection(), ["/r/docs/x.txt"], "a stale lease releases nothing");
    h.send(Event::EndConfirmation { lease });
    h.run();
    assert_eq!(h.collection(), ["/r/docs/x.txt", "/r/file1"]);
    // Clearing during a confirmation empties the collection and closes the review.
    let token = h.state.review_token().unwrap();
    h.send(Event::BeginConfirmation { review: token.clone() });
    h.send(Event::ClearCollection { review: token });
    assert!(h.collection().is_empty());
    assert!(!h.state.show_review());
    assert_eq!(h.state.confirmation_lease(), None);
}

#[test]
fn clearing_cancels_queued_additions() {
    let mut h = harness();
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/file1"]);
    h.run();
    collect(&mut h, &["/r/docs/x.txt"]);
    let (id, _) = h.requests.iter().find(|(_, r)| is_validate(r)).cloned().unwrap();
    h.send(Event::BeginReview);
    let token = h.state.review_token().unwrap();
    h.send(Event::ClearCollection { review: token });
    assert!(h.cancelled.contains(&id));
    h.run();
    assert!(h.collection().is_empty(), "delayed additions do not refill the cleared collection");
    assert!(!h.state.is_validating_collection());
}

#[test]
fn nested_items_mark_ancestors_and_totals_saturate() {
    let mut h = harness();
    h.open_mapped(b"/r");
    collect(&mut h, &["/r/docs/deep/z.txt"]);
    h.run();
    assert_eq!(h.state.collection_state(b"/r/docs/deep/z.txt"), CollectionState::Included);
    assert_eq!(h.state.collection_state(b"/r/docs"), CollectionState::Partial);
    let presentation = h.state.presentation().unwrap();
    let folder = h.state.folder().unwrap().clone();
    let docs = presentation
        .layout
        .segments()
        .iter()
        .position(|s| s.node.is_some_and(|ix| folder.path(ix) == b"/r/docs"))
        .unwrap();
    assert_eq!(presentation.state(docs), CollectionState::Partial);
    let huge = |path: &str| Item::new(path.as_bytes().to_vec(), NodeMeta { size: u64::MAX - 1, ..NodeMeta::default() });
    let total = CollectionSnapshot::new(vec![huge("/a"), huge("/b")], 1);
    assert_eq!(total.size(), u64::MAX);
    assert_eq!(total.without(&std::iter::once(&b"/a"[..]).collect(), 2).size(), u64::MAX - 1);
}
