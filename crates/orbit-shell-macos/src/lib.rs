//! macOS UI shell. Implements [`orbit_platform::Shell`].
//!
//! Everything here must run on the main thread: [`MacShell::new`] takes a
//! [`MainThreadMarker`] and the shell holds Objective-C objects that are neither `Send` nor
//! `Sync`.
#![cfg(target_os = "macos")]

mod legacy;
mod updater;

use std::ffi::OsString;
use std::process::{Command, Stdio};

use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::NSBundle;
use orbit_platform::{LegacySettings, Shell, Updater};

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

    /// Starts Sparkle's scheduler. Idempotent; a no-op without an updater.
    pub fn start_updater(&self) {
        if let Some(updater) = &self.updater {
            updater.start();
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

    fn relaunch(&self) -> std::io::Result<()> {
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
            .map(drop)
    }

    fn updater(&self) -> Option<&dyn Updater> {
        self.updater.as_ref().map(|updater| updater as &dyn Updater)
    }

    fn import_legacy_settings(&self) -> Option<LegacySettings> {
        legacy::read(LEGACY_DEFAULTS_DOMAIN)
    }
}
