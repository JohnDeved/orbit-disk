#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
swift build -c release --target OrbitCore
build_path=$(swift build -c release --show-bin-path)
framework_path="$PWD/.build/artifacts/sparkle/Sparkle/Sparkle.xcframework/macos-arm64_x86_64"
swiftc -parse-as-library -O -swift-version 5 -package-name orbitdisk \
  -target "$(uname -m)-apple-macosx14.0" \
  -I "$build_path/Modules" -I Sources/CSQLite -F "$framework_path" \
  -Xlinker -rpath -Xlinker "$framework_path" \
  Sources/OrbitDisk/AppUpdater.swift Sources/OrbitDisk/DiskModel.swift \
  Sources/OrbitDisk/PointingCursor.swift \
  Sources/OrbitDisk/DiskChangeMonitor.swift Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitUpdaterChecks.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitUpdaterChecks"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitUpdaterChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
"$build_path/OrbitUpdaterChecks" "$fixture"
