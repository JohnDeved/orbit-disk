//! Search debounce, keep-previous-results, cancellation, preview re-runs and stale results.

use std::time::Duration;

use super::*;
use crate::backend::Reply;
use crate::event::{Effect, Regions};

fn is_search(request: &Request) -> bool {
    matches!(request, Request::Search(_))
}

#[test]
fn search_debounces_keeps_previous_rows_and_drops_superseded_results() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::QueryChanged("x".into()));
    assert!(h.state.is_searching());
    assert!(!h.has_request(is_search), "debounced");
    // Typing again restarts the debounce; the first timer is ignored when it fires.
    h.send(Event::QueryChanged("x.".into()));
    h.advance(SHORT);
    assert_eq!(h.requests.iter().filter(|(_, r)| is_search(r)).count(), 1);
    h.run();
    assert_eq!(h.rows(), ["x.txt"]);
    assert_eq!(h.state.search_match_count(), 1);
    h.send(Event::Select { path: Some(p("/r/docs/x.txt")) });
    // A new query keeps the previous rows until its results land, but clears the selection.
    h.send(Event::QueryChanged("y".into()));
    assert_eq!(h.rows(), ["x.txt"]);
    assert_eq!(selected(&h), None);
    assert!(h.state.search_outdated());
    h.advance(SHORT);
    let superseded = h.take(is_search).unwrap();
    h.send(Event::QueryChanged("txt".into()));
    let effects = h.state.handle(Event::QueryChanged("txt!".into()), h.now);
    assert!(effects.iter().all(|e| !matches!(e, Effect::Cancel(id) if *id == superseded.0)), "already taken");
    h.absorb(effects);
    h.send(Event::QueryChanged("txt".into()));
    // The superseded reply arrives late and is dropped.
    h.deliver(superseded);
    assert_eq!(h.rows(), ["x.txt"]);
    h.advance(SHORT);
    h.run();
    assert_eq!(h.rows(), ["x.txt", "y.txt", "z.txt"]);
    assert!(!h.state.search_outdated());
    // An in-flight search is cancelled when the query changes again.
    h.send(Event::QueryChanged("z".into()));
    h.advance(SHORT);
    let (id, _) = h.requests.iter().find(|(_, r)| is_search(r)).cloned().unwrap();
    h.send(Event::QueryChanged("q".into()));
    assert!(h.cancelled.contains(&id));
    // Clearing the query restores the folder rows.
    h.send(Event::QueryChanged(String::new()));
    assert_eq!(h.rows(), ["big", "docs", "file1"]);
}

#[test]
fn preview_search_reruns_after_preview_changes_then_uses_the_saved_index() {
    let mut h = harness();
    h.open(b"/r");
    h.send(Event::QueryChanged("txt".into()));
    // Not ready yet: the longer debounce applies.
    h.advance(SHORT);
    assert!(!h.has_request(is_search));
    h.advance(Duration::from_millis(84));
    let request = h.take(is_search).unwrap();
    let reply = h.perform_held(request).unwrap();
    // The preview changes while the search runs.
    h.backend.add_file(b"/r/docs/w.txt", 25);
    h.preview(b"/r");
    h.run();
    h.send(reply);
    assert_eq!(h.rows(), ["x.txt", "y.txt", "z.txt"], "results land, the newer preview is searched again");
    h.advance(Duration::from_millis(1_499));
    assert!(!h.has_request(is_search));
    h.advance(Duration::from_millis(1));
    h.advance(SHORT);
    h.run();
    assert_eq!(h.rows(), ["x.txt", "w.txt", "y.txt", "z.txt"]);
    // After publication the saved index answers, with the same query kept.
    h.publish(b"/r");
    h.settle();
    assert!(!h.state.is_partial());
    assert_eq!(h.state.query(), "txt");
    assert_eq!(h.state.row_count(), 4);
}

#[test]
fn failed_search_stays_outdated_until_retried() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::QueryChanged("x.".into()));
    h.advance(SHORT);
    let (id, _) = h.take(is_search).unwrap();
    h.send(Event::Backend { id, reply: Reply::Search(Err("index busy".into())) });
    assert!(h.state.search_failed());
    assert!(h.state.search_outdated());
    assert!(!h.state.is_searching());
    h.send(Event::RetrySearch);
    h.advance(SHORT);
    h.run();
    assert!(!h.state.search_failed());
    assert_eq!(h.rows(), ["x.txt"]);
}

#[test]
fn search_results_of_a_previous_root_are_dropped() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::QueryChanged("o".into()));
    h.advance(SHORT);
    let held = h.take(is_search).unwrap();
    let late = h.perform_held(held).unwrap();
    h.open_mapped(b"/s");
    h.send(late);
    assert!(h.rows().iter().all(|r| r == "one" || r == "two"), "only /s matches: {:?}", h.rows());
}

#[test]
fn pausing_completes_a_preview_search_without_restarting_mapping() {
    let mut h = harness();
    h.open(b"/r");
    h.send(Event::QueryChanged("txt".into()));
    h.send(Event::Pause);
    let starts = h.mappings.len();
    assert!(h.state.is_searching(), "the search restarts against the kept partial map");
    h.settle();
    assert_eq!(h.rows(), ["x.txt", "y.txt", "z.txt"]);
    assert!(!h.state.search_outdated());
    assert_eq!(h.mappings.len(), starts);
    // A replaced query on the stopped map still searches.
    h.send(Event::QueryChanged("y.".into()));
    h.settle();
    assert_eq!(h.rows(), ["y.txt"]);
}

#[test]
fn removing_a_matched_item_during_a_partial_search_drops_it_and_keeps_the_query() {
    let mut h = harness();
    h.open(b"/r");
    h.send(Event::QueryChanged("txt".into()));
    h.settle();
    h.send(Event::Collect { paths: vec![p("/r/docs/x.txt")] });
    h.run();
    h.send(Event::BeginReview);
    let review = h.state.review_token().unwrap();
    h.send(Event::ConfirmTrash { review });
    h.settle();
    assert_eq!(h.state.query(), "txt");
    assert_eq!(h.rows(), ["y.txt", "z.txt"]);
}

#[test]
fn typing_repaints_the_map_only_when_a_selection_or_hover_changes() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::Select { path: Some(p("/r/docs/x.txt")) });
    assert!(h.send(Event::QueryChanged("x".into())).contains(Regions::MAP), "the cleared selection outline");
    h.advance(SHORT);
    let reply = h.take(is_search).unwrap();
    assert!(!h.deliver(reply).contains(Regions::MAP), "results without a hover leave the map alone");
    assert!(!h.send(Event::QueryChanged("x.".into())).contains(Regions::MAP));
}
