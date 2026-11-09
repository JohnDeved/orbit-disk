//! Pure bookkeeping of the Contents list (no GPUI views): hover suppression while scrolling
//! (Swift `ListHoverTracking`), the scroll anchor kept across re-sorts and reloads, and the
//! scrollbar geometry.

use gpui::{Bounds, Pixels, TouchPhase, point, px, size};
use orbit_app_state::{AppState, RowView};
use orbit_core::paths::file_name;

use crate::theme;

/// Trackpad fingers that never report `Ended` stop deferring the restore after this many
/// settle periods without scrolling (≈ 1 s).
const MAX_GESTURE_REARMS: u8 = 5;

/// Row-hover suppression while the list scrolls: wheel input, live scrolling and momentum.
///
/// The first scroll activity clears the row hover once; row hover input is ignored until the
/// scroll offset has been still for one settle period (and no trackpad gesture is held), then
/// the row under the pointer is hovered again.
#[derive(Debug, Default)]
pub(super) struct ScrollHover {
    suppressed: bool,
    gesture: bool,
    rearms: u8,
    epoch: u64,
}

/// What a settle timer should do when it fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Settle {
    /// A newer scroll activity superseded this timer.
    Stale,
    /// Scrolling may continue (fingers on the trackpad): wait one more period.
    Wait,
    /// Scrolling settled: hover the row under the pointer.
    Restore,
}

impl ScrollHover {
    pub fn suppressed(&self) -> bool {
        self.suppressed
    }

    /// Scroll activity. Returns whether it began suppression (clear the hover now) and the
    /// epoch the settle timer must carry.
    pub fn scrolled(&mut self) -> (bool, u64) {
        let began = !self.suppressed;
        self.suppressed = true;
        self.rearms = 0;
        self.epoch += 1;
        (began, self.epoch)
    }

    /// Trackpad phase of a wheel event over the list.
    pub fn phase(&mut self, phase: TouchPhase) {
        match phase {
            TouchPhase::Started => self.gesture = true,
            TouchPhase::Ended => self.gesture = false,
            TouchPhase::Moved => {}
        }
    }

    /// The settle timer of `epoch` fired.
    pub fn settle(&mut self, epoch: u64) -> Settle {
        if epoch != self.epoch || !self.suppressed {
            return Settle::Stale;
        }
        if self.gesture && self.rearms < MAX_GESTURE_REARMS {
            self.rearms += 1;
            return Settle::Wait;
        }
        self.gesture = false;
        self.suppressed = false;
        Settle::Restore
    }
}

/// The display row at `y` (window coordinates) of a list whose viewport starts at `top` and is
/// scrolled by `offset_y` (≤ 0).
pub(super) fn row_at_y(y: Pixels, top: Pixels, offset_y: Pixels, count: usize) -> Option<usize> {
    let content_y = y - top - offset_y;
    if content_y < px(0.) {
        return None;
    }
    let row = (content_y / theme::ROW_HEIGHT).floor() as usize;
    (row < count).then_some(row)
}

/// The scroll offset (≤ 0) that brings `row` fully into a `viewport`-high view with the least
/// movement, or `None` when it is already fully visible.
pub(super) fn reveal_offset(row: usize, offset_y: Pixels, viewport: Pixels) -> Option<Pixels> {
    let top = theme::ROW_HEIGHT * row;
    let bottom = top + theme::ROW_HEIGHT;
    let scroll_top = -offset_y;
    if top < scroll_top {
        Some(-top)
    } else if bottom > scroll_top + viewport {
        Some(-(bottom - viewport).max(px(0.)))
    } else {
        None
    }
}

/// Clamps a scroll offset to the content of `count` rows in a `viewport`-high view.
pub(super) fn clamp_offset(offset_y: Pixels, count: usize, viewport: Pixels) -> Pixels {
    let max = (theme::ROW_HEIGHT * count - viewport).max(px(0.));
    offset_y.min(px(0.)).max(-max)
}

/// Thumb of the scrollbar drawn in `gutter` for a list of `content` height in a `viewport`.
pub(super) fn thumb(
    gutter: Bounds<Pixels>,
    viewport: Pixels,
    content: Pixels,
    offset_y: Pixels,
) -> Option<Bounds<Pixels>> {
    if viewport <= px(0.) || content <= viewport {
        return None;
    }
    let inset = px(2.);
    let track = gutter.size.height - inset * 2.;
    let height = (track * (viewport / content)).max(px(24.)).min(track);
    let travel = track - height;
    let progress = ((-offset_y) / (content - viewport)).clamp(0., 1.);
    let width = px(6.);
    Some(Bounds::new(
        point(gutter.origin.x + (gutter.size.width - width) / 2., gutter.origin.y + inset + travel * progress),
        size(width, height),
    ))
}

/// The first visible row's identity, kept across list reloads and re-sorts so the rows the user
/// looks at stay in place (Swift `ListHoverTracking.prepareForReset` / restore).
#[derive(Debug, Default)]
pub(super) struct Anchor {
    /// Name of a folder row, or the path of a search hit.
    key: Vec<u8>,
    /// (display row, list revision) the key was read at.
    captured: Option<(usize, u64)>,
    /// How far the anchor row is scrolled past the viewport top.
    intra: Pixels,
}

impl Anchor {
    /// Records the first visible row for `offset_y`. Copies its name only when the row changed.
    pub fn capture(&mut self, state: &AppState, offset_y: Pixels) {
        let scroll_top = (-offset_y).max(px(0.));
        let row = (scroll_top / theme::ROW_HEIGHT).floor() as usize;
        self.intra = scroll_top - theme::ROW_HEIGHT * row;
        let revision = state.list_revision();
        if self.captured == Some((row, revision)) {
            return;
        }
        self.key.clear();
        match state.row(row) {
            Some(RowView::Node { folder, ix }) => self.key.extend_from_slice(folder.name(ix)),
            Some(RowView::Hit(item)) => self.key.extend_from_slice(&item.path),
            None => {
                self.captured = None;
                return;
            }
        }
        self.captured = Some((row, revision));
    }

    pub fn clear(&mut self) {
        self.captured = None;
        self.key.clear();
    }

    /// The offset that shows the anchor row where it was, in the current rows and order.
    pub fn restore(&self, state: &AppState) -> Option<Pixels> {
        self.captured?;
        let row = if state.filtering() {
            (0..state.row_count())
                .find(|&d| matches!(state.row(d), Some(RowView::Hit(item)) if *item.path == *self.key))?
        } else {
            display_of_base(state, state.folder()?.row_of_name(&self.key)?)?
        };
        Some(-(theme::ROW_HEIGHT * row + self.intra))
    }
}

/// The display position of unsorted row `base` (O(1) without a name order, a scan with one).
pub(super) fn display_of_base(state: &AppState, base: usize) -> Option<usize> {
    if state.base_row(base) == base {
        return Some(base);
    }
    (0..state.row_count()).find(|&display| state.base_row(display) == base)
}

/// An item of the list resolved once per frame, compared per row without building paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowKey<'a> {
    None,
    /// Unsorted folder row.
    Base(usize),
    /// Search hit path.
    Hit(&'a [u8]),
}

impl<'a> RowKey<'a> {
    pub fn of(state: &AppState, path: Option<&'a [u8]>) -> Self {
        let Some(path) = path else { return Self::None };
        if state.filtering() {
            return Self::Hit(path);
        }
        let Some(folder) = state.folder() else { return Self::None };
        let parent = orbit_app_state::paths::parent_or_root(path);
        if parent != folder.folder_path() || path == folder.folder_path() {
            return Self::None;
        }
        folder.row_of_name(file_name(path)).map_or(Self::None, Self::Base)
    }

    pub fn matches(self, state: &AppState, display: usize) -> bool {
        match self {
            Self::None => false,
            Self::Base(base) => state.base_row(display) == base,
            Self::Hit(path) => matches!(state.row(display), Some(RowView::Hit(item)) if *item.path == *path),
        }
    }

    /// Display position (a scan for sorted folders and hits).
    pub fn display(self, state: &AppState) -> Option<usize> {
        match self {
            Self::None => None,
            Self::Base(base) => display_of_base(state, base),
            Self::Hit(_) => (0..state.row_count()).find(|&d| self.matches(state, d)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settle_restores_only_after_the_last_activity_and_released_fingers() {
        let mut hover = ScrollHover::default();
        let (began, first) = hover.scrolled();
        assert!(began && hover.suppressed());
        let (began, second) = hover.scrolled();
        assert!(!began, "hover is cleared once per scroll");
        assert_eq!(hover.settle(first), Settle::Stale, "a newer activity re-armed the timer");
        hover.phase(TouchPhase::Started);
        assert_eq!(hover.settle(second), Settle::Wait, "fingers still on the trackpad");
        hover.phase(TouchPhase::Ended);
        assert_eq!(hover.settle(second), Settle::Restore);
        assert!(!hover.suppressed());
        // A gesture whose end never arrives stops deferring.
        let (_, epoch) = hover.scrolled();
        hover.phase(TouchPhase::Started);
        let waits = std::iter::repeat_with(|| hover.settle(epoch)).take_while(|s| *s == Settle::Wait).count();
        assert_eq!(waits, MAX_GESTURE_REARMS as usize);
        assert!(!hover.suppressed());
    }

    #[test]
    fn reveal_moves_the_least_and_only_for_hidden_rows() {
        let h = theme::ROW_HEIGHT;
        let viewport = h * 10;
        assert_eq!(reveal_offset(3, px(0.), viewport), None);
        assert_eq!(reveal_offset(9, px(0.), viewport), None);
        assert_eq!(reveal_offset(10, px(0.), viewport), Some(-h), "bottom edge: scroll one row");
        assert_eq!(reveal_offset(2, -(h * 5), viewport), Some(-(h * 2)), "above: align to the top");
        assert_eq!(reveal_offset(5, -(h * 5) - px(1.), viewport), Some(-(h * 5)), "partly hidden at the top");
    }
}
