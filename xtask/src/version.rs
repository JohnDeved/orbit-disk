//! Bundle version stamping: the workspace version (`[workspace.package] version`) is the single
//! source of the app's `CFBundleShortVersionString` and `CFBundleVersion`.
//!
//! `Config/Info.plist` holds two placeholders that only the bundle step fills in:
//! `$(ORBIT_SHORT_VERSION)` and `$(ORBIT_BUILD_NUMBER)`.
//!
//! Mapping (mirrored by `scripts/validate-release.py`, which checks the stamped app at release
//! time; both test the same vectors):
//!
//! - short version: `MAJOR.MINOR.PATCH` for a stable release, the full semver string
//!   (`1.0.0-alpha.0`) for a prerelease. Releases require a stable version; the tag must be
//!   `v` + the short version.
//! - build number: `MAJOR * 1_000_000 + MINOR * 10_000 + PATCH * 100 + STAGE`, where STAGE is
//!   `alpha.N` → N (0–29), `beta.N` → 30 + N (0–29), `rc.N` → 60 + N (0–38) and stable → 99.
//!   `MINOR` and `PATCH` are at most 99. Every later semver gets a larger integer, so Sparkle's
//!   numeric `sparkle:version` comparison orders updates like semver, and 1.0.0-alpha.0
//!   (1,000,000) is far above the Swift app's last build number (27).

use crate::Result;

pub const SHORT_VERSION_PLACEHOLDER: &str = "$(ORBIT_SHORT_VERSION)";
pub const BUILD_NUMBER_PLACEHOLDER: &str = "$(ORBIT_BUILD_NUMBER)";

/// The workspace version this xtask was compiled with (cargo rebuilds it when it changes).
pub const WORKSPACE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleVersion {
    pub short: String,
    pub build: u64,
    pub prerelease: bool,
}

pub fn map(version: &str) -> Result<BundleVersion> {
    let invalid = |why: &str| format!("workspace version {version:?} cannot be bundled: {why}");
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    if core.contains('+') || pre.is_some_and(|pre| pre.contains('+')) {
        return Err(invalid("build metadata (+…) is not supported"));
    }
    let parts: Vec<&str> = core.split('.').collect();
    let number = |text: &str| -> Option<u64> {
        let canonical = text == "0" || (!text.is_empty() && !text.starts_with('0'));
        if canonical && text.bytes().all(|b| b.is_ascii_digit()) && text.len() <= 6 { text.parse().ok() } else { None }
    };
    let [major, minor, patch] = parts[..] else {
        return Err(invalid("expected MAJOR.MINOR.PATCH"));
    };
    let (Some(major), Some(minor), Some(patch)) = (number(major), number(minor), number(patch)) else {
        return Err(invalid("expected MAJOR.MINOR.PATCH with canonical integers"));
    };
    if minor > 99 || patch > 99 {
        return Err(invalid("MINOR and PATCH must be at most 99"));
    }
    if major == 0 && minor == 0 && patch == 0 {
        return Err(invalid("0.0.0 has no build number"));
    }
    let stage = match pre {
        None => 99,
        Some(pre) => {
            let Some((label, n)) = pre.split_once('.') else {
                return Err(invalid("prerelease must be alpha.N, beta.N or rc.N"));
            };
            let n = number(n).ok_or_else(|| invalid("prerelease number must be an integer"))?;
            let (base, limit) = match label {
                "alpha" => (0, 29),
                "beta" => (30, 29),
                "rc" => (60, 38),
                _ => return Err(invalid("prerelease must be alpha.N, beta.N or rc.N")),
            };
            if n > limit {
                return Err(invalid(&format!("{label} number must be at most {limit}")));
            }
            base + n
        }
    };
    let short = match pre {
        None => format!("{major}.{minor}.{patch}"),
        Some(pre) => format!("{major}.{minor}.{patch}-{pre}"),
    };
    Ok(BundleVersion {
        short,
        build: major * 1_000_000 + minor * 10_000 + patch * 100 + stage,
        prerelease: pre.is_some(),
    })
}

/// Fills the two placeholders of `Config/Info.plist`; each must appear exactly once, so a
/// hand-edited literal version can never be silently overwritten or left stale.
pub fn stamp(template: &str, version: &BundleVersion) -> Result<String> {
    for placeholder in [SHORT_VERSION_PLACEHOLDER, BUILD_NUMBER_PLACEHOLDER] {
        let count = template.matches(placeholder).count();
        if count != 1 {
            return Err(format!(
                "Config/Info.plist must contain {placeholder} exactly once (found {count}); the version comes from \
                 [workspace.package] version in Cargo.toml"
            ));
        }
    }
    Ok(template
        .replace(SHORT_VERSION_PLACEHOLDER, &version.short)
        .replace(BUILD_NUMBER_PLACEHOLDER, &version.build.to_string()))
}

/// `cargo xtask version [--json]`: prints the mapping for the current workspace version.
pub fn run(args: &[std::ffi::OsString]) -> Result<()> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err("Usage: cargo xtask version [--json]".into()),
    };
    let version = map(WORKSPACE_VERSION)?;
    if json {
        println!(
            "{{\"workspace\":\"{WORKSPACE_VERSION}\",\"short\":\"{}\",\"build\":{},\"prerelease\":{}}}",
            version.short, version.build, version.prerelease
        );
    } else {
        println!("{} {}", version.short, version.build);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same vectors are in `scripts/test-release.py`.
    const VECTORS: &[(&str, &str, u64)] = &[
        ("1.0.0-alpha.0", "1.0.0-alpha.0", 1_000_000),
        ("1.0.0-alpha.29", "1.0.0-alpha.29", 1_000_029),
        ("1.0.0-beta.2", "1.0.0-beta.2", 1_000_032),
        ("1.0.0-rc.1", "1.0.0-rc.1", 1_000_061),
        ("1.0.0", "1.0.0", 1_000_099),
        ("1.0.1", "1.0.1", 1_000_199),
        ("1.2.3", "1.2.3", 1_020_399),
        ("2.0.0-alpha.0", "2.0.0-alpha.0", 2_000_000),
        ("0.9.15", "0.9.15", 91_599),
    ];

    #[test]
    fn vectors() {
        for &(input, short, build) in VECTORS {
            let mapped = map(input).unwrap();
            assert_eq!((mapped.short.as_str(), mapped.build), (short, build), "{input}");
        }
    }

    #[test]
    fn semver_order_is_build_order() {
        let ordered = [
            "1.0.0-alpha.0",
            "1.0.0-alpha.29",
            "1.0.0-beta.0",
            "1.0.0-rc.38",
            "1.0.0",
            "1.0.1-alpha.0",
            "1.0.1",
            "1.99.99",
            "2.0.0-alpha.0",
        ];
        let builds: Vec<u64> = ordered.iter().map(|v| map(v).unwrap().build).collect();
        assert!(builds.windows(2).all(|pair| pair[0] < pair[1]), "{builds:?}");
        assert!(builds[0] > 27, "must stay above the Swift app's last build number");
    }

    #[test]
    fn rejects_unmappable_versions() {
        for bad in [
            "1.0",
            "1.0.0.0",
            "01.0.0",
            "1.100.0",
            "1.0.100",
            "1.0.0-dev",
            "1.0.0-alpha",
            "1.0.0-alpha.1.2",
            "1.0.0-alpha.30",
            "1.0.0-rc.39",
            "1.0.0+5",
            "0.0.0",
            "1.0.0-beta.x",
        ] {
            assert!(map(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn workspace_version_maps() {
        map(WORKSPACE_VERSION).unwrap();
    }

    #[test]
    fn stamp_requires_each_placeholder_once() {
        let version = map("1.0.0").unwrap();
        let template = format!("<s>{SHORT_VERSION_PLACEHOLDER}</s><b>{BUILD_NUMBER_PLACEHOLDER}</b>");
        assert_eq!(stamp(&template, &version).unwrap(), "<s>1.0.0</s><b>1000099</b>");
        assert!(stamp("<s>1.0.0</s>", &version).is_err());
        assert!(stamp(&format!("{template}{BUILD_NUMBER_PLACEHOLDER}"), &version).is_err());
    }

    #[test]
    fn config_template_stamps() {
        let template = std::fs::read_to_string(crate::workspace_root().join("Config/Info.plist")).unwrap();
        let stamped = stamp(&template, &map(WORKSPACE_VERSION).unwrap()).unwrap();
        assert!(!stamped.contains("$(ORBIT_"));
    }
}
