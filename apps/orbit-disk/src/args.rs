//! Command-line arguments.

use std::ffi::OsString;
use std::path::PathBuf;

use orbit_ui::harness::HarnessScript;

#[derive(Debug, Default)]
pub struct Args {
    /// `--fixture`: run against the generated in-memory backend.
    pub fixture: bool,
    /// `--fixture-entries N`: size of the generated tree (default 200,000).
    pub fixture_entries: Option<usize>,
    /// `--scan <path>`: open this location instead of the last one.
    pub scan: Option<Vec<u8>>,
    /// `--frame-harness [script]`: run the frame harness (built-in scenario without a script).
    pub harness: Option<HarnessScript>,
}

const USAGE: &str = "usage: OrbitDisk [--fixture] [--fixture-entries N] [--scan PATH] [--frame-harness [SCRIPT]]";

impl Args {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut parsed = Self::default();
        let mut args = args.into_iter().peekable();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--fixture") => parsed.fixture = true,
                Some("--fixture-entries") => {
                    let value = args.next().and_then(|v| v.to_str()?.parse().ok());
                    parsed.fixture_entries = Some(value.ok_or("--fixture-entries needs a number")?);
                }
                Some("--scan") => {
                    let path = args.next().ok_or("--scan needs a path")?;
                    parsed.scan = Some(orbit_ui::host::path_bytes(
                        &std::path::absolute(PathBuf::from(path)).map_err(|e| e.to_string())?,
                    ));
                }
                Some("--frame-harness") => {
                    let script = match args.peek().and_then(|next| next.to_str()) {
                        Some(next) if !next.starts_with("--") => {
                            let path = PathBuf::from(args.next().unwrap_or_default());
                            let text = std::fs::read_to_string(&path)
                                .map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
                            HarnessScript::parse(&text)?
                        }
                        _ => HarnessScript::builtin(),
                    };
                    parsed.harness = Some(script);
                    parsed.fixture = true;
                }
                // macOS adds `-psn_…` when launched from Finder on old systems; ignore it.
                Some(other) if other.starts_with("-psn_") => {}
                Some("--help" | "-h") => return Err(USAGE.into()),
                _ => return Err(format!("unknown argument {arg:?}\n{USAGE}")),
            }
        }
        Ok(parsed)
    }
}
