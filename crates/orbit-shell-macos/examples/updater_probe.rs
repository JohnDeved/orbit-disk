//! Prints what the macOS shell sees. Copy the binary into `Orbit Disk.app/Contents/MacOS/` and run
//! it from there to confirm Sparkle loads from the bundle; unbundled runs report no updater.
//!
//! The updater is created but not started, so no update check (and no network access) happens.
//! Pass `--start` to also start Sparkle's scheduler; the process exits without running a run loop.

#[cfg(target_os = "macos")]
fn main() {
    use orbit_platform::Shell;
    use orbit_shell_macos::MacShell;

    let mtm = objc2::MainThreadMarker::new().expect("runs on the main thread");
    let shell = MacShell::new(mtm);
    println!("reduce_motion: {}", shell.reduce_motion());
    println!("privacy_settings_url: {:?}", shell.privacy_settings_url());
    println!("legacy_settings: {:?}", shell.import_legacy_settings());
    let Some(updater) = shell.updater() else {
        println!("updater: none (no Contents/Frameworks/Sparkle.framework next to this binary)");
        return;
    };
    println!("updater: loaded, {:?}", updater.state());
    if std::env::args().any(|argument| argument == "--start") {
        shell.start_updater();
        println!("updater: started, {:?}", updater.state());
    }
    updater.set_blocked(true);
    println!("updater: blocked, {:?}", updater.state());
    updater.set_blocked(false);
    println!("updater: unblocked, {:?}", updater.state());
}

#[cfg(not(target_os = "macos"))]
fn main() {}
