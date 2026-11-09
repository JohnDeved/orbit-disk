//! `cargo xtask bench-compare`: interleaved Rust vs. Swift v0.9.15 comparisons (scaffold).
//!
//! TODO(bench-compare): build the `orbit-bench` probes in release, then for each of `rounds`
//! rounds run every probe pair (Rust, Swift from `swift_dir`) in fresh processes, alternating
//! which side goes first, and write per-probe medians plus raw samples as `.md` and `.json`
//! under `output` (see `docs/rust-port/README.md`, "Performance"). Reports only; no timing gates.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::{Args, Result};

#[derive(Debug)]
struct Options {
    rounds: u32,
    swift_dir: PathBuf,
    output: PathBuf,
}

fn parse(args: &[OsString]) -> Result<Options> {
    const USAGE: &str = "Usage: cargo xtask bench-compare --rounds N --swift-dir DIR --output DIR";
    let (mut rounds, mut swift_dir, mut output) = (None, None, None);
    let mut args = Args::new(args);
    while let Some(arg) = args.next() {
        match arg? {
            "--rounds" => {
                let value = args.value("--rounds")?;
                let parsed = value.to_str().and_then(|value| value.parse::<u32>().ok()).filter(|&value| value > 0);
                rounds = Some(parsed.ok_or_else(|| format!("--rounds needs a positive integer, got {value:?}"))?);
            }
            "--swift-dir" => swift_dir = Some(PathBuf::from(args.value("--swift-dir")?)),
            "--output" => output = Some(PathBuf::from(args.value("--output")?)),
            other => return Err(format!("unexpected argument {other:?}\n{USAGE}")),
        }
    }
    Ok(Options {
        rounds: rounds.ok_or(format!("--rounds is required\n{USAGE}"))?,
        swift_dir: swift_dir.ok_or(format!("--swift-dir is required\n{USAGE}"))?,
        output: output.ok_or(format!("--output is required\n{USAGE}"))?,
    })
}

pub fn run(args: &[OsString]) -> Result<()> {
    let options = parse(args)?;
    println!(
        "bench-compare: {} rounds, Swift probes in {}, reports to {}",
        options.rounds,
        options.swift_dir.display(),
        options.output.display()
    );
    println!("bench-compare: probes not yet implemented");
    Ok(())
}
