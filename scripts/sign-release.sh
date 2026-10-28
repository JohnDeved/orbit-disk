#!/bin/zsh
set +x
set -euo pipefail
cd "${0:A:h:h}"
app_path="${1:?Usage: scripts/sign-release.sh PATH.app}"
app_path="${app_path:A}"
[[ -d "$app_path/Contents" ]] || { print -u2 'Application bundle missing'; exit 1; }
version=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' Config/Info.plist)
mode=$(python3 scripts/validate-release.py "v$version" --signing-mode)
if [[ "$mode" == adhoc ]]; then
  print 'No Apple credentials configured; retaining the ad-hoc signature.'
  codesign --verify --deep --strict "$app_path"
  exit 0
fi
mkdir -p .build
staging=$(mktemp -d "$PWD/.build/apple-signing.XXXXXX")
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
framework="$app_path/Contents/Frameworks/Sparkle.framework"
# Sign nested code first, preserving Sparkle's helper entitlements.
for target in \
  "$framework/Versions/Current/XPCServices/Downloader.xpc" \
  "$framework/Versions/Current/XPCServices/Installer.xpc" \
  "$framework/Versions/Current/Autoupdate" \
  "$framework/Versions/Current/Updater.app" \
  "$framework" "$app_path/Contents/MacOS/OrbitControl" "$app_path"; do
  codesign --force --sign "$APPLE_SIGNING_IDENTITY" --keychain "$keychain" \
    --options runtime --timestamp --preserve-metadata=identifier,entitlements "$target"
done
codesign --verify --deep --strict "$app_path"
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
