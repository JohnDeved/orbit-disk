//! UI-side platform services. Implemented by `orbit-shell-macos` (and future shells); anything
//! GPUI already provides (reveal, open, folder picker, clipboard, menus, cursors, drag and drop)
//! is intentionally absent.

use std::path::PathBuf;

pub trait Shell: 'static {
    /// The user asked the OS to reduce motion: disable fill transitions and activity animation.
    fn reduce_motion(&self) -> bool;

    /// Deep link to the OS privacy pane for full-disk access, if the OS has one.
    fn privacy_settings_url(&self) -> Option<&'static str>;

    /// Starts a fresh instance of the app (used after clearing caches) and returns. The caller
    /// quits afterwards.
    fn relaunch(&self) -> std::io::Result<()>;

    /// The auto-updater, when this build ships one.
    fn updater(&self) -> Option<&dyn Updater>;

    /// Settings saved by the previous (Swift) app, read once on first launch.
    fn import_legacy_settings(&self) -> Option<LegacySettings>;
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
}

pub trait Updater {
    fn state(&self) -> UpdaterState;
    fn set_automatic_checks(&self, enabled: bool);
    fn check_now(&self);
    /// While `true`, update checks/installs and relaunch are postponed (removal in progress).
    fn set_blocked(&self, blocked: bool);
}
