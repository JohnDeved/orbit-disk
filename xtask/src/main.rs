//! Build automation for the Rust port: `cargo xtask <command>`.
//!
//! - `bundle [--universal] [--output PATH.app] [--profile NAME]`: the Rust replacement for
//!   `build-app.sh` (signed `Orbit Disk.app` with Sparkle).
//! - `dmg APP OUTPUT.dmg`: wraps `scripts/package-dmg.sh`.
//! - `bench-compare --rounds N --swift-dir DIR --output DIR`: scaffold only.

mod bench;
mod bundle;
mod sparkle;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

type Result<T, E = String> = std::result::Result<T, E>;

const USAGE: &str = "\
Usage: cargo xtask <command>

Commands:
  bundle [--universal] [--output PATH.app] [--profile NAME]
      Build and sign Orbit Disk.app (default output: ../Orbit Disk.app, profile: release).
  dmg APP OUTPUT.dmg
      Package an existing app into a disk image (scripts/package-dmg.sh).
  bench-compare --rounds N --swift-dir DIR --output DIR
      Compare Rust and Swift v0.9.15 probes (not implemented yet).";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let command = args.next();
    let rest: Vec<OsString> = args.collect();
    let result = match command.as_ref().and_then(|command| command.to_str()) {
        Some("bundle") => bundle::run(&rest),
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
