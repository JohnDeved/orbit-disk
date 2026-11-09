//! Compile-time platform selection. Apps name `orbit_native::Native`; nothing else should know
//! which OS layer is in use. Add new OS layers here.

#[cfg(target_os = "macos")]
pub use orbit_platform_macos::MacPlatform as Native;
#[cfg(target_os = "macos")]
pub use orbit_shell_macos::MacShell as NativeShell;

#[cfg(not(target_os = "macos"))]
pub use orbit_platform_generic::GenericPlatform as Native;
#[cfg(not(target_os = "macos"))]
pub use orbit_platform_generic::GenericShell as NativeShell;
