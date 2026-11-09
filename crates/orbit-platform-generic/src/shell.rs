//! Minimal [`Shell`]: no updater, no OS motion preference, no legacy settings.

use orbit_platform::{LegacySettings, RelaunchHandle, Shell, Updater};

#[derive(Debug, Default)]
pub struct GenericShell;

impl Shell for GenericShell {
    fn reduce_motion(&self) -> bool {
        false
    }

    fn privacy_settings_url(&self) -> Option<&'static str> {
        None
    }

    /// A POSIX `sh` helper waits for this process to exit, then starts the same executable.
    fn relaunch(&self) -> std::io::Result<RelaunchHandle> {
        let executable = std::env::current_exe()?;
        let helper = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; exec \"$2\"")
            .arg("orbit-relaunch")
            .arg(std::process::id().to_string())
            .arg(executable)
            .stdin(std::process::Stdio::null())
            .spawn()?;
        Ok(RelaunchHandle::new(Some(helper)))
    }

    fn updater(&self) -> Option<&dyn Updater> {
        None
    }

    fn import_legacy_settings(&self) -> Option<LegacySettings> {
        None
    }
}
