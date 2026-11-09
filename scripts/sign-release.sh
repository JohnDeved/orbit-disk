#!/bin/zsh
# Signs a bundle built by `cargo xtask bundle` for release.
#
# - No Apple credentials: keeps the bundle's ad-hoc signature.
# - All six Apple secrets: Developer ID signing with the hardened runtime, notarization, stapling.
# - --dry-run: rehearses the Developer ID signing with an ad-hoc identity (same codesign options,
#   hardened runtime and entitlements, no timestamp), checks the result, and prints the
#   notarization steps instead of running them. Apple credentials are ignored.
set +x
set -euo pipefail
cd "${0:A:h:h}"
usage='Usage: scripts/sign-release.sh PATH.app [--dry-run]'
app_path="${1:?$usage}"
dry_run=false
if (( $# == 2 )) && [[ "$2" == --dry-run ]]; then
  dry_run=true
elif (( $# != 1 )); then
  print -u2 "$usage"; exit 2
fi
app_path="${app_path:A}"
[[ -d "$app_path/Contents" ]] || { print -u2 'Application bundle missing'; exit 1; }
entitlements="$PWD/Config/OrbitDisk.entitlements"
plutil -lint -s "$entitlements"
framework="$app_path/Contents/Frameworks/Sparkle.framework"
executables=("$app_path/Contents/MacOS/OrbitDisk" "$app_path/Contents/MacOS/OrbitControl")

# Called in subshells: zsh skips EXIT traps (keychain cleanup) when errexit fires in a function.
# Signs nested code first. Sparkle's helpers keep their own entitlements; OrbitControl and the
# app (whose signature covers Contents/MacOS/OrbitDisk) get Config/OrbitDisk.entitlements.
sign_bundle() {
  local identity="$1"; shift
  local target
  for target in \
    "$framework/Versions/Current/XPCServices/Downloader.xpc" \
    "$framework/Versions/Current/XPCServices/Installer.xpc" \
    "$framework/Versions/Current/Autoupdate" \
    "$framework/Versions/Current/Updater.app" \
    "$framework"; do
    codesign --force --sign "$identity" "$@" --options runtime \
      --preserve-metadata=identifier,entitlements "$target"
  done
  for target in "$app_path/Contents/MacOS/OrbitControl" "$app_path"; do
    codesign --force --sign "$identity" "$@" --options runtime \
      --preserve-metadata=identifier --entitlements "$entitlements" "$target"
  done
}

# Both executables must carry the hardened runtime and exactly the configured entitlements.
check_signature() {
  codesign --verify --deep --strict "$app_path"
  local executable details expected actual
  expected=$(plutil -convert json -o - "$entitlements")
  for executable in "${executables[@]}"; do
    details=$(codesign --display --verbose=2 "$executable" 2>&1)
    print -r -- "$details" | /usr/bin/grep -Eq '^CodeDirectory .*flags=0x[0-9a-f]+\([^)]*runtime' ||
      { print -u2 "Hardened runtime missing: $executable"; exit 1; }
    actual=$(codesign --display --entitlements - --xml "$executable" 2>/dev/null | plutil -convert json -o - - 2>/dev/null || print '{}')
    [[ "$actual" == "$expected" ]] || { print -u2 "Unexpected entitlements on $executable: $actual"; exit 1; }
  done
  "$app_path/Contents/MacOS/OrbitControl" --version >/dev/null
}

if $dry_run; then
  unset APPLE_CERTIFICATE_P12_BASE64 APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY \
    APPLE_API_KEY_P8 APPLE_API_KEY_ID APPLE_API_ISSUER
  print 'Dry run: rehearsing Developer ID signing with an ad-hoc identity (hardened runtime, entitlements).'
  (sign_bundle - --timestamp=none)
  (check_signature)
  # The hardened runtime must not stop the executables from loading (dyld, library validation).
  "$app_path/Contents/MacOS/OrbitDisk" --help >/dev/null 2>&1 || (( $? == 2 ))
  print 'Dry run: would now submit to Apple and staple:'
  print '  xcrun notarytool submit notarization.zip --key AuthKey.p8 --key-id … --issuer … --wait --timeout 30m'
  print '  xcrun stapler staple "Orbit Disk.app" && xcrun stapler validate "Orbit Disk.app"'
  print 'Hardened-runtime rehearsal signature verified.'
  exit 0
fi

mode=$(python3 scripts/validate-release.py --signing-mode)
if [[ "$mode" == adhoc ]]; then
  print 'No Apple credentials configured; retaining the ad-hoc signature.'
  codesign --verify --deep --strict "$app_path"
  exit 0
fi
mkdir -p target
staging=$(mktemp -d "$PWD/target/apple-signing.XXXXXX")
keychain="$staging/release.keychain-db"
cleanup() {
  if [[ -e "$keychain" ]]; then security delete-keychain "$keychain" >/dev/null 2>&1 || true; fi
  rm -rf "$staging"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
umask 077
python3 - "$staging" <<'PY'
import base64, os, pathlib, sys
folder = pathlib.Path(sys.argv[1])
certificate = base64.b64decode("".join(os.environ["APPLE_CERTIFICATE_P12_BASE64"].split()), validate=True)
(folder / "certificate.p12").write_bytes(certificate)
(folder / "AuthKey.p8").write_text(os.environ["APPLE_API_KEY_P8"])
PY
unset APPLE_CERTIFICATE_P12_BASE64 APPLE_API_KEY_P8
keychain_password=$(openssl rand -base64 32)
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$staging/certificate.p12" -k "$keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
unset APPLE_CERTIFICATE_PASSWORD
rm "$staging/certificate.p12"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$keychain" >/dev/null
unset keychain_password
(sign_bundle "$APPLE_SIGNING_IDENTITY" --keychain "$keychain" --timestamp)
(check_signature)
ditto -c -k --sequesterRsrc --keepParent "$app_path" "$staging/notarization.zip"
if ! xcrun notarytool submit "$staging/notarization.zip" --key "$staging/AuthKey.p8" \
  --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" \
  --wait --timeout 30m --output-format json > "$staging/notarization.json"; then
  cat "$staging/notarization.json"
  exit 1
fi
python3 - "$staging/notarization.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1]))
if result.get("status") != "Accepted":
    raise SystemExit(f"Notarization failed: {result.get('status', 'unknown')}; submission {result.get('id', 'unknown')}")
PY
xcrun stapler staple "$app_path"
xcrun stapler validate "$app_path"
codesign --verify --deep --strict "$app_path"
print 'Developer ID signature and stapled Apple notarization verified.'
