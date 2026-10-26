#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
app_path="${PWD:h}/Orbit Disk.app"
universal=false
while (( $# )); do
  case "$1" in
    --universal) universal=true; shift ;;
    --output) app_path="${2:?--output needs an app path}"; shift 2 ;;
    *) print -u2 "Usage: $0 [--universal] [--output PATH.app]"; exit 2 ;;
  esac
done
app_path="${app_path:A}"
[[ "$app_path" == *.app ]] || { print -u2 'Output must end in .app'; exit 2; }
plutil -lint Config/Info.plist
architectures=("$(uname -m)")
$universal && architectures=(arm64 x86_64)
binaries=()
for architecture in "${architectures[@]}"; do
  swift build -c release --arch "$architecture" --product OrbitDisk
  bin_path=$(swift build -c release --arch "$architecture" --show-bin-path)
  binaries+=("$bin_path/OrbitDisk")
done
sparkle_path="$PWD/.build/artifacts/sparkle/Sparkle"
framework="$sparkle_path/Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework"
[[ -d "$framework" && -f "$sparkle_path/LICENSE" ]] || { print -u2 'Resolved Sparkle framework or license missing'; exit 1; }
mkdir -p .build "${app_path:h}"
staging=$(mktemp -d "$PWD/.build/app-bundle.XXXXXX")
trap 'rm -rf "$staging"' EXIT
bundle="$staging/Orbit Disk.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$bundle/Contents/Frameworks"
cp Config/Info.plist "$bundle/Contents/Info.plist"
cp Assets/OrbitDisk.icns "$bundle/Contents/Resources/OrbitDisk.icns"
cp "$sparkle_path/LICENSE" "$bundle/Contents/Resources/Sparkle-LICENSE.txt"
# Framework version links and helper executable permissions must survive packaging.
ditto "$framework" "$bundle/Contents/Frameworks/Sparkle.framework"
python3 scripts/customize-sparkle.py "$bundle/Contents/Frameworks/Sparkle.framework"
codesign --force --sign - --preserve-metadata=identifier,entitlements,flags \
  "$bundle/Contents/Frameworks/Sparkle.framework"
if $universal; then
  lipo -create "${binaries[@]}" -output "$bundle/Contents/MacOS/OrbitDisk"
  lipo "$bundle/Contents/MacOS/OrbitDisk" -verify_arch arm64 x86_64
else
  cp "${binaries[1]}" "$bundle/Contents/MacOS/OrbitDisk"
fi
codesign --force --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
# Swap only after the complete replacement has passed verification.
backup="$staging/previous.app"
if [[ -e "$app_path" ]]; then mv "$app_path" "$backup"; fi
if ! mv "$bundle" "$app_path"; then
  [[ ! -e "$backup" ]] || mv "$backup" "$app_path"
  exit 1
fi
printf 'Built %s\n' "$app_path"
