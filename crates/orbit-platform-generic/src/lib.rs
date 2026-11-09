//! Portable fallback layer. Implements [`orbit_platform::Platform`] with readdir + fstatat.
//! (Implemented in Wave B.)

#[derive(Debug, Default)]
pub struct GenericPlatform;

#[derive(Debug, Default)]
pub struct GenericShell;
