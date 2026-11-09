//! `cargo xtask bundle`: the Rust port of `build-app.sh`.
//!
//! Builds `OrbitDisk` and `OrbitControl`, assembles `Orbit Disk.app` with the pinned Sparkle
//! framework in a private staging directory, signs it ad hoc, verifies the signature, and only
//! then swaps it into place (restoring the previous app if the swap fails).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Args, Result, exec, sparkle, workspace_root};

const BINARIES: [(&str, &str); 2] = [("orbit-disk", "OrbitDisk"), ("orbit-control", "OrbitControl")];

struct Options {
    universal: bool,
    output: Option<PathBuf>,
    profile: String,
}

fn parse(args: &[OsString]) -> Result<Options> {
    let mut options = Options { universal: false, output: None, profile: "release".into() };
    let mut args = Args::new(args);
    while let Some(arg) = args.next() {
        match arg? {
            "--universal" => options.universal = true,
            "--output" => options.output = Some(args.value("--output")?.into()),
            "--profile" => {
                options.profile = args.value("--profile")?.into_string().map_err(|_| "--profile must be UTF-8")?;
            }
            other => {
                return Err(format!(
                    "unexpected argument {other:?}\nUsage: cargo xtask bundle [--universal] [--output PATH.app] [--profile NAME]"
                ));
            }
        }
    }
    Ok(options)
}

pub fn run(args: &[OsString]) -> Result<()> {
    let options = parse(args)?;
    let root = workspace_root();
    let app_path = match options.output {
        Some(path) => std::path::absolute(path).map_err(|error| format!("invalid --output: {error}"))?,
        None => root.parent().ok_or("workspace has no parent directory")?.join("Orbit Disk.app"),
    };
    if app_path.extension().is_none_or(|extension| extension != "app") {
        return Err("Output must end in .app".into());
    }
    let info_plist = root.join("Config/Info.plist");
    exec(Command::new("plutil").arg("-lint").arg(&info_plist))?;

    let architectures: Vec<&str> = if options.universal { vec!["arm64", "x86_64"] } else { vec![host_architecture()?] };
    let target_dir = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let mut built: Vec<[PathBuf; 2]> = Vec::new();
    for architecture in &architectures {
        let triple = rust_target(architecture)?;
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command.current_dir(&root).args(["build", "--profile", &options.profile, "--target", triple]);
        for (package, binary) in BINARIES {
            command.args(["--package", package, "--bin", binary]);
        }
        exec(&mut command)?;
        let dir = target_dir.join(triple).join(profile_dir(&options.profile));
        built.push(BINARIES.map(|(_, binary)| dir.join(binary)));
    }

    let xtask_dir = target_dir.join("xtask");
    fs::create_dir_all(&xtask_dir).map_err(|error| format!("could not create {}: {error}", xtask_dir.display()))?;
    if let Some(parent) = app_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let staging = Staging::new(&xtask_dir)?;
    let release =
        sparkle::extract(&xtask_dir.join(format!("sparkle-{}", sparkle::VERSION)), &staging.0.join("sparkle"))?;

    let bundle = staging.0.join("Orbit Disk.app");
    let contents = bundle.join("Contents");
    for dir in ["MacOS", "Resources", "Frameworks"] {
        fs::create_dir_all(contents.join(dir)).map_err(|error| format!("could not create bundle layout: {error}"))?;
    }
    copy(&info_plist, &contents.join("Info.plist"))?;
    copy(&root.join("Assets/OrbitDisk.icns"), &contents.join("Resources/OrbitDisk.icns"))?;
    copy(&release.license, &contents.join("Resources/Sparkle-LICENSE.txt"))?;
    // Framework version links and helper executable permissions must survive packaging.
    let framework = contents.join("Frameworks/Sparkle.framework");
    exec(Command::new("ditto").arg(&release.framework).arg(&framework))?;
    exec(Command::new("python3").arg(root.join("scripts/customize-sparkle.py")).arg(&framework))?;
    exec(
        Command::new("codesign")
            .args(["--force", "--sign", "-", "--preserve-metadata=identifier,entitlements,flags"])
            .arg(&framework),
    )?;
    for (index, (_, binary)) in BINARIES.iter().enumerate() {
        let destination = contents.join("MacOS").join(binary);
        if options.universal {
            exec(
                Command::new("lipo")
                    .arg("-create")
                    .args(built.iter().map(|paths| &paths[index]))
                    .arg("-output")
                    .arg(&destination),
            )?;
            exec(Command::new("lipo").arg(&destination).args(["-verify_arch", "arm64", "x86_64"]))?;
        } else {
            copy(&built[0][index], &destination)?;
        }
    }
    exec(Command::new("codesign").args(["--force", "--sign", "-"]).arg(contents.join("MacOS/OrbitControl")))?;
    exec(Command::new("codesign").args(["--force", "--sign", "-"]).arg(&bundle))?;
    exec(Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&bundle))?;

    // Swap only after the complete replacement has passed verification.
    let backup = staging.0.join("previous.app");
    if fs::symlink_metadata(&app_path).is_ok() {
        exec(Command::new("mv").arg(&app_path).arg(&backup))?;
    }
    if let Err(error) = exec(Command::new("mv").arg(&bundle).arg(&app_path)) {
        if fs::symlink_metadata(&backup).is_ok() {
            exec(Command::new("mv").arg(&backup).arg(&app_path))?;
        }
        return Err(error);
    }
    println!("Built {}", app_path.display());
    Ok(())
}

/// A private directory under `target/xtask`, removed (with any backup inside it) on drop.
struct Staging(PathBuf);

impl Staging {
    fn new(parent: &Path) -> Result<Self> {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_nanos());
        let path = parent.join(format!("app-bundle.{}.{nanos}", std::process::id()));
        fs::create_dir(&path).map_err(|error| format!("could not create {}: {error}", path.display()))?;
        Ok(Self(path))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to)
        .map(drop)
        .map_err(|error| format!("could not copy {} to {}: {error}", from.display(), to.display()))
}

fn host_architecture() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "aarch64" => Ok("arm64"),
        "x86_64" => Ok("x86_64"),
        other => Err(format!("unsupported host architecture {other}")),
    }
}

fn rust_target(architecture: &str) -> Result<&'static str> {
    match architecture {
        "arm64" => Ok("aarch64-apple-darwin"),
        "x86_64" => Ok("x86_64-apple-darwin"),
        other => Err(format!("unsupported architecture {other}")),
    }
}

/// Cargo's output directory name for a profile.
fn profile_dir(profile: &str) -> &str {
    match profile {
        "dev" | "test" => "debug",
        "bench" => "release",
        other => other,
    }
}
