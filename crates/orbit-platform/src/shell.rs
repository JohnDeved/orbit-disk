//! UI-side platform services. Implemented by `orbit-shell-macos` (and future shells); anything
//! GPUI already provides (reveal, open, folder picker, clipboard, menus, cursors, drag and drop)
//! is intentionally absent.

use std::path::PathBuf;

pub trait Shell: 'static {
    /// The user asked the OS to reduce motion: disable fill transitions and activity animation.
    fn reduce_motion(&self) -> bool;

    /// Deep link to the OS privacy pane for full-disk access, if the OS has one.
    fn privacy_settings_url(&self) -> Option<&'static str>;

    /// Arranges for a fresh instance to start once this process exits (used after clearing
    /// caches). The caller then quits; if the quit is refused, it must call
    /// [`RelaunchHandle::cancel`] so no second instance appears later.
    fn relaunch(&self) -> std::io::Result<RelaunchHandle>;

    /// The auto-updater, when this build ships one.
    fn updater(&self) -> Option<&dyn Updater>;

    /// Settings saved by the previous (Swift) app, read once on first launch.
    fn import_legacy_settings(&self) -> Option<LegacySettings>;

    /// Called once for every window right after it is created, with its native handle. The
    /// macOS shell tags the window's Metal layer as sRGB so colors are color-managed exactly
    /// like AppKit/SwiftUI content (GPUI leaves the layer untagged, which renders the palette
    /// with the display's raw primaries).
    fn prepare_window(&self, _window: raw_window_handle::RawWindowHandle) {}
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacySettings {
    pub places: Vec<PathBuf>,
    pub last_location: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdaterState {
    pub automatic_checks: bool,
    pub can_check: bool,
    /// An update is downloaded and will install once the running removal finishes.
    pub waiting_for_removal: bool,
}

/// A pending relaunch helper. Dropping it leaves the relaunch armed.
#[derive(Debug)]
pub struct RelaunchHandle(Option<std::process::Child>);

impl RelaunchHandle {
    pub fn new(helper: Option<std::process::Child>) -> Self {
        Self(helper)
    }

    /// Disarms the relaunch (the quit that should follow it was refused).
    pub fn cancel(mut self) {
        if let Some(mut helper) = self.0.take() {
            let _ = helper.kill();
            let _ = helper.wait();
        }
    }
}

pub trait Updater {
    /// Starts scheduled checks. Call once the main window is on screen; idempotent.
    fn start(&self);
    fn state(&self) -> UpdaterState;
    fn set_automatic_checks(&self, enabled: bool);
    fn check_now(&self);
    /// While `true`, update checks/installs and relaunch are postponed (removal in progress).
    fn set_blocked(&self, blocked: bool);
}
