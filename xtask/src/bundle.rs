//! `cargo xtask bundle`: builds `Orbit Disk.app` (what `build-app.sh` wraps).
//!
//! Builds `OrbitDisk` and `OrbitControl`, assembles the bundle with the pinned Sparkle framework
//! in a private staging directory, stamps the workspace version into a copy of
//! `Config/Info.plist`, signs it ad hoc, verifies the signature, and only then swaps it into
//! place (restoring the previous app if the swap fails).
//!
//! Metal shaders: GPUI either embeds a precompiled `metallib` (needs Xcode's Metal toolchain at
//! build time) or compiles the shader source at launch (`orbit-ui/runtime-shaders`). Bundles use
//! precompiled shaders whenever the toolchain is present. `--release-mode` (and any build with
//! `CI` set) requires it; other local builds fall back to runtime shaders with a warning.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Args, Result, exec, sparkle, target_dir, version, workspace_root};

const BINARIES: [(&str, &str); 2] = [("orbit-disk", "OrbitDisk"), ("orbit-control", "OrbitControl")];

const USAGE: &str =
    "Usage: cargo xtask bundle [--universal] [--output PATH.app] [--profile NAME] [--release-mode] [--runtime-shaders]";

struct Options {
    universal: bool,
    output: Option<PathBuf>,
    profile: String,
    release_mode: bool,
    runtime_shaders: bool,
}

fn parse(args: &[OsString]) -> Result<Options> {
    let mut options = Options {
        universal: false,
        output: None,
        profile: "release".into(),
        release_mode: false,
        runtime_shaders: false,
    };
    let mut args = Args::new(args);
    while let Some(arg) = args.next() {
        match arg? {
            "--universal" => options.universal = true,
            "--output" => options.output = Some(args.value("--output")?.into()),
            "--profile" => {
                options.profile = args.value("--profile")?.into_string().map_err(|_| "--profile must be UTF-8")?;
            }
            "--release-mode" => options.release_mode = true,
            "--runtime-shaders" => options.runtime_shaders = true,
            other => return Err(format!("unexpected argument {other:?}\n{USAGE}")),
        }
    }
    if options.release_mode && (options.runtime_shaders || options.profile != "release") {
        return Err("--release-mode builds the release profile with precompiled shaders only".into());
    }
    Ok(options)
}

/// Whether the build embeds precompiled Metal shaders.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shaders {
    Precompiled,
    Runtime,
}

/// `xcrun metal` exists as a shim even without the toolchain; only running it proves the
/// compiler (a separate Xcode 26 component) is installed.
fn metal_toolchain() -> Result<(), String> {
    let run = |args: &[&str]| -> Result<(), String> {
        let output =
            Command::new("xcrun").args(args).output().map_err(|error| format!("could not run xcrun: {error}"))?;
        if output.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&output.stderr).trim().to_string()) }
    };
    run(&["-sdk", "macosx", "metal", "--version"])?;
    run(&["-sdk", "macosx", "--find", "metallib"])
}

fn choose_shaders(options: &Options) -> Result<Shaders> {
    let strict = options.release_mode || std::env::var_os("CI").is_some_and(|value| !value.is_empty());
    if options.runtime_shaders {
        if strict {
            return Err("--runtime-shaders is not allowed in release mode or CI".into());
        }
        return Ok(Shaders::Runtime);
    }
    match metal_toolchain() {
        Ok(()) => Ok(Shaders::Precompiled),
        Err(reason) if strict => Err(format!(
            "release bundles need precompiled Metal shaders, but Xcode's Metal toolchain is unavailable: {reason}\n\
             Select a full Xcode (DEVELOPER_DIR or xcode-select) and install the toolchain with \
             `xcodebuild -downloadComponent MetalToolchain`."
        )),
        Err(reason) => {
            let rule = "!".repeat(78);
            eprintln!(
                "\n{rule}\n\
                 xtask: WARNING: Xcode's Metal toolchain is unavailable ({reason}).\n\
                 xtask: WARNING: building with RUNTIME shader compilation (orbit-ui/runtime-shaders).\n\
                 xtask: WARNING: this bundle is for local testing only; releases and CI use\n\
                 xtask: WARNING: `--release-mode`, which refuses to build without precompiled shaders.\n\
                 {rule}\n"
            );
            Ok(Shaders::Runtime)
        }
    }
}

/// `plutil -extract KEY raw` for a key of the (stamped) Info.plist.
fn plist_value(path: &Path, key: &str) -> Result<String> {
    let output = Command::new("plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(path)
        .output()
        .map_err(|error| format!("could not run plutil: {error}"))?;
    if !output.status.success() {
        return Err(format!("{} has no {key}", path.display()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn run(args: &[OsString]) -> Result<()> {
    let options = parse(args)?;
    let root = workspace_root();
    let app_path = match &options.output {
        Some(path) => std::path::absolute(path).map_err(|error| format!("invalid --output: {error}"))?,
        None => root.parent().ok_or("workspace has no parent directory")?.join("Orbit Disk.app"),
    };
    if app_path.extension().is_none_or(|extension| extension != "app") {
        return Err("Output must end in .app".into());
    }
    let template_path = root.join("Config/Info.plist");
    exec(Command::new("plutil").arg("-lint").arg(&template_path))?;
    let bundle_version = version::map(version::WORKSPACE_VERSION)?;
    let template = fs::read_to_string(&template_path)
        .map_err(|error| format!("could not read {}: {error}", template_path.display()))?;
    let info = version::stamp(&template, &bundle_version)?;
    let minimum_system = plist_value(&template_path, "LSMinimumSystemVersion")?;
    let shaders = choose_shaders(&options)?;

    let architectures: Vec<&str> = if options.universal { vec!["arm64", "x86_64"] } else { vec![host_architecture()?] };
    let target_dir = target_dir();
    let mut built: Vec<[PathBuf; 2]> = Vec::new();
    for architecture in &architectures {
        let triple = rust_target(architecture)?;
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .current_dir(&root)
            // The binaries must not claim older systems than the bundle (Info.plist) supports.
            .env("MACOSX_DEPLOYMENT_TARGET", &minimum_system)
            .args(["build", "--locked", "--profile", &options.profile, "--target", triple, "--no-default-features"]);
        if shaders == Shaders::Runtime {
            command.args(["--features", "orbit-disk/runtime-shaders"]);
        }
        for (package, binary) in BINARIES {
            command.args(["--package", package, "--bin", binary]);
        }
        exec(&mut command)?;
        let dir = target_dir.join(triple).join(profile_dir(&options.profile));
        let paths = BINARIES.map(|(_, binary)| dir.join(binary));
        if shaders == Shaders::Precompiled {
            require_metallib(&paths[0])?;
        }
        built.push(paths);
    }

    let xtask_dir = target_dir.join("xtask");
    fs::create_dir_all(&xtask_dir).map_err(|error| format!("could not create {}: {error}", xtask_dir.display()))?;
    if let Some(parent) = app_path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let staging = Staging::new(&xtask_dir)?;
    let release = sparkle::extract(&sparkle::cache_dir(&target_dir), &staging.0.join("sparkle"), false)?;

    let bundle = staging.0.join("Orbit Disk.app");
    let contents = bundle.join("Contents");
    for dir in ["MacOS", "Resources", "Frameworks"] {
        fs::create_dir_all(contents.join(dir)).map_err(|error| format!("could not create bundle layout: {error}"))?;
    }
    let info_path = contents.join("Info.plist");
    fs::write(&info_path, info).map_err(|error| format!("could not write {}: {error}", info_path.display()))?;
    exec(Command::new("plutil").arg("-lint").arg(&info_path))?;
    let stamped = (plist_value(&info_path, "CFBundleShortVersionString")?, plist_value(&info_path, "CFBundleVersion")?);
    if stamped != (bundle_version.short.clone(), bundle_version.build.to_string()) {
        return Err(format!("stamped Info.plist has unexpected version {stamped:?}"));
    }
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
    println!(
        "Built {} (version {}, build {}, {}, {} shaders)",
        app_path.display(),
        bundle_version.short,
        bundle_version.build,
        architectures.join(" + "),
        match shaders {
            Shaders::Precompiled => "precompiled",
            Shaders::Runtime => "RUNTIME",
        }
    );
    Ok(())
}

/// A precompiled build embeds the `metallib` (magic `MTLB`); a runtime-shader build embeds the
/// Metal source instead. Guards against a feature leaking in through unification.
fn require_metallib(binary: &Path) -> Result<()> {
    let bytes = fs::read(binary).map_err(|error| format!("could not read {}: {error}", binary.display()))?;
    if bytes.windows(4).any(|window| window == b"MTLB") {
        Ok(())
    } else {
        Err(format!("{} does not embed a precompiled metallib", binary.display()))
    }
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
