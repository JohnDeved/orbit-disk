//! Footer count throttling, monitoring and refresh, volumes, places and automation.

use std::time::Duration;

use orbit_automation::{AutomationModel, RemovalMode, RemovalStart};

use super::*;
use crate::event::{Persist, Regions};
use crate::footer::Phase;
use crate::model::Volume;

fn is_count(request: &Request) -> bool {
    matches!(request, Request::DiscoveredCount { .. })
}

fn is_refresh(request: &Request) -> bool {
    matches!(request, Request::Refresh(_))
}

#[test]
fn hidden_refresh_keeps_the_map_and_reads_counts_at_most_every_half_second() {
    let mut h = harness();
    h.open_mapped(b"/r");
    let revision = h.state.map_revision();
    h.send(Event::Refresh);
    h.run();
    assert!(h.mappings.last().unwrap().keep_map, "a full refresh keeps the saved map visible");
    assert!(h.state.is_refreshing() && !h.state.is_partial());
    assert_eq!(h.state.map_revision(), revision);
    // Discovery runs without a visible preview: counts are read, throttled to 0.5 s.
    h.preview(b"/r");
    assert!(h.run_one(is_count));
    h.advance(Duration::from_millis(100));
    h.preview(b"/r");
    h.preview(b"/r");
    assert!(!h.has_request(is_count));
    h.advance(Duration::from_millis(399));
    assert!(!h.has_request(is_count));
    h.advance(Duration::from_millis(1));
    assert!(h.has_request(is_count));
    let regions = {
        let request = h.take(is_count).unwrap();
        h.deliver(request)
    };
    assert_eq!(regions, Regions::FOOTER, "count reads notify only the footer");
    let footer = h.state.footer().unwrap();
    assert_eq!(footer.discovered_files, h.backend.file_count(b"/r"));
    assert_eq!(h.state.map_revision(), revision, "the map is untouched by discovery");
}

#[test]
fn monitor_changes_debounce_into_one_refresh_and_a_failed_update_pauses() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.backend.add_file(b"/r/docs/new.txt", 3);
    h.mapping(b"/r", MappingEvent::Changes { directories: vec![b"/r/docs".to_vec()], full_refresh: false });
    h.advance(Duration::from_millis(1_000));
    h.mapping(b"/r", MappingEvent::Changes { directories: vec![b"/r/big".to_vec()], full_refresh: false });
    assert!(!h.has_request(is_refresh));
    h.advance(Duration::from_millis(500));
    let refresh = h.take(is_refresh).unwrap();
    let Request::Refresh(request) = &refresh.1 else { unreachable!() };
    assert_eq!(request.paths, [b"/r/big".to_vec(), b"/r/docs".to_vec()]);
    assert_eq!(status(&h).as_deref(), Some("Updating changed folders…"));
    assert_eq!(h.state.footer().map(|f| f.phase), Some(Phase::Updating));
    h.deliver(refresh);
    h.run();
    assert_eq!(status(&h).as_deref(), Some("Updated changed folders."));
    // A failed update keeps the map, pauses updates and offers Refresh.
    h.backend.fail_next_refresh("disk busy");
    h.mapping(b"/r", MappingEvent::Changes { directories: vec![b"/r/docs".to_vec()], full_refresh: false });
    h.settle();
    assert_eq!(status(&h).as_deref(), Some("Map kept. Update failed; use Refresh to retry."));
    assert_eq!(error(&h).as_deref(), Some("disk busy"));
    assert!(h.state.updates_paused());
    assert_eq!(h.state.footer().map(|f| f.action), Some(crate::footer::FooterAction::Refresh));
    // Changes while paused are ignored.
    h.mapping(b"/r", MappingEvent::Changes { directories: vec![b"/r/big".to_vec()], full_refresh: false });
    h.advance(Duration::from_secs(2));
    assert!(!h.has_request(is_refresh));
}

#[test]
fn unavailable_monitoring_pauses_updates_with_a_refresh_hint() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.mapping(b"/r", MappingEvent::MonitorUnavailable);
    assert!(h.state.updates_paused());
    assert_eq!(status(&h).as_deref(), Some("Saved map available. Automatic updates unavailable; use Refresh."));
}

#[test]
fn volume_discovery_coalesces_and_keeps_a_root_fallback() {
    let mut h = harness();
    let effects = h.state.launch(None, h.now);
    h.absorb(effects);
    assert_eq!(h.state.volumes()[0].name, "Mac", "fallback while discovery runs");
    // Repeated refreshes while one runs restart discovery exactly once.
    h.send(Event::RefreshVolumes);
    h.send(Event::RefreshVolumes);
    assert_eq!(h.requests.iter().filter(|(_, r)| matches!(r, Request::Volumes)).count(), 1);
    h.backend.set_volumes(vec![]);
    h.run_one(|r| matches!(r, Request::Volumes));
    assert!(h.state.is_discovering_volumes(), "a newer refresh was requested meanwhile");
    assert_eq!(h.requests.iter().filter(|(_, r)| matches!(r, Request::Volumes)).count(), 1);
    h.run();
    assert!(!h.state.is_discovering_volumes());
    assert_eq!(h.state.volumes().len(), 1);
    assert_eq!(h.state.volumes()[0].name, "Mac", "an empty result keeps the root fallback");
    let root = Volume { path: b"/".to_vec(), name: "Macintosh HD".into(), capacity: 10, available: 5 };
    let usb = Volume { path: b"/Volumes/USB".to_vec(), name: "USB".into(), capacity: 2, available: 1 };
    h.backend.set_volumes(vec![root, usb]);
    h.send(Event::RefreshVolumes);
    h.run();
    let names: Vec<&str> = h.state.volumes().iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["Macintosh HD", "USB"]);
}

#[test]
fn places_accept_only_new_directories_and_persist_changes() {
    let mut h = harness();
    h.backend.add_dir(b"/Users/me/Downloads");
    h.send(Event::AddPlaces(vec![
        b"/r/docs".to_vec(),
        b"/r/file1".to_vec(),
        b"/missing".to_vec(),
        b"/Users/me/Downloads".to_vec(),
    ]));
    h.run();
    assert_eq!(h.state.places(), [b"/r/docs".to_vec()]);
    assert_eq!(h.persisted.last(), Some(&Persist::Places(vec![b"/r/docs".to_vec()])));
    h.send(Event::AddPlaces(vec![b"/r/docs".to_vec()]));
    h.run();
    assert_eq!(h.persisted.len(), 1, "duplicates change nothing");
    h.open_mapped(b"/r");
    h.send(Event::Open { path: p("/r/docs") });
    h.run();
    assert_eq!(h.state.selected_place(), Some(&b"/r/docs"[..]));
    h.send(Event::RemovePlace(b"/r/docs".to_vec()));
    assert!(h.state.places().is_empty());
    assert_eq!(h.persisted.last(), Some(&Persist::Places(vec![])));
    assert!(h.backend.identity_of(b"/r/docs").is_some(), "the folder itself is untouched");
}

#[test]
fn automation_collects_and_removes_on_a_partial_map_with_its_own_receipt() {
    let mut h = harness();
    h.open(b"/r");
    assert!(h.state.is_partial());
    let session = AutomationModel::session(&h.state).expect("a map context while mapping");
    let file = AutomationModel::find(&h.state, b"/r/file1").unwrap();
    assert!(AutomationModel::describe(&h.state, &file).can_collect);
    assert!(AutomationModel::collect(&mut h.state, std::slice::from_ref(&file)));
    let effects = h.state.take_automation_effects();
    h.absorb(effects);
    h.run();
    let review = AutomationModel::collection_review(&mut h.state).unwrap();
    let RemovalStart::Started(receipt) =
        AutomationModel::start_removal(&mut h.state, &review, RemovalMode::Trash, session)
    else {
        panic!("removal did not start");
    };
    let effects = h.state.take_automation_effects();
    h.absorb(effects);
    assert!(AutomationModel::removal_receipt(&h.state, &receipt).unwrap().running);
    // A second start while one runs is busy, not a new removal.
    assert_eq!(AutomationModel::start_removal(&mut h.state, &review, RemovalMode::Trash, session), RemovalStart::Busy);
    h.run();
    let done = AutomationModel::removal_receipt(&h.state, &receipt).unwrap();
    assert_eq!((done.running, done.removed_count, done.message.as_str()), (false, Some(1), "Moved 1 item to Trash."));
    // A later native removal does not overwrite the automated receipt.
    h.send(Event::Collect { paths: vec![p("/r/docs")] });
    h.run();
    h.send(Event::BeginReview);
    let review = h.state.review_token().unwrap();
    h.send(Event::ConfirmPermanentDelete { review });
    h.run();
    let kept = AutomationModel::removal_receipt(&h.state, &receipt).unwrap();
    assert_eq!(kept.message, "Moved 1 item to Trash.");
    assert!(h.backend.identity_of(b"/r/file1").is_none());
}
