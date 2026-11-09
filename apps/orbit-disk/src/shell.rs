//! Creates the platform UI shell (`orbit_native::NativeShell`).

use orbit_native::NativeShell;

/// The shell for this platform. Call on the main thread (inside `Application::run`).
#[cfg(target_os = "macos")]
#[allow(clippy::missing_transmute_annotations)] // the target type is the one we may not name
pub fn create() -> NativeShell {
    // `MacShell::new` takes a main-thread token from the Objective-C bindings, which this crate
    // may not name (scripts/checks/check-platform-boundary.sh). The token is a zero-sized marker.
    // SAFETY: called from the GPUI `Application::run` callback, i.e. on the main thread, which is
    // the only invariant the marker encodes. Stopgap until the shell crate offers a constructor
    // that checks the thread itself (reported to the integrator).
    let main_thread = unsafe { std::mem::transmute::<(), _>(()) };
    NativeShell::new(main_thread)
}

#[cfg(not(target_os = "macos"))]
pub fn create() -> NativeShell {
    NativeShell
}
