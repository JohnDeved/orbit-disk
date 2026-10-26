#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
swift build -c release --target OrbitCore
build_path=$(swift build -c release --show-bin-path)
checks_path="$PWD/.build/layout-checks"
mkdir -p "$checks_path"
python3 Tests/OrbitLayoutChecks/make-probes.py "$checks_path/ContentView.swift"
app_sources=(Sources/OrbitDisk/*.swift)
app_sources=(${app_sources:#*/ContentView.swift})
app_sources=(${app_sources:#*/OrbitDiskApp.swift})
swiftc -parse-as-library -O -swift-version 5 \
  -target "$(uname -m)-apple-macosx14.0" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  "${app_sources[@]}" "$checks_path/ContentView.swift" \
  Tests/OrbitLayoutChecks/LayoutChecks.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$checks_path/OrbitLayoutChecks"
# Collection policy rejects hidden ancestors such as .build.
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitLayoutChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
"$checks_path/OrbitLayoutChecks" "$fixture" "$checks_path"
printf 'Layout reports: %s\n' "$checks_path"
