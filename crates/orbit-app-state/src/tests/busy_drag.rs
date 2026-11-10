//! Dragging is never refused because the map is busy: refreshes, rescans, mapping and journal
//! replays update in the background, and a drop collects (validated on demand) whatever they do.

use std::time::Duration;

use super::*;
use crate::state::REFRESH_DELAY;
use crate::testing::Harness;

fn drop_on_tray(h: &mut Harness, drag: DragId) {
    h.send(Event::DragOverTray { drag, over: true });
    assert!(h.state.drop_target_active(), "Release to collect");
    h.send(Event::DragDrop { drag });
}

fn changes(h: &mut Harness, directories: &[&str]) {
    let directories = directories.iter().map(|d| d.as_bytes().to_vec()).collect();
    h.mapping(b"/r", MappingEvent::Changes { directories, subtrees: Vec::new(), full_refresh: false });
}

#[test]
fn a_drag_during_a_refresh_collects() {
    let mut h = harness();
    h.open_mapped(b"/r");
    changes(&mut h, &["/r/docs"]);
    h.advance(REFRESH_DELAY);
    assert!(h.state.is_refreshing(), "a refresh is in flight");
    let drag = drag_items(&mut h, 1, &["/r/file1"]);
    assert_eq!(h.state.drag_id(), Some(drag), "the drag starts while the map updates");
    drop_on_tray(&mut h, drag);
    h.settle();
    assert_eq!(h.collection(), ["/r/file1"]);
}

#[test]
fn a_refresh_landing_mid_drag_keeps_the_drag() {
    let mut h = harness();
    h.open_mapped(b"/r");
    let drag = drag_items(&mut h, 1, &["/r/big"]);
    h.backend.add_file(b"/r/docs/new.txt", 4);
    changes(&mut h, &["/r/docs"]);
    h.advance(REFRESH_DELAY);
    h.run();
    assert_eq!(h.state.drag_id(), Some(drag), "the published refresh did not cancel the drag");
    drop_on_tray(&mut h, drag);
    h.settle();
    assert_eq!(h.collection(), ["/r/big"]);
}

#[test]
fn a_drag_during_rescan_everything_collects() {
    let mut h = harness();
    h.open_mapped(b"/r");
    h.send(Event::RescanEverything);
    h.run();
    assert!(h.state.is_refreshing() || h.state.is_scanning(), "the rescan runs");
    let drag = drag_items(&mut h, 1, &["/r/file1"]);
    assert_eq!(h.state.drag_id(), Some(drag), "the saved map stays draggable while it is rescanned");
    drop_on_tray(&mut h, drag);
    h.run();
    assert_eq!(h.collection(), ["/r/file1"]);
}

#[test]
fn a_drag_from_a_map_still_mapping_collects() {
    let mut h = harness();
    h.open(b"/r");
    h.preview(b"/r");
    h.run();
    assert!(h.state.is_scanning() && h.state.row_count() > 0, "the partial map is shown");
    let drag = drag_items(&mut h, 1, &["/r/file1"]);
    assert_eq!(h.state.drag_id(), Some(drag), "the partial map is draggable");
    drop_on_tray(&mut h, drag);
    h.run();
    assert_eq!(h.collection(), ["/r/file1"]);
    let _ = Duration::ZERO;
}
