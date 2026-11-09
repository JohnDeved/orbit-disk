//! The pinned Sparkle release: download once, verify, extract into a fresh directory.
//!
//! `SHA256` was computed with `shasum -a 256 Sparkle-2.10.1.tar.xz` on the downloaded release
//! asset and cross-checked against the `digest` GitHub publishes for that asset:
//! `gh api repos/sparkle-project/Sparkle/releases/tags/2.10.1 --jq '.assets[] | [.name, .digest]'`.
//! When bumping Sparkle, update all three constants together (and `Package.swift` while the Swift
//! app still ships).

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::{Result, exec};

pub const VERSION: &str = "2.10.1";
const URL: &str = "https://github.com/sparkle-project/Sparkle/releases/download/2.10.1/Sparkle-2.10.1.tar.xz";
const SHA256: &str = "1a856d2a9edb8e628de88121d7be04cf0e2167f8641650b78fca00b14244cf27";

/// The extracted release: `Sparkle.framework` and its `LICENSE`.
pub struct Release {
    pub framework: PathBuf,
    pub license: PathBuf,
}

/// Returns the verified archive, downloading it into `cache_dir` when missing or corrupt.
fn archive(cache_dir: &Path) -> Result<PathBuf> {
    let archive = cache_dir.join(format!("Sparkle-{VERSION}.tar.xz"));
    if archive.is_file() {
        if sha256(&archive)? == SHA256 {
            return Ok(archive);
        }
        eprintln!("xtask: cached {} does not match the pinned hash; downloading again", archive.display());
        fs::remove_file(&archive).map_err(|error| format!("could not remove {}: {error}", archive.display()))?;
    }
    fs::create_dir_all(cache_dir).map_err(|error| format!("could not create {}: {error}", cache_dir.display()))?;
    let partial = cache_dir.join(format!("Sparkle-{VERSION}.tar.xz.partial"));
    eprintln!("xtask: downloading {URL}");
    exec(
        Command::new("curl")
            .args(["--fail", "--location", "--silent", "--show-error", "--proto", "=https", "--retry", "3", "--output"])
            .arg(&partial)
            .arg(URL),
    )?;
    let actual = sha256(&partial)?;
    if actual != SHA256 {
        let _ = fs::remove_file(&partial);
        return Err(format!("Sparkle archive hash mismatch: expected {SHA256}, got {actual}"));
    }
    fs::rename(&partial, &archive).map_err(|error| format!("could not move {}: {error}", partial.display()))?;
    Ok(archive)
}

/// Extracts the framework and license into `destination`, which must not exist yet (it is
/// created empty, so nothing left over from earlier runs can end up in the bundle).
pub fn extract(cache_dir: &Path, destination: &Path) -> Result<Release> {
    let archive = archive(cache_dir)?;
    fs::create_dir(destination).map_err(|error| format!("could not create {}: {error}", destination.display()))?;
    exec(
        Command::new("tar")
            .arg("-xJf")
            .arg(&archive)
            .arg("-C")
            .arg(destination)
            .args(["./Sparkle.framework", "./LICENSE"]),
    )?;
    let release = Release { framework: destination.join("Sparkle.framework"), license: destination.join("LICENSE") };
    let symlinked =
        fs::symlink_metadata(release.framework.join("Versions/Current")).is_ok_and(|meta| meta.is_symlink());
    if !symlinked || !release.license.is_file() {
        return Err(format!("Sparkle {VERSION} archive does not have the expected layout"));
    }
    Ok(release)
}

fn sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer).map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}
