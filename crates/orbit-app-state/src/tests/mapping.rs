//! Sessions, resolution, previews, gestures, colors, pause/resume and failure handling.

use std::time::Duration;

use super::*;
use crate::backend::Request;
use crate::event::{Effect, Persist, Regions};
use crate::footer::Phase;

#[test]
fn partial_map_is_browsable_and_publication_keeps_route_selection_and_collection() {
    let mut h = harness();
    h.open(b"/r");
    assert!(h.state.is_partial() && h.state.is_scanning());
    assert_eq!(current(&h), "/r");
    assert_eq!(h.rows(), ["big", "docs", "file1"]);
    // Navigate the partial map and select a file before anything is saved.
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    assert_eq!(current(&h), "/r/docs");
    h.send(Event::Select { path: Some(p("/r/docs/x.txt")) });
    h.send(Event::Collect { paths: vec![p("/r/docs/y.txt")] });
    h.run();
    assert_eq!(h.collection(), ["/r/docs/y.txt"]);
    let colors_before = h.state.color_revision();
    h.publish(b"/r");
    h.run();
    assert!(!h.state.is_partial() && !h.state.is_scanning());
    assert_eq!(current(&h), "/r/docs");
    assert_eq!(selected(&h).as_deref(), Some("/r/docs/x.txt"));
    assert_eq!(h.collection(), ["/r/docs/y.txt"]);
    assert_eq!(h.state.color_revision(), colors_before, "same folder keeps its colors");
    assert!(h.persisted.contains(&Persist::LastLocation(b"/r".to_vec())));
    assert!(h.backend.log().contains(&"monitor /r".to_owned()));
    assert_eq!(h.state.footer().map(|f| f.phase), Some(Phase::Saved));
    // Going up keeps the previous folder selected.
    h.send(Event::GoUp);
    h.run();
    assert_eq!(current(&h), "/r");
    assert_eq!(selected(&h).as_deref(), Some("/r/docs"));
}

#[test]
fn stale_resolution_and_preview_work_cannot_replace_a_newer_root() {
    let mut h = harness();
    h.send(Event::OpenLocation { path: b"/r".to_vec(), force: false });
    let first = h.take(|r| matches!(r, Request::Resolve { .. })).unwrap();
    h.send(Event::OpenLocation { path: b"/s".to_vec(), force: false });
    h.run();
    assert_eq!(current(&h), "/s");
    // The abandoned resolution arrives late.
    h.deliver(first);
    h.run();
    assert_eq!(current(&h), "/s");
    // A preview read issued for /s is superseded by switching to /r; its reply is dropped.
    h.preview(b"/s");
    let late = h.take(is_preview_view);
    h.open(b"/r");
    assert_eq!(current(&h), "/r");
    if let Some(late) = late {
        h.deliver(late);
    }
    assert_eq!(current(&h), "/r");
    assert_eq!(h.state.root_path(), Some(&b"/r"[..]));
}

#[test]
fn switching_roots_keeps_the_background_session_and_restores_its_view() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    h.send(Event::Select { path: Some(p("/r/docs/x.txt")) });
    h.send(Event::Collect { paths: vec![p("/r/docs/y.txt")] });
    h.run();
    let starts = h.backend.log().iter().filter(|l| l.starts_with("start /r")).count();
    h.open(b"/s");
    assert_eq!(current(&h), "/s");
    assert!(h.collection().is_empty(), "another root never shows /r's collection");
    // Return: the remembered folder and selection come back immediately, without re-mapping.
    h.send(Event::OpenLocation { path: b"/r".to_vec(), force: false });
    h.run_one(|r| matches!(r, Request::Resolve { .. }));
    assert_eq!(current(&h), "/r/docs");
    assert_eq!(selected(&h).as_deref(), Some("/r/docs/x.txt"));
    assert_eq!(h.collection(), ["/r/docs/y.txt"]);
    h.run();
    assert_eq!(current(&h), "/r/docs");
    let restarts = h.backend.log().iter().filter(|l| l.starts_with("start /r")).count();
    assert_eq!(starts, restarts, "a finished session is reattached, not mapped again");
    assert!(h.state.is_saved_map());
}

#[test]
fn unfinished_session_continues_offscreen_and_its_completion_applies_on_return() {
    let mut h = harness();
    h.open(b"/r");
    h.open(b"/s");
    // /r finishes while /s is shown: nothing of /r is published into /s.
    h.publish(b"/r");
    h.run();
    assert_eq!(current(&h), "/s");
    assert!(h.state.is_partial());
    h.open(b"/r");
    assert_eq!(current(&h), "/r");
    assert!(!h.state.is_partial(), "the reattached session is finished");
    assert_eq!(h.state.footer().map(|f| f.phase), Some(Phase::Saved));
}

#[test]
fn nested_location_reuses_the_covering_map() {
    let mut h = harness();
    h.open_mapped(b"/r");
    let starts = h.mappings.len();
    h.open(b"/r/docs");
    assert_eq!(current(&h), "/r/docs");
    assert_eq!(h.state.root_path(), Some(&b"/r"[..]));
    assert_eq!(h.mappings.len(), starts, "no second index for a location inside the map");
    // Same-root click on the current folder does nothing.
    h.open(b"/r/docs");
    assert_eq!(h.mappings.len(), starts);
}

#[test]
fn replaced_item_at_a_mapped_path_is_mapped_fresh() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.backend.replace(b"/r/docs");
    h.backend.add_file(b"/r/docs/new.txt", 1);
    h.open(b"/r/docs");
    // The replacement becomes its own map instead of being shown from /r's saved map.
    let last = h.mappings.last().unwrap();
    assert_eq!(&*last.root, b"/r/docs");
    h.preview(b"/r/docs");
    h.run();
    assert_eq!(h.state.root_path(), Some(&b"/r/docs"[..]));
    assert!(h.rows().contains(&"new.txt".to_owned()));
}

#[test]
fn pause_keeps_the_partial_map_and_resume_continues_in_a_new_session() {
    let mut h = harness();
    h.open(b"/r");
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    h.mapping(b"/r", MappingEvent::Progress(crate::model::ProgressSnapshot { revision: 3, ..Default::default() }));
    h.send(Event::Pause);
    assert!(h.has_request(|r| matches!(r, Request::StopMapping { .. })));
    h.run();
    assert_eq!(status(&h).as_deref(), Some("Partial map kept. Restart mapping to finish."));
    assert!(h.state.updates_paused());
    assert_eq!(current(&h), "/r/docs", "the route is retained");
    let footer = h.state.footer().unwrap();
    assert_eq!((footer.phase, footer.action), (Phase::Paused, crate::footer::FooterAction::Resume));
    // A preview update of the stopped session publishes nothing.
    let before = h.state.map_revision();
    h.preview(b"/r");
    h.run();
    assert_eq!(h.state.map_revision(), before);
    let old = h.session_for(b"/r");
    h.send(Event::Resume);
    h.run();
    let new = h.session_for(b"/r");
    assert_ne!(old, new);
    assert!(!h.mappings.last().unwrap().force, "resume continues the saved stage");
    assert_eq!(current(&h), "/r/docs");
}

#[test]
fn mapping_failure_keeps_the_partial_map_and_offscreen_failures_stay_offscreen() {
    let mut h = harness();
    h.open(b"/r");
    h.open(b"/s");
    // /r fails while /s is shown: no feedback leaks into /s.
    h.mapping(b"/r", MappingEvent::Failed("disk vanished".into()));
    assert_eq!(error(&h), None);
    h.mapping(b"/s", MappingEvent::Failed("read error".into()));
    assert_eq!(error(&h).as_deref(), Some("read error"));
    assert_eq!(status(&h).as_deref(), Some("Partial map kept. Restart mapping to finish."));
    assert_eq!(current(&h), "/s");
    assert_eq!(h.state.footer().map(|f| f.phase), Some(Phase::Interrupted));
    // A failed session is not reused: opening /r again starts a new one that resumes the stage.
    let failed = h.session_for(b"/r");
    h.open(b"/r");
    assert_ne!(h.session_for(b"/r"), failed);
}

#[test]
fn failure_during_a_newer_opening_keeps_the_newer_request() {
    let mut h = harness();
    h.open(b"/r");
    h.send(Event::OpenLocation { path: b"/s".to_vec(), force: false });
    // /r fails while /s is still resolving.
    h.mapping(b"/r", MappingEvent::Failed("boom".into()));
    assert!(h.state.is_opening(), "the newer opening request is kept");
    h.run();
    assert_eq!(current(&h), "/s");
}

#[test]
fn missing_location_reports_an_error_and_opens_nothing() {
    let mut h = harness();
    h.open(b"/nope");
    assert!(error(&h).is_some_and(|e| e.contains("no such file")));
    assert!(h.state.folder().is_none());
    assert!(h.mappings.is_empty());
    assert!(!h.state.is_opening());
}

#[test]
fn superseded_folder_reads_never_replace_newer_navigation() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::Open { path: p("/r/docs") });
    let docs = h.take(is_saved_view);
    h.send(Event::Open { path: p("/r/big") });
    h.run();
    assert_eq!(current(&h), "/r/big");
    if let Some(docs) = docs {
        h.deliver(docs);
    }
    assert_eq!(current(&h), "/r/big");
}

#[test]
fn gesture_holds_updates_and_applies_them_in_order_on_release() {
    let mut h = harness();
    h.open(b"/r");
    let held = gesture(&mut h, 1);
    let revision = h.state.map_revision();
    // A preview update while the pointer is down does not move the map.
    h.backend.add_file(b"/r/new.bin", 500);
    h.preview(b"/r");
    h.run();
    assert_eq!(h.state.map_revision(), revision);
    assert_eq!(h.rows(), ["big", "docs", "file1"]);
    // Nor does the completion.
    h.publish(b"/r");
    h.run();
    assert!(h.state.is_partial());
    assert_eq!(h.state.map_revision(), revision);
    // A different gesture's end does not release this one.
    h.send(Event::GestureEnd(GestureId(99)));
    assert_eq!(h.state.map_revision(), revision);
    h.send(Event::GestureEnd(held));
    h.run();
    assert!(!h.state.is_partial(), "the held completion published");
    assert_eq!(h.rows(), ["new.bin", "big", "docs", "file1"]);
}

#[test]
fn gesture_holds_a_refreshed_folder_and_a_background_name_sort() {
    let mut h = harness();
    for i in 0..2_100 {
        h.backend.add_file(format!("/r/many/f{i:05}").as_bytes(), 10_000 - i as u64);
    }
    h.open_mapped(b"/r");
    h.send(Event::Open { path: p("/r/many") });
    h.run();
    let held = gesture(&mut h, 1);
    h.send(Event::SortChanged(crate::model::SortOrder::Name));
    assert!(h.state.is_sorting_rows());
    // Monitor reports a change in the open folder; the refreshed folder is held.
    h.backend.add_file(b"/r/many/zzz", 1);
    h.mapping(b"/r", MappingEvent::Changes { directories: vec![b"/r/many".to_vec()], full_refresh: false });
    h.settle();
    assert_eq!(h.state.row_count(), 2_100, "refreshed rows wait for the release");
    assert!(h.state.is_sorting_rows(), "the sort result waits too");
    h.send(Event::GestureEnd(held));
    h.run();
    h.settle();
    assert_eq!(h.state.row_count(), 2_101);
    assert!(!h.state.is_sorting_rows());
    assert_eq!(h.rows().first().map(String::as_str), Some("f00000"));
    assert_eq!(h.rows().last().map(String::as_str), Some("zzz"));
}

#[test]
fn colors_keep_their_branch_ordinals_and_stale_preparations_append() {
    let mut h = harness();
    h.open(b"/r");
    let docs = h.state.color_ordinal(b"docs");
    let big = h.state.color_ordinal(b"big");
    // A file that sorts first does not shift existing hues.
    h.backend.add_file(b"/r/huge", 10_000);
    h.preview(b"/r");
    h.run();
    assert_eq!(h.rows()[0], "huge");
    assert_eq!(h.state.color_ordinal(b"docs"), docs);
    assert_eq!(h.state.color_ordinal(b"big"), big);
    assert_eq!(h.state.color_ordinal(b"huge"), Some(3));
    // A preparation made against an older color revision appends to the newer assignment.
    h.backend.add_file(b"/r/late1", 1);
    h.preview(b"/r");
    let stale = h.take(is_preview_view).unwrap();
    h.backend.add_file(b"/r/late2", 2);
    h.send(Event::Select { path: None });
    h.preview(b"/r");
    h.deliver(stale);
    h.run();
    assert_eq!(h.state.color_ordinal(b"docs"), docs);
    assert!(h.state.color_ordinal(b"late1").is_some() && h.state.color_ordinal(b"late2").is_some());
    let ordinals: std::collections::BTreeSet<_> = ["big", "docs", "file1", "huge", "late1", "late2"]
        .iter()
        .map(|n| h.state.color_ordinal(n.as_bytes()))
        .collect();
    assert_eq!(ordinals.len(), 6, "ordinals stay distinct");
    // A new folder starts a fresh assignment.
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    assert_eq!(h.state.color_ordinal(b"x.txt"), Some(0));
}

#[test]
fn older_progress_and_superseded_preview_reads_never_replace_newer_state() {
    let mut h = harness();
    h.open(b"/r");
    let newer = crate::model::ProgressSnapshot { revision: 5, item_count: 50, ..Default::default() };
    let older = crate::model::ProgressSnapshot { revision: 4, item_count: 40, ..Default::default() };
    let regions = h.mapping(b"/r", MappingEvent::Progress(newer));
    assert_eq!(regions, Regions::FOOTER, "progress notifies only the footer");
    h.mapping(b"/r", MappingEvent::Progress(older));
    assert_eq!(h.state.progress_count(), 50);
    // A preview read computed before a navigation lands after it.
    h.backend.add_file(b"/r/late", 1);
    h.preview(b"/r");
    let held = h.take(is_preview_view).unwrap();
    let late = h.perform_held(held).unwrap();
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    let revision = h.state.map_revision();
    h.send(late);
    assert_eq!(current(&h), "/r/docs");
    assert_eq!(h.state.map_revision(), revision);
}

#[test]
fn deleting_the_open_folder_during_mapping_returns_to_its_parent() {
    let mut h = harness();
    h.open(b"/r");
    h.send(Event::Open { path: p("/r/docs/deep") });
    h.run();
    assert_eq!(current(&h), "/r/docs/deep");
    h.backend.delete(b"/r/docs/deep");
    h.mapping(
        b"/r",
        MappingEvent::RemovalReplay {
            removals: vec![],
            affected_parents: vec![b"/r/docs".to_vec()],
            full_refresh: false,
        },
    );
    h.run();
    assert_eq!(current(&h), "/r/docs");
}

#[test]
fn removal_replay_overflow_blocks_previews_until_a_full_refresh() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.mapping(b"/r", MappingEvent::RemovalReplay { removals: vec![], affected_parents: vec![], full_refresh: true });
    assert!(h.state.folder().is_none(), "the stale map is withheld");
    assert_eq!(status(&h).as_deref(), Some("Updating affected folders…"));
    h.advance(Duration::from_secs(2));
    h.run();
    let last = h.mappings.last().unwrap();
    assert!(last.force && &*last.root == b"/r", "a full refresh re-maps the root");
}

#[test]
fn stop_effects_are_emitted_for_replaced_sessions_only() {
    let mut h = harness();
    h.open(b"/r");
    h.open(b"/s");
    // Switching roots keeps /r mapping in the background.
    assert!(!h.backend.log().contains(&"stop".to_owned()));
    h.send(Event::Refresh);
    h.run();
    assert!(h.backend.log().contains(&"stop".to_owned()), "a forced refresh replaces the session");
    let effects = h.state.handle(Event::Pause, h.now);
    assert!(effects.iter().any(|e| matches!(e, Effect::Backend { request: Request::StopMapping { .. }, .. })));
}

#[test]
fn switching_away_during_a_refresh_restores_the_refreshing_map() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    h.send(Event::Refresh);
    h.run();
    assert!(h.state.is_refreshing());
    h.open_mapped(b"/s");
    h.open(b"/r");
    assert_eq!(current(&h), "/r/docs");
    assert!(h.state.is_refreshing() && !h.state.is_partial());
    // The refresh completes on return.
    h.publish(b"/r");
    h.run();
    assert!(!h.state.is_refreshing());
    assert_eq!(current(&h), "/r/docs");
}

/// Before the first preview the map's opening view shows the progress (items, bytes, the folder
/// being read); afterwards progress reaches the footer only. The list never re-renders for it.
#[test]
fn progress_reaches_the_opening_view_until_the_first_folder_and_never_the_list() {
    let mut h = harness();
    h.send(Event::OpenLocation { path: b"/r".to_vec(), force: false });
    while h.run_one(|request| !is_preview_view(request)) {}
    assert!(h.state.folder().is_none() && h.state.is_scanning());
    let tick = |revision: u64, path: &str| {
        MappingEvent::Progress(crate::model::ProgressSnapshot {
            revision,
            item_count: 12 * revision,
            bytes_scanned: 4_096 * revision,
            current_path: path.as_bytes().to_vec(),
            ..Default::default()
        })
    };
    let regions = h.mapping(b"/r", tick(1, "/r/docs"));
    assert!(regions.contains(Regions::MAP | Regions::FOOTER) && !regions.contains(Regions::LIST), "{regions:?}");
    assert_eq!((h.state.progress_count(), h.state.progress_bytes()), (12, 4_096));
    assert_eq!(h.state.scan_path(), b"/r/docs");

    h.run();
    assert!(h.state.folder().is_some(), "the first preview is shown");
    let regions = h.mapping(b"/r", tick(2, "/r/big"));
    assert!(!regions.intersects(Regions::MAP | Regions::LIST), "{regions:?}");
    assert!(regions.contains(Regions::FOOTER));
    assert_eq!(h.state.scan_path(), b"/r/big");
}
