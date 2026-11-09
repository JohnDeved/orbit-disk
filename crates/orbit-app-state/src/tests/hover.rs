//! Map ↔ row hover relations, groups, search rows, stale layouts and refreshes.

use super::*;
use crate::model::SortOrder;
use orbit_sunburst::SegmentIndex;

/// `/g`: one huge file, a folder with one big file and 51 tiny ones, 60 tiny files.
fn grouped() -> Harness {
    let h = harness();
    let b = &h.backend;
    b.add_file(b"/g/huge", 1_000_000);
    b.add_file(b"/g/sub/big", 500_000);
    for i in 0..51 {
        b.add_file(format!("/g/sub/s{i:02}").as_bytes(), 1);
    }
    for i in 0..60 {
        b.add_file(format!("/g/t{i:02}").as_bytes(), 1);
    }
    h
}

fn segment_of(h: &mut Harness, path: &str) -> (SegmentIndex, u64) {
    let presentation = h.state.presentation().unwrap();
    let folder = h.state.folder().unwrap().clone();
    let index = presentation
        .layout
        .segments()
        .iter()
        .position(|s| s.node.is_some_and(|ix| folder.path(ix) == path.as_bytes()))
        .expect("segment for path");
    (index, presentation.layout_revision)
}

fn group_of(h: &mut Harness, parent: &str) -> (SegmentIndex, u64) {
    let presentation = h.state.presentation().unwrap();
    let folder = h.state.folder().unwrap().clone();
    let ix = folder.find(parent.as_bytes()).unwrap();
    (presentation.layout.group_of_parent(ix).expect("group"), presentation.layout_revision)
}

fn hover(h: &mut Harness, (segment, layout_revision): (SegmentIndex, u64)) {
    h.send(Event::HoverSegment { segment, layout_revision });
}

fn related(h: &Harness) -> Vec<String> {
    (0..h.state.row_count())
        .filter(|&i| h.state.is_row_related(i))
        .map(|i| String::from_utf8_lossy(h.state.row(i).unwrap().name()).into_owned())
        .collect()
}

#[test]
fn map_hover_relates_nodes_deep_segments_and_groups_to_rows() {
    let mut h = grouped();
    h.open_mapped(b"/g");
    let huge = segment_of(&mut h, "/g/huge");
    hover(&mut h, huge);
    assert_eq!(related(&h), ["huge"]);
    assert_eq!(h.state.hover_description(), Some("huge"));
    // An outer-ring segment highlights its visible ancestor row and names its relative path.
    let deep = segment_of(&mut h, "/g/sub/big");
    hover(&mut h, deep);
    assert_eq!(related(&h), ["sub"]);
    assert_eq!(h.state.hover_description(), Some("sub/big"));
    // A root group relates every member row, and only those.
    let group = group_of(&mut h, "/g");
    hover(&mut h, group);
    let rows = related(&h);
    assert_eq!(rows.len(), 60);
    assert!(rows.iter().all(|r| r.starts_with('t')));
    assert_eq!(h.state.hover_description(), Some("60 items in g"));
    assert_eq!(h.state.first_related_row(), Some(2));
    // A deep group relates its folder's row.
    let deep_group = group_of(&mut h, "/g/sub");
    hover(&mut h, deep_group);
    assert_eq!(related(&h), ["sub"]);
    assert_eq!(h.state.hover_description(), Some("51 items in sub"));
    // Group relations follow a new sort order rather than pinning the old one.
    hover(&mut h, group);
    h.send(Event::SortChanged(SortOrder::Name));
    assert_eq!(h.rows()[0], "huge");
    assert_eq!(h.state.first_related_row(), Some(2), "huge, sub, then t00…");
    assert!(h.state.is_row_related(2) && !h.state.is_row_related(0));
    // Row hover outlines the segment showing the row (its group for a grouped child).
    h.send(Event::HoverRow { path: p("/g/t07") });
    assert_eq!(h.state.row_hover_segment(), Some(group.0));
    h.send(Event::HoverRow { path: p("/g/huge") });
    assert_eq!(h.state.row_hover_segment(), Some(huge.0));
}

#[test]
fn stale_layout_hover_is_ignored_and_navigation_clears_hover() {
    let mut h = grouped();
    h.open_mapped(b"/g");
    let huge = segment_of(&mut h, "/g/huge");
    hover(&mut h, huge);
    let (group, _) = group_of(&mut h, "/g");
    // A hover computed against an older layout revision cannot replace the current hover.
    h.send(Event::HoverSegment { segment: group, layout_revision: huge.1 + 1_000 });
    assert_eq!(related(&h), ["huge"]);
    h.send(Event::Open { path: p("/g/sub") });
    h.run();
    assert!(h.state.hover().is_empty());
    assert_eq!(h.state.hover_description(), None);
    // The old layout's segment index is stale in the new folder.
    h.send(Event::HoverSegment { segment: huge.0, layout_revision: huge.1 });
    assert!(h.state.hover().is_empty());
}

#[test]
fn search_rows_relate_to_exact_matches_and_to_group_ancestors() {
    let mut h = grouped();
    h.open_mapped(b"/g");
    let huge = segment_of(&mut h, "/g/huge");
    hover(&mut h, huge);
    // Changing the query clears hover.
    h.send(Event::QueryChanged("t0".into()));
    assert!(h.state.hover().is_empty());
    h.settle();
    assert_eq!(h.state.row_count(), 10);
    let group = group_of(&mut h, "/g");
    hover(&mut h, group);
    assert_eq!(related(&h).len(), 10, "every matching member row");
    hover(&mut h, huge);
    assert!(related(&h).is_empty(), "huge is not among the results");
    // A deep group relates search rows that contain its parent, never siblings or descendants.
    h.send(Event::QueryChanged("s".into()));
    h.settle();
    let deep_group = group_of(&mut h, "/g/sub");
    hover(&mut h, deep_group);
    let rows = related(&h);
    assert!(rows.contains(&"sub".to_owned()));
    assert!(rows.iter().all(|r| r == "sub" || (r.starts_with('s') && r.len() == 3)));
    // Same query, new results (an item vanished): relations are recomputed.
    h.backend.delete(b"/g/sub/s00");
    h.send(Event::RetrySearch);
    h.send(Event::QueryChanged("s ".into()));
    h.settle();
    assert!(!h.rows().contains(&"s00".to_owned()));
    assert!(!related(&h).contains(&"s00".to_owned()));
}

#[test]
fn group_hover_survives_same_membership_updates_and_clears_when_members_change() {
    let mut h = grouped();
    h.open(b"/g");
    let group = group_of(&mut h, "/g");
    hover(&mut h, group);
    assert_eq!(related(&h).len(), 60);
    // A preview update that keeps the membership (a size change elsewhere) keeps the group.
    h.backend.add_file(b"/g/sub/more", 10);
    h.preview(b"/g");
    h.run();
    assert_eq!(related(&h).len(), 60);
    assert_eq!(h.state.hover_description(), Some("60 items in g"));
    // A new member: the exact previous members stay hovered.
    h.backend.add_file(b"/g/t99", 1);
    h.preview(b"/g");
    h.run();
    assert_eq!(related(&h).len(), 60);
    assert!(!related(&h).contains(&"t99".to_owned()));
    // A replaced member (new identity) clears the hover.
    h.backend.replace(b"/g/t05");
    h.preview(b"/g");
    h.run();
    assert!(h.state.hover().is_empty());
    // Pointer exit leaves nothing behind.
    let huge = segment_of(&mut h, "/g/huge");
    hover(&mut h, huge);
    h.send(Event::ClearMapHover);
    assert!(h.state.hover().is_empty());
}
