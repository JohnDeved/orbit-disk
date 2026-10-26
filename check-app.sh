#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
swift build -c release --target OrbitCore
build_path=$(swift build -c release --show-bin-path)
target="$(uname -m)-apple-macosx14.0"
swiftc -parse-as-library -O -swift-version 5 -target "$target" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitAppChecks.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitAppChecks"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitAppChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
"$build_path/OrbitAppChecks" "$fixture"
swiftc -O -swift-version 5 -target "$target" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/SunburstView.swift Sources/OrbitDisk/MapInteractionView.swift \
  Sources/OrbitDisk/ListRowInteractionView.swift \
  Sources/OrbitDisk/CollectionCoverage.swift \
  Sources/OrbitDisk/PointingCursor.swift Tests/OrbitInteractionChecks/main.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitInteractionChecks"
"$build_path/OrbitInteractionChecks"
