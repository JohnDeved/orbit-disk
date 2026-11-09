//! macOS UI shell. Implements [`orbit_platform::Shell`].
//!
//! Everything here must run on the main thread: [`MacShell::new`] takes a
//! [`MainThreadMarker`] and the shell holds Objective-C objects that are neither `Send` nor
//! `Sync`.
#![cfg(target_os = "macos")]

mod color;
mod legacy;
mod updater;

use std::ffi::OsString;
use std::process::{Command, Stdio};

use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::NSBundle;
use orbit_platform::{LegacySettings, RelaunchHandle, Shell, Updater};

pub use legacy::LEGACY_DEFAULTS_DOMAIN;
pub use updater::SparkleUpdater;

/// Full Disk Access pane of System Settings (same deep link the Swift app opens).
pub const PRIVACY_SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

pub struct MacShell {
    updater: Option<SparkleUpdater>,
}

impl std::fmt::Debug for MacShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacShell").field("updater", &self.updater.is_some()).finish()
    }
}

impl MacShell {
    /// Creates the shell. When the running executable sits inside an app bundle that ships
    /// `Contents/Frameworks/Sparkle.framework`, Sparkle is loaded and an updater controller is
    /// created but **not started**; call [`MacShell::start_updater`] once the main window is on
    /// screen (the Swift app starts it from the window's `onAppear`). Unbundled development runs
    /// have no updater.
    pub fn new(mtm: MainThreadMarker) -> Self {
        Self { updater: SparkleUpdater::load(mtm) }
    }

    /// Creates the shell if called on the main thread; `None` otherwise. Lets callers that must
    /// not name Objective-C types construct the shell safely.
    pub fn on_main_thread() -> Option<Self> {
        MainThreadMarker::new().map(Self::new)
    }

    /// Starts Sparkle's scheduler. Idempotent; a no-op without an updater.
    pub fn start_updater(&self) {
        if let Some(updater) = &self.updater {
            orbit_platform::Updater::start(updater);
        }
    }

    /// The concrete updater, for state the portable [`Updater`] trait does not expose
    /// (for example [`SparkleUpdater::is_waiting_for_removal`]).
    pub fn sparkle(&self) -> Option<&SparkleUpdater> {
        self.updater.as_ref()
    }
}

impl Shell for MacShell {
    fn reduce_motion(&self) -> bool {
        NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }

    fn privacy_settings_url(&self) -> Option<&'static str> {
        Some(PRIVACY_SETTINGS_URL)
    }

    fn relaunch(&self) -> std::io::Result<RelaunchHandle> {
        let bundle = NSBundle::mainBundle().bundlePath().to_string();
        // Like the Swift app: a detached helper waits until every thread of this process has
        // exited, then starts a fresh instance. Paths are positional arguments, never spliced
        // into the script. Unbundled development runs re-execute the binary instead.
        const WAIT_THEN: &str = "while kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; exec";
        let (script, target): (String, OsString) = if bundle.ends_with(".app") {
            (format!("{WAIT_THEN} /usr/bin/open -n \"$2\""), bundle.into())
        } else {
            (format!("{WAIT_THEN} \"$2\""), std::env::current_exe()?.into_os_string())
        };
        Command::new("/bin/sh")
            .args(["-c", &script, "orbit-relaunch", &std::process::id().to_string()])
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|helper| RelaunchHandle::new(Some(helper)))
    }

    fn updater(&self) -> Option<&dyn Updater> {
        self.updater.as_ref().map(|updater| updater as &dyn Updater)
    }

    fn prepare_window(&self, window: raw_window_handle::RawWindowHandle) {
        if let raw_window_handle::RawWindowHandle::AppKit(handle) = window {
            // SAFETY: the handle comes from a live GPUI window on the main thread.
            unsafe { color::tag_layer_srgb(handle.ns_view.as_ptr()) };
        }
    }

    fn import_legacy_settings(&self) -> Option<LegacySettings> {
        legacy::read(LEGACY_DEFAULTS_DOMAIN)
    }
}
