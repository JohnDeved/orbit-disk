//! Row text (formatted once per row and list revision) and the row element.

use std::collections::HashMap;
use std::ops::Range;

use gpui::{Hsla, SharedString};
use orbit_app_state::{AppState, RowView};
use orbit_core::paths::file_name;
use orbit_core::types::ScanState;
use orbit_core::view::NodeMeta;
use orbit_sunburst::CollectionState;

use crate::theme;

/// Everything a row shows that does not depend on pointer state.
#[derive(Clone, Debug)]
pub(super) struct RowText {
    pub name: SharedString,
    /// Collection mark on the secondary line.
    pub mark: CollectionState,
    /// Secondary line (file count, type, state, or a search hit's folder). Empty when the mark
    /// replaces it.
    pub detail: SharedString,
    pub size: SharedString,
    /// Share of the folder ("12.3%"), "so far", or empty.
    pub share: SharedString,
    pub swatch: Hsla,
    pub is_dir: bool,
}

/// Inputs of [`RowText`] besides the row itself; the cache is dropped when they change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Inputs {
    list: u64,
    colors: u64,
    collection: u64,
    filtering: bool,
    scanning: bool,
    updating_sizes: bool,
    /// The removal overlay (which folders show "Updating…").
    removal_epoch: u64,
}

impl Inputs {
    fn of(state: &AppState) -> Self {
        Self {
            list: state.list_revision(),
            colors: state.color_revision(),
            collection: state.collection().map_or(0, |c| c.revision()),
            filtering: state.filtering(),
            scanning: state.is_scanning(),
            updating_sizes: state.updating_removal_sizes(),
            removal_epoch: state.active_epoch(),
        }
    }
}

/// Formatted rows around the visible range. Scrolling reuses the rows that stay visible; a new
/// list revision (rows, order), color, collection or mapping state drops them.
#[derive(Debug, Default)]
pub(super) struct RowCache {
    inputs: Inputs,
    rows: HashMap<usize, RowText>,
    /// Rows formatted since creation (diagnostics, tests).
    pub formatted: u64,
}

/// Rows kept beyond the visible range before eviction.
const KEEP: usize = 64;

impl RowCache {
    /// Prepares the cache for drawing `range` of `state`.
    pub fn begin(&mut self, state: &AppState, range: &Range<usize>) {
        let inputs = Inputs::of(state);
        if inputs != self.inputs {
            self.inputs = inputs;
            self.rows.clear();
        } else if self.rows.len() > 4 * KEEP && range.len() > 1 {
            let keep = range.start.saturating_sub(KEEP)..range.end + KEEP;
            self.rows.retain(|row, _| keep.contains(row));
        }
    }

    pub fn get(&mut self, state: &AppState, display: usize) -> Option<RowText> {
        if let Some(text) = self.rows.get(&display) {
            return Some(text.clone());
        }
        let text = format_row(state, display)?;
        self.formatted += 1;
        self.rows.insert(display, text.clone());
        Some(text)
    }
}

fn format_row(state: &AppState, display: usize) -> Option<RowText> {
    let row = state.row(display)?;
    let meta = *row.meta();
    let filtering = state.filtering();
    let updating = meta.flags.is_dir && state.updating_removal_size(&row.path(), true);
    let name = row.name();
    let mark = match row {
        RowView::Hit(item) => state.collection_state(&item.path),
        RowView::Node { .. } if state.collection().is_some_and(|c| !c.is_empty()) => {
            state.collection_state(&row.path())
        }
        RowView::Node { .. } => CollectionState::None,
    };
    let detail: SharedString = if mark != CollectionState::None && !filtering {
        SharedString::default()
    } else if let RowView::Hit(item) = row {
        theme::display(orbit_app_state::paths::parent_or_root(&item.path)).into()
    } else if updating {
        "Updating sizes".into()
    } else {
        node_detail(&meta, name, state.is_scanning())
    };
    let size: SharedString = if updating {
        "Updating…".into()
    } else if !meta.flags.accessible {
        "Unavailable".into()
    } else if meta.state == ScanState::Pending {
        "Waiting".into()
    } else {
        theme::format_bytes(meta.size).into()
    };
    let share: SharedString = if state.updating_removal_sizes() {
        SharedString::default()
    } else if meta.state == ScanState::Scanning {
        "so far".into()
    } else if !filtering && meta.state == ScanState::Complete {
        let parent = state.folder().map_or(0, |f| f.folder_meta().size);
        theme::format_percent(if parent > 0 { meta.size as f64 / parent as f64 } else { 0.0 }).into()
    } else {
        SharedString::default()
    };
    let swatch = if filtering {
        theme::color(theme::SLATE).into()
    } else {
        theme::chart(state.color_ordinal(name).unwrap_or(0), 0)
    };
    Some(RowText { name: theme::display(name).into(), mark, detail, size, share, swatch, is_dir: meta.flags.is_dir })
}

/// The secondary line of an item's row (and of its map tooltip): its readability or mapping
/// state, "Symbolic link", the file count of a folder, or a file's type. `mapping`: a mapping
/// is running (an unfinished folder is being mapped rather than incomplete).
pub(crate) fn node_detail(meta: &NodeMeta, name: &[u8], mapping: bool) -> SharedString {
    if !meta.flags.accessible {
        "Not scanned".into()
    } else if meta.state == ScanState::Pending {
        "Waiting to map".into()
    } else if meta.state == ScanState::Scanning {
        let phase = if mapping { "Mapping" } else { "Incomplete" };
        format!("{} files found · {phase}", theme::format_count(meta.file_count)).into()
    } else if meta.flags.is_symlink {
        "Symbolic link".into()
    } else if meta.flags.is_dir {
        let noun = if meta.file_count == 1 { "file" } else { "files" };
        format!("{} {noun}", theme::format_count(meta.file_count)).into()
    } else {
        file_type(name)
    }
}

/// Uppercased extension (`"PNG"`), or `"File"`.
fn file_type(name: &[u8]) -> SharedString {
    let name = file_name(name);
    match name.iter().rposition(|&b| b == b'.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => String::from_utf8_lossy(&name[dot + 1..]).to_uppercase().into(),
        _ => SharedString::new_static("File"),
    }
}
