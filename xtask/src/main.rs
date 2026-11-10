//! Build automation for the Rust port: `cargo xtask <command>`.
//!
//! - `bundle`: builds and signs `Orbit Disk.app` (`build-app.sh` wraps it).
//! - `version`: prints the bundle version mapped from the workspace version.
//! - `sparkle DIR`: extracts the pinned, verified Sparkle release with its tools.
//! - `dmg APP OUTPUT.dmg`: wraps `scripts/package-dmg.sh`.
//! - `bench-compare`: interleaved Rust vs. Swift v0.9.15 probe comparisons.

mod bench;
mod bundle;
mod sparkle;
mod version;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

type Result<T, E = String> = std::result::Result<T, E>;

const USAGE: &str = "\
Usage: cargo xtask <command>

Commands:
  bundle [--universal] [--output PATH.app] [--profile NAME] [--release-mode] [--runtime-shaders]
      Build and sign Orbit Disk.app (default output: ../Orbit Disk.app, profile: release).
      --release-mode requires Xcode's Metal toolchain (precompiled shaders); without it, local
      builds fall back to runtime shader compilation with a warning.
  version [--json]
      Print CFBundleShortVersionString and CFBundleVersion for the workspace version.
  sparkle NEW-DIR
      Extract the pinned Sparkle release (framework, LICENSE, bin/ tools) into NEW-DIR.
  dmg APP OUTPUT.dmg
      Package an existing app into a disk image (scripts/package-dmg.sh).
  bench-compare --output DIR [--rounds N] [--suites LIST] [--small]
      Interleaved fresh-process comparison of the Rust and Swift v0.9.15 probes.";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let command = args.next();
    let rest: Vec<OsString> = args.collect();
    let result = match command.as_ref().and_then(|command| command.to_str()) {
        Some("bundle") => bundle::run(&rest),
        Some("version") => version::run(&rest),
        Some("sparkle") => sparkle::run(&rest),
        Some("dmg") => dmg(&rest),
        Some("bench-compare") => bench::run(&rest),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(format!("unknown or missing command\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root (the parent of this crate).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives inside the workspace").to_path_buf()
}

/// Cargo's target directory (`CARGO_TARGET_DIR` or `<workspace>/target`).
fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| workspace_root().join("target"), PathBuf::from)
}

/// Walks `--flag` / `--name value` arguments; callers reject anything they do not expect.
struct Args<'a> {
    items: std::slice::Iter<'a, OsString>,
}

impl<'a> Args<'a> {
    fn new(args: &'a [OsString]) -> Self {
        Self { items: args.iter() }
    }

    fn next(&mut self) -> Option<Result<&'a str>> {
        let item = self.items.next()?;
        Some(item.to_str().ok_or_else(|| format!("argument is not valid UTF-8: {item:?}")))
    }

    fn value(&mut self, flag: &str) -> Result<OsString> {
        self.items.next().cloned().ok_or_else(|| format!("{flag} needs a value"))
    }
}

/// Runs a command to completion and fails unless it exits successfully.
fn exec(command: &mut Command) -> Result<()> {
    let status = command.status().map_err(|error| format!("could not start {command:?}: {error}"))?;
    if status.success() { Ok(()) } else { Err(format!("{command:?} failed with {status}")) }
}

fn dmg(args: &[OsString]) -> Result<()> {
    exec(Command::new(workspace_root().join("scripts/package-dmg.sh")).args(args))
}
