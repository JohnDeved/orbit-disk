//! Quitting and closing guards.
//!
//! While any removal runs (`AppState::removal_active`), quitting and closing the main window are
//! refused with a visible notice (Swift `applicationShouldTerminate`). GPUI 0.2.2 has no
//! terminate veto for quits that bypass our actions (Dock menu, logout): those still quit.

use gpui::App;

use crate::host::Host;

pub const QUIT_BLOCKED: &str = "Files are still being removed. Quit after this finishes.";

/// Whether the app (or the main window) may close now. When not, shows [`QUIT_BLOCKED`].
pub fn may_quit(host: &Host, cx: &mut App) -> bool {
    if host.state(cx).removal_active() {
        host.update(cx, |host, cx| host.set_notice(Some(QUIT_BLOCKED.into()), cx));
        return false;
    }
    true
}

/// The Quit action: quits unless a removal runs.
pub fn quit(host: &Host, cx: &mut App) {
    if may_quit(host, cx) {
        cx.quit();
    }
}
