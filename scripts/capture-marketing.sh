#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h}"
root="$PWD"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/OrbitMarketing.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/app" "$root/docs/screenshots" "$root/docs/art"
# Compile a snapshot so an unrelated build never changes the app under capture.
cp Sources/OrbitDisk/*.swift "$tmp/app/"
cat scripts/marketing/FixtureExtension.swift >> "$tmp/app/DiskModel.swift"
target="$(uname -m)-apple-macosx14.0"
print "Building OrbitCore for the screenshot fixture…"
swiftc -parse-as-library -Onone -swift-version 5 -package-name OrbitDisk -module-name OrbitCore \
  -target "$target" -I Sources/CSQLite -module-cache-path "$tmp/module-cache" \
  -emit-module -emit-module-path "$tmp/OrbitCore.swiftmodule" -emit-object \
  -whole-module-optimization Sources/OrbitCore/*.swift -o "$tmp/OrbitCore.o"
print "Building the native screenshot renderer…"
app_sources=("$tmp/app/"*.swift)
app_sources=(${app_sources:#*/OrbitDiskApp.swift})
app_sources=(${app_sources:#*/AppUpdater.swift})
app_sources=(${app_sources:#*/AutomationController.swift})
app_sources=(${app_sources:#*/AutomationIntegration.swift})
swiftc -parse-as-library -Onone -swift-version 5 -package-name OrbitDisk -target "$target" \
  -module-cache-path "$tmp/module-cache" -I "$tmp" -I Sources/CSQLite \
  "${app_sources[@]}" scripts/marketing/Capture.swift "$tmp/OrbitCore.o" -o "$tmp/MarketingCapture"
"$tmp/MarketingCapture" "$tmp/raw" -AppleLocale en_US
swiftc -swift-version 5 -target "$target" -module-cache-path "$tmp/module-cache" \
  scripts/marketing/Frame.swift -o "$tmp/MarketingFrame"
"$tmp/MarketingFrame" "$tmp/raw" "$root/docs/screenshots"
swiftc -swift-version 5 -target "$target" -module-cache-path "$tmp/module-cache" \
  scripts/marketing/DrawFlow.swift -o "$tmp/DrawFlow"
"$tmp/DrawFlow" "$tmp/raw" "$root/docs/art"
print "Rendered README flow images."
