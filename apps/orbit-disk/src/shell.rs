//! Creates the platform UI shell (`orbit_native::NativeShell`).

use orbit_native::NativeShell;

/// The shell for this platform. Must be called on the main thread (inside `Application::run`).
#[cfg(target_os = "macos")]
pub fn create() -> NativeShell {
    NativeShell::on_main_thread().expect("the UI shell must be created on the main thread")
}

#[cfg(not(target_os = "macos"))]
pub fn create() -> NativeShell {
    NativeShell
}
