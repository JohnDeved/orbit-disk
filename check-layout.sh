#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
checks_path="$PWD/.build/layout-checks"
build_path="$checks_path/core"
mkdir -p "$build_path"
target="$(uname -m)-apple-macosx14.0"
swiftc -assert-config Debug -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
  Sources/OrbitCore/*.swift -o "$build_path/libOrbitCore.dylib"
python3 Tests/OrbitLayoutChecks/make-probes.py "$checks_path/ContentView.swift"
app_sources=(Sources/OrbitDisk/*.swift)
app_sources=(${app_sources:#*/ContentView.swift})
app_sources=(${app_sources:#*/FileRowView.swift})
app_sources=(${app_sources:#*/OrbitDiskApp.swift})
app_sources=(${app_sources:#*/AppUpdater.swift})
app_sources=(${app_sources:#*/AutomationController.swift})
app_sources=(${app_sources:#*/AutomationIntegration.swift})
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -module-cache-path "$PWD/.build/module-cache" \
  -I "$build_path" -I Sources/CSQLite -L "$build_path" -lOrbitCore \
  -Xlinker -rpath -Xlinker "$build_path" \
  "${app_sources[@]}" "$checks_path/ContentView.swift" "$checks_path/FileRowView.swift" \
  Tests/OrbitLayoutChecks/LayoutChecks.swift \
  -o "$checks_path/OrbitLayoutChecks"
# Collection policy rejects hidden ancestors such as .build.
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitLayoutChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
"$checks_path/OrbitLayoutChecks" "$fixture" "$checks_path"
printf 'Layout reports: %s\n' "$checks_path"
