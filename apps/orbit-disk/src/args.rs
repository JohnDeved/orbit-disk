//! Command-line arguments.

use std::ffi::OsString;
use std::path::PathBuf;

use orbit_ui::harness::HarnessScript;
use orbit_ui::marketing::MarketingState;

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
    /// `--marketing-state STATE`: show a README screenshot state on the sample fixture.
    pub marketing: Option<MarketingState>,
    /// `--window-size WxH`: the main window's content size in points (fixture runs only).
    pub window_size: Option<(f32, f32)>,
}

const USAGE: &str = "usage: OrbitDisk [--fixture] [--fixture-entries N] [--scan PATH] [--frame-harness [SCRIPT]]
                 [--marketing-state overview|downloads|collection|hero|projects|projects-open|review|trash-confirm|permanent-delete] [--appearance dark] [--window-size WxH]

  --marketing-state STATE  show a README screenshot state on the sample Home (implies --fixture,
                           in-memory settings, reduced motion); prints `marketing-state: ready` on
                           stderr once it is on screen
  --appearance dark        the window appearance (fixture runs only; the interface is dark-only)
  --window-size WxH        content size in points, at least 1060x680 (fixture runs only)";

impl Args {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut parsed = Self::default();
        let mut appearance = false;
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
                Some("--marketing-state") => {
                    let value = args.next();
                    let state = value.as_deref().and_then(|v| MarketingState::parse(v.to_str()?));
                    parsed.marketing = Some(state.ok_or_else(|| {
                        format!("--marketing-state needs one of overview, downloads, collection, hero, projects, projects-open, review, trash-confirm, permanent-delete\n{USAGE}")
                    })?);
                    parsed.fixture = true;
                }
                Some("--appearance") => match args.next().as_deref().and_then(|v| v.to_str()) {
                    Some("dark") => appearance = true,
                    Some("light") => {
                        return Err("--appearance light: Orbit Disk has a dark interface only (DESIGN.md); the \
                                    light README art frames dark captures"
                            .into());
                    }
                    _ => return Err(format!("--appearance needs dark or light\n{USAGE}")),
                },
                Some("--window-size") => {
                    let value = args.next();
                    let size = value.as_deref().and_then(|v| parse_size(v.to_str()?));
                    parsed.window_size = Some(size.ok_or_else(|| {
                        let (w, h) = orbit_ui::theme::WINDOW_MIN;
                        format!("--window-size needs WIDTHxHEIGHT in points, at least {w}x{h}")
                    })?);
                }
                // macOS adds `-psn_…` when launched from Finder on old systems; ignore it.
                Some(other) if other.starts_with("-psn_") => {}
                Some("--help" | "-h") => return Err(USAGE.into()),
                _ => return Err(format!("unknown argument {arg:?}\n{USAGE}")),
            }
        }
        if (appearance || parsed.window_size.is_some()) && !parsed.fixture {
            return Err("--appearance and --window-size are fixture-only: add --fixture or --marketing-state".into());
        }
        if parsed.marketing.is_some() {
            let conflict = [
                (parsed.scan.is_some(), "--scan"),
                (parsed.harness.is_some(), "--frame-harness"),
                (parsed.fixture_entries.is_some(), "--fixture-entries"),
            ];
            if let Some((_, flag)) = conflict.into_iter().find(|(set, _)| *set) {
                return Err(format!("--marketing-state shows the fixed sample Home and can't be combined with {flag}"));
            }
        }
        Ok(parsed)
    }
}

/// `1280x810` → `(1280., 810.)`, at least the window's minimum size.
fn parse_size(text: &str) -> Option<(f32, f32)> {
    let (w, h) = text.split_once(['x', 'X', '×'])?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    let (min_w, min_h) = orbit_ui::theme::WINDOW_MIN;
    let size = (w as f32, h as f32);
    (size.0 >= min_w && size.1 >= min_h && w <= 16_384 && h <= 16_384).then_some(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn marketing_state_implies_the_fixture_and_parses_the_capture_flags() {
        let args = parse(&["--appearance", "dark", "--window-size", "1280x810", "--marketing-state", "review"])
            .expect("the capture script's arguments");
        assert!(args.fixture);
        assert_eq!(args.marketing, Some(MarketingState::Review));
        assert_eq!(args.window_size, Some((1280., 810.)));
        for state in MarketingState::ALL {
            assert_eq!(parse(&["--marketing-state", state.name()]).unwrap().marketing, Some(state));
        }
    }

    #[test]
    fn invalid_or_conflicting_capture_flags_are_rejected() {
        for bad in [
            &["--marketing-state"][..],
            &["--marketing-state", "home"],
            &["--marketing-state", "overview", "--scan", "/tmp"],
            &["--marketing-state", "overview", "--fixture-entries", "10"],
            &["--marketing-state", "overview", "--frame-harness"],
            &["--fixture", "--appearance", "light"],
            &["--fixture", "--appearance"],
            &["--appearance", "dark"],
            &["--window-size", "1280x810"],
            &["--fixture", "--window-size", "1280"],
            &["--fixture", "--window-size", "800x600"],
            &["--fixture", "--window-size", "-1280x810"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn help_lists_the_marketing_flags() {
        // scripts/capture-marketing.sh checks `--help` for `--marketing-state`.
        let help = parse(&["--help"]).unwrap_err();
        assert!(help.contains("--marketing-state") && help.contains("--window-size") && help.contains("--appearance"));
    }
}
