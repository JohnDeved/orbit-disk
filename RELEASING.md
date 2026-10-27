# Releasing Orbit Disk

Push a stable `vX.Y.Z` tag to publish a universal app for macOS 14 or later. The release workflow runs the checks, builds for Apple silicon and Intel, signs the update archive and feed, then publishes a GitHub release with:

- `Orbit-Disk-X.Y.Z-universal.zip`
- `Orbit-Disk.dmg`
- `appcast.xml`
- `SHA256SUMS`

The disk image contains the app and an Applications shortcut. Its stable name supports direct downloads from `https://github.com/JohnDeved/orbit-disk/releases/latest/download/Orbit-Disk.dmg`. Sparkle uses the versioned ZIP, not the disk image. Assets are uploaded to a draft before it becomes the latest public release. The app reads `https://github.com/JohnDeved/orbit-disk/releases/latest/download/appcast.xml`; users do not need GitHub credentials. Existing releases are never overwritten.

Both workflows share parallel check jobs. Portable core logic and release metadata checks run on `ubuntu-24.04`; filesystem, app, and rendering integration checks run on `macos-26`. The macOS jobs explicitly select Xcode 26.6 through `DEVELOPER_DIR`. Toolchain versions appear in the job logs. App packaging and publishing wait for every check job to pass. This build environment does not raise the app's macOS 14 deployment target.

After publication, the release workflow renders the README screenshots from the released commit and commits only the generated images to `main`. Ordinary pushes do not refresh screenshots. If the images are unchanged, no screenshot commit is created.

## Version and tag

1. Change `CFBundleShortVersionString` in `Config/Info.plist` to the new version and increase the integer `CFBundleVersion`. Both must be newer than previous release tags. For example, after version `0.9.0`, build `12`, use version `0.9.1`, build `13`.
2. Write the changes since the previous release in `docs/releases/vX.Y.Z.md`. The release script includes these notes and appends the platform and signing details automatically.
3. Run the checks below, review the changes, and commit them. The release script requires a clean working tree.
4. Create an annotated tag matching the version and push it with the commit:

```sh
git tag -a v0.9.1 -m 'Orbit Disk 0.9.1'
git push origin main v0.9.1
```

Tags must have exactly three numeric components; prerelease tags are not supported. The tag must point to the checked-out commit, and the remote tag must match before publishing. The workflow uses its built-in GitHub token to create the release.

## Signing key

The repository's `SPARKLE_PRIVATE_KEY` Actions secret is already configured. Its matching public key is in `Config/Info.plist`. The existing private key is stored in the maintainer's macOS login Keychain under Sparkle account `local.orbitdisk.app`.

Keep an encrypted backup of this existing key. Do not generate a replacement or change the public key for an ordinary release: installed apps trust the current key. A key rotation needs a separate migration. Never commit, print, or paste the private key into release notes or logs.

Sparkle signatures authenticate the app archive and update feed. They do not replace Apple Developer ID signing or notarization. Without Apple credentials, releases retain an ad-hoc signature and their release notes explain the first-launch approval requirement. Users of 0.8.3 must manually install an updater-enabled version once.

## Local checks and preparation

Use Swift 6.3 or later command-line tools or Xcode, Python 3, and macOS. CI uses Swift 6.3.3 from Xcode 26.6. Publishing locally also needs an authenticated GitHub CLI with release access.

`check-portable-core.sh` also runs on Linux. It compiles the production value types, removal projection, path ordering, collection membership and compact filename matcher without the macOS scanner. The session suite builds an optimized app-model test module once and shares it across its test programs. Its largest test harness uses `-Onone` to reduce compilation time; the app code and the timing-sensitive volume checks remain optimized.

```sh
./build-app.sh --universal
./check-portable-core.sh
swift run -c release OrbitCoreChecks
./check-app.sh
./check-updater.sh
./check-rendering.sh
./check-layout.sh
python3 scripts/test-release.py
python3 Tests/test_update_copy.py
python3 scripts/validate-release.py v0.9.1 --check-tags
```

After committing and creating the local tag, prepare the exact release assets without publishing. This exports the existing Keychain key to a temporary private file, passes it to the release script without printing it, and removes the file afterward:

```sh
(
  set +x
  set -eu
  umask 077
  key_file=$(mktemp)
  trap 'rm -f "$key_file"' EXIT
  .build/artifacts/sparkle/Sparkle/bin/generate_keys \
    --account local.orbitdisk.app -x "$key_file"
  SPARKLE_PRIVATE_KEY="$(cat "$key_file")" \
    ./scripts/release.sh v0.9.1 --prepare-only
)
```

Verified assets are written to `.build/releases/v0.9.1`. The script checks version/build metadata, archive URLs and size, signatures, and bundle integrity. It creates the disk image after the update feed so the feed contains only the ZIP. It verifies and mounts the image read-only, checks the app's code signature and compares its files, symlinks and permissions with the original bundle before adding it to the release. The Build workflow also tests that a change to executable permissions is rejected. The script refuses to overwrite an existing preparation directory. `--prepare-only` does not create or push a tag, contact GitHub to publish, or run the test suite; run the checks above first. Omit that flag only when deliberately publishing from a clean, tagged checkout whose tag is already on GitHub.

To package an already verified app separately, run `./scripts/package-dmg.sh "PATH/Orbit Disk.app" "PATH/Orbit-Disk.dmg"`. The destination must not already exist.

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

If none are set, the release remains ad-hoc signed. An incomplete set fails validation. The signing script uses a temporary Keychain, signs nested helpers and the app, waits for Apple to accept notarization, and staples and verifies the ticket before packaging. Keep these values in Actions secrets or temporary local environment variables, never in the repository.

The disk image itself is not signed or notarized. Apple recommends signing and notarizing the distributed disk image as well as the app.

## Update interface

The app uses Sparkle's standard installer with daily checks and user-approved installation/restart. The English dialog copy is in `Assets/Sparkle.en.json`. Packaging merges it into the staged framework and re-signs the framework; the original Sparkle license and other languages remain included. The merge fails if an expected vendor string key changes, so review this overlay when upgrading Sparkle. Skip, dismissal, and install-on-quit actions retain their distinct meanings.
