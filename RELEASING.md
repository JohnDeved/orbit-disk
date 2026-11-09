# Releasing Orbit Disk

Push a stable `vX.Y.Z` tag to publish a universal app for macOS 14 or later. The app is built from the Rust workspace (`apps/orbit-disk`, `apps/orbit-control`) by `cargo xtask bundle`. The release workflow runs the checks, builds for Apple silicon and Intel, signs the update archive and feed, then publishes a GitHub release with:

- `Orbit-Disk-X.Y.Z-universal.zip`
- `Orbit-Disk.dmg`
- `appcast.xml`
- `SHA256SUMS`

The disk image contains the app and an Applications shortcut. Its stable name supports direct downloads from `https://github.com/JohnDeved/orbit-disk/releases/latest/download/Orbit-Disk.dmg`. Sparkle uses the versioned ZIP, not the disk image. Assets are uploaded to a draft before it becomes the latest public release. The app reads `https://github.com/JohnDeved/orbit-disk/releases/latest/download/appcast.xml`; users do not need GitHub credentials. Existing releases are never overwritten.

Both workflows share the jobs in `checks.yml`:

- `ubuntu-24.04`: release-metadata tests and the portable crates (engine, app state, automation, xtask) on the generic platform.
- `macos-26`, lint: platform boundary check, `cargo fmt`, `cargo clippy -D warnings`.
- `macos-26`, tests: `cargo nextest run --workspace`, `Tests/test_mcp.py` against the Rust `OrbitControl`, the updater copy overlay against the pinned Sparkle, and benchmark compilation.

The macOS jobs select Xcode 26.6 through `DEVELOPER_DIR`; Rust comes from `rust-toolchain.toml`. App packaging and publishing wait for every check job. The Swift app was removed at the 0.10.0 cutover. The manual **Swift baseline** workflow still builds it, with its benchmark probes, from a tag (default `v0.9.15`) for `cargo xtask bench-compare`.

After publication, the release workflow renders the README screenshots from the released commit (`scripts/capture-marketing.sh`) and commits only the generated images to `main`. Until the app has the deterministic `--marketing-state` mode described in that script, the step is skipped with a warning. Ordinary pushes do not refresh screenshots.

## Version and tag

The version lives in one place: `version` under `[workspace.package]` in `Cargo.toml`. `Config/Info.plist` contains the placeholders `$(ORBIT_SHORT_VERSION)` and `$(ORBIT_BUILD_NUMBER)`; `cargo xtask bundle` fills them in the bundle's copy. Do not write literal versions into `Config/Info.plist`; the bundle step and the validator reject that.

| Workspace version | `CFBundleShortVersionString` | `CFBundleVersion` |
|---|---|---|
| `1.0.0-alpha.0` | `1.0.0-alpha.0` | `1000000` |
| `1.0.0-beta.2` | `1.0.0-beta.2` | `1000032` |
| `1.0.0-rc.1` | `1.0.0-rc.1` | `1000061` |
| `1.0.0` | `1.0.0` | `1000099` |
| `1.0.1` | `1.0.1` | `1000199` |

The build number is `MAJOR × 1,000,000 + MINOR × 10,000 + PATCH × 100 + stage`. The stage is `N` for `alpha.N` (N ≤ 29), `30 + N` for `beta.N` (N ≤ 29), `60 + N` for `rc.N` (N ≤ 38) and `99` for a stable release. MINOR and PATCH are at most 99. Sparkle compares `sparkle:version`, the build number, as an integer, so it orders updates exactly like semver. Every Rust version is above the Swift app's last build number (27, v0.9.15), so Swift users are offered the first Rust release. `cargo xtask version` prints the mapping. `xtask/src/version.rs` and `scripts/validate-release.py` implement it; both test the same vectors, and the release script checks that the built bundle matches.

Only stable versions are released. Tags have exactly three numeric components and must equal `v` + the workspace version.

1. Set `[workspace.package] version` in `Cargo.toml` to the new stable version, for example `1.0.0`, and run `cargo check` so `Cargo.lock` follows.
2. Write the changes since the previous release in `docs/releases/vX.Y.Z.md`. The release script includes these notes and appends the platform and signing details automatically.
3. Run the checks below, review the changes, and commit them. The release script requires a clean working tree.
4. Create an annotated tag matching the version and push it with the commit:

```sh
git tag -a v1.0.0 -m 'Orbit Disk 1.0.0'
git push origin main v1.0.0
```

The tag must point to the checked-out commit, and the remote tag must match before publishing. `scripts/validate-release.py vX.Y.Z --check-tags` checks that the version and build number are newer than every existing tag. For tags made before the Rust port, it reads the literal versions from that tag's `Config/Info.plist`. The workflow uses its built-in GitHub token to create the release.

## Metal shaders

GPUI either embeds a precompiled Metal library or compiles the shader source at first launch (`orbit-ui`'s default `runtime-shaders` feature). Releases always embed the precompiled library. Precompiling needs Xcode's Metal toolchain, which is a separate Xcode 26 component: `xcodebuild -downloadComponent MetalToolchain`. CI installs it if the runner image lacks it.

- `cargo xtask bundle --release-mode` (`./build-app.sh --release-mode`) builds `orbit-disk` without `runtime-shaders`. It fails if `xcrun metal` does not run. It also checks that `OrbitDisk` embeds a metallib. Any build with `CI` set is held to the same rule.
- Local builds without `--release-mode` precompile when the toolchain is present. Without it, they print a warning banner and fall back to runtime shaders. Such bundles are for local testing only.

## Signing key

The repository's `SPARKLE_PRIVATE_KEY` Actions secret is already configured. Its matching public key is in `Config/Info.plist`. The existing private key is stored in the maintainer's macOS login Keychain under Sparkle account `local.orbitdisk.app`.

Keep an encrypted backup of this existing key. Do not generate a replacement or change the public key for an ordinary release: installed apps, including Swift releases, trust the current key. A key rotation needs a separate migration. Never commit, print, or paste the private key into release notes or logs.

Sparkle signatures authenticate the app archive and update feed. They do not replace Apple Developer ID signing or notarization. Without Apple credentials, releases retain an ad-hoc signature and their release notes explain the first-launch approval requirement. Users of 0.8.3 must manually install an updater-enabled version once.

## Local checks and preparation

Use the Rust toolchain from `rust-toolchain.toml` (rustup installs it), `cargo-nextest`, Python 3 and macOS. For a release-mode bundle, use Xcode 26.6 with the Metal toolchain. CI uses Xcode 26.6. Publishing locally also needs an authenticated GitHub CLI with release access.

```sh
./build-app.sh --universal --release-mode
./scripts/checks/check-platform-boundary.sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
python3 -I Tests/test_mcp.py '../Orbit Disk.app/Contents/MacOS/OrbitControl'
python3 scripts/test-release.py
(dir=$(mktemp -d)/sparkle && cargo xtask sparkle "$dir" && ORBIT_SPARKLE_DIR="$dir" python3 Tests/test_update_copy.py)
python3 scripts/validate-release.py v1.0.0 --check-tags
```

`./build-app.sh [--universal] [--release-mode] [--output PATH.app]` wraps `cargo xtask bundle`. The bundle step builds `OrbitDisk` and `OrbitControl` with `--locked` for the macOS 14 deployment target. It downloads the pinned Sparkle 2.10.1 release once and verifies its SHA-256 (`xtask/src/sparkle.rs`). It then merges the English updater copy, signs ad hoc, verifies the result, and only then replaces the output. `cargo xtask sparkle DIR` extracts the same verified release with its `bin/` tools (`generate_appcast`, `sign_update`, `generate_keys`).

### Dry run

`scripts/release.sh vX.Y.Z --dry-run` rehearses the whole pipeline locally without publishing anything. It needs no tag, clean tree, secret or network write; it only reports what a real release would refuse. It accepts the current workspace version even when that is a prerelease, for example `v1.0.0-alpha.0`. The dry run:

- builds the universal bundle (release mode if the Metal toolchain is available, otherwise runtime shaders with a warning) and checks the stamped `Info.plist`
- rehearses Developer ID signing with an ad-hoc identity: hardened runtime, entitlements, and the same verification. Apple credentials are ignored, and nothing is sent to Apple.
- signs the archive and feed with a throwaway EdDSA key. Its public key replaces `SUPublicEDKey` in the dry-run bundle only, so these assets are not publishable.
- creates and verifies the ZIP, appcast, disk image and `SHA256SUMS` in `target/releases/dry-run/<tag>.<time>`, writes the release notes next to them, and stops before any `gh` call

### Preparing real assets

After committing and creating the local tag, prepare the exact release assets without publishing. This exports the existing Keychain key to a temporary private file, passes it to the release script without printing it, and removes the file afterward:

```sh
(
  set +x
  set -eu
  umask 077
  key_file=$(mktemp)
  tools=$(mktemp -d)/sparkle
  trap 'rm -f "$key_file"' EXIT
  cargo xtask sparkle "$tools" >/dev/null
  "$tools/bin/generate_keys" --account local.orbitdisk.app -x "$key_file"
  SPARKLE_PRIVATE_KEY="$(cat "$key_file")" \
    ./scripts/release.sh v1.0.0 --prepare-only
)
```

Verified assets are written to `target/releases/v1.0.0`, with the release notes in `target/releases/v1.0.0.release-notes.md`. The script:

- checks the version and build metadata and that the bundle's `Info.plist` is exactly the stamped `Config/Info.plist`
- checks the archive URL and size, the signatures and the bundle integrity
- creates the disk image after the update feed, so the feed contains only the ZIP
- verifies the image and mounts it read-only, then checks the app's code signature and compares its files, symlinks and permissions with the original bundle before adding it to the release

The Build workflow also tests that a change to executable permissions is rejected. The script refuses to overwrite an existing preparation directory. `--prepare-only` does not create or push a tag, contact GitHub to publish, or run the test suite; run the checks above first. Omit that flag only when deliberately publishing from a clean, tagged checkout whose tag is already on GitHub. Releases are normally published by the workflow.

To package an already verified app separately, run `./scripts/package-dmg.sh "PATH/Orbit Disk.app" "PATH/Orbit-Disk.dmg"` (or `cargo xtask dmg …`). The destination must not already exist.

## Optional Apple signing and notarization

Set all six repository Actions secrets to enable Developer ID signing and notarization:

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE_P12_BASE64` | Base64-encoded Developer ID Application certificate and private key exported as a `.p12` file |
| `APPLE_CERTIFICATE_PASSWORD` | Password for that `.p12` file |
| `APPLE_SIGNING_IDENTITY` | Full identity beginning with `Developer ID Application:` |
| `APPLE_API_KEY_P8` | Contents of the App Store Connect API private key `.p8` file |
| `APPLE_API_KEY_ID` | ID of that API key |
| `APPLE_API_ISSUER` | Issuer UUID for that API key |

If none are set, the release remains ad-hoc signed. An incomplete set fails validation. The signing script uses a temporary Keychain. It signs Sparkle's helpers and framework (keeping their own entitlements), then `Contents/MacOS/OrbitControl` and the app. The app signature covers `Contents/MacOS/OrbitDisk`. All are signed with the hardened runtime and a secure timestamp. It checks that both executables carry the runtime flag and exactly the entitlements in `Config/OrbitDisk.entitlements`, then waits for Apple to accept notarization and staples and verifies the ticket before packaging. Keep these values in Actions secrets or temporary local environment variables, never in the repository. The Build workflow and the dry run rehearse the same signing with an ad-hoc identity.

The disk image itself is not signed or notarized. Apple recommends signing and notarizing the distributed disk image as well as the app.

### Entitlements

`Config/OrbitDisk.entitlements` is deliberately empty, matching the Swift app, which shipped without entitlements:

- **Metal:** GPUI loads the precompiled metallib with `newLibraryWithData`. Nothing is compiled in-process, so neither `com.apple.security.cs.allow-jit` nor `allow-unsigned-executable-memory` is needed. A runtime-shader build would compile through the system Metal compiler service, which also needs no entitlement, but releases never use it.
- **Sparkle:** the updater loads `Contents/Frameworks/Sparkle.framework` at runtime (`NSBundle` load). It is signed with the same Developer ID, so library validation stays on (no `disable-library-validation`).
- **No App Sandbox, no Apple Events.** Nothing in the app uses `NSAppleScript` or `osascript`. Finder reveal goes through GPUI's `NSWorkspace` call, Trash through `NSFileManager`, and relaunch through `/usr/bin/open`. Access to protected folders is governed by TCC (Full Disk Access), not by entitlements.
- **`OrbitControl`** talks to the app over a Unix socket in the per-user temporary directory and needs nothing extra.

Add an entitlement only with a reason recorded here. A real Developer ID run is still the final proof: an ad-hoc rehearsal cannot exercise team-ID library validation or notarization.

## Benchmarks against the Swift app

`cargo xtask bench-compare --output NEW-DIR [--rounds N] [--small] [--suites indexing,marking,pipeline,discovery,e2e] [--swift-dir DIR]` builds the `orbit-bench` probes and runs them against the Swift v0.9.15 probes (`~/orbit-bench/swift-v0.9.15-{indexing,marking,pipeline}` by default; the Swift baseline workflow builds them). Each run is a fresh process, and the order alternates each round. The script writes `results.jsonl`, `summary.md` (median, minimum, maximum and speedup), `environment.json` (machine, commit and load averages) and per-run logs under `raw/`. Discovery and end-to-end sessions have no Swift probe binary, so they show the documented Swift numbers without a speedup. The results are reports, not CI gates; run them on an idle machine.

## Update interface

The app uses Sparkle's standard installer with daily checks and user-approved installation/restart. The English dialog copy is in `Assets/Sparkle.en.json`. Packaging merges it into the staged framework and re-signs the framework; the original Sparkle license and other languages remain included. The merge fails if an expected vendor string key changes, so review this overlay when upgrading Sparkle (`xtask/src/sparkle.rs` pins the version and hash). Skip, dismissal, and install-on-quit actions retain their distinct meanings.
