//! Pure pointer rules shared by the map and the rows: the drag threshold and double-click opening.
//! The deferral of updates while a gesture is held lives in the state machine
//! ([`crate::Event::GestureBegin`] / [`crate::Event::GestureEnd`]).

use std::sync::Arc;

/// Movement (points) before a press becomes a drag; shorter movements still click.
pub const DRAG_THRESHOLD: f64 = 5.0;

/// Whether moving from `pressed` to `current` starts a drag.
pub fn exceeds_drag_threshold(pressed: (f64, f64), current: (f64, f64)) -> bool {
    (current.0 - pressed.0).hypot(current.1 - pressed.1) >= DRAG_THRESHOLD
}

/// Row click tracking (Swift `ListRowInteractionView`): a row opens on the mouse-up of a second
/// click on the same item within the double-click interval, once; a press that moved beyond the
/// threshold (a drag, even a rejected one) never opens and does not count as a click.
#[derive(Clone, Debug, Default)]
pub struct RowClicks {
    last_click: Option<(Arc<[u8]>, f64)>,
    pressed: Option<Press>,
}

#[derive(Clone, Debug)]
struct Press {
    item: Arc<[u8]>,
    timestamp: f64,
    opens_on_release: bool,
    moved: bool,
}

impl RowClicks {
    /// Mouse down on `item` (`click_count` from the OS event, times in seconds).
    pub fn press(&mut self, item: Arc<[u8]>, timestamp: f64, click_count: u32, double_click_interval: f64) {
        let opens = click_count == 2
            && self.last_click.as_ref().is_some_and(|(last, at)| {
                let interval = timestamp - at;
                *last == item && (0.0..=double_click_interval).contains(&interval)
            });
        self.pressed = Some(Press { item, timestamp, opens_on_release: opens, moved: false });
    }

    /// The pointer moved beyond the drag threshold during the press.
    pub fn moved(&mut self) {
        if let Some(press) = &mut self.pressed {
            press.moved = true;
        }
    }

    /// Mouse up. Returns the item to open, if this release completes a double click.
    pub fn release(&mut self, inside: bool) -> Option<Arc<[u8]>> {
        let press = self.pressed.take()?;
        if press.moved || !inside {
            return None;
        }
        self.last_click = Some((Arc::clone(&press.item), press.timestamp));
        if press.opens_on_release {
            // A third click must start a new pair.
            self.last_click = None;
            return Some(press.item);
        }
        None
    }

    /// A gesture was cancelled (e.g. the list reloaded under the pointer).
    pub fn cancel(&mut self) {
        self.pressed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_movements_still_click_and_threshold_starts_drag() {
        assert!(!exceeds_drag_threshold((10.0, 10.0), (13.0, 13.9)));
        assert!(exceeds_drag_threshold((10.0, 10.0), (13.0, 14.0)));
    }

    #[test]
    fn double_click_opens_once_and_dragged_press_never_opens() {
        let item: Arc<[u8]> = Arc::from(&b"/r/a"[..]);
        let other: Arc<[u8]> = Arc::from(&b"/r/b"[..]);
        let mut clicks = RowClicks::default();
        clicks.press(item.clone(), 1.0, 1, 0.5);
        assert_eq!(clicks.release(true), None);
        clicks.press(item.clone(), 1.2, 2, 0.5);
        assert_eq!(clicks.release(true), Some(item.clone()));
        // A third click does not open again.
        clicks.press(item.clone(), 1.3, 3, 0.5);
        assert_eq!(clicks.release(true), None);
        // A press that turned into a (rejected) drag never opens.
        clicks.press(item.clone(), 2.0, 1, 0.5);
        clicks.release(true);
        clicks.press(item.clone(), 2.1, 2, 0.5);
        clicks.moved();
        assert_eq!(clicks.release(true), None);
        // Different item or too slow.
        clicks.press(item.clone(), 3.0, 1, 0.5);
        clicks.release(true);
        clicks.press(other, 3.1, 2, 0.5);
        assert_eq!(clicks.release(true), None);
        clicks.press(item.clone(), 4.0, 1, 0.5);
        clicks.release(true);
        clicks.press(item, 4.9, 2, 0.5);
        assert_eq!(clicks.release(true), None);
    }
}
