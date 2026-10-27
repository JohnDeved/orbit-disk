#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitMappingReuse.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build"
target="$(uname -m)-apple-macosx14.0"
swiftc -O -swift-version 5 -target "$target" -D ORBIT_TESTING -enable-testing -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
  Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -Xlinker -rpath -Xlinker "$build" \
  Tests/OrbitMappingCoverageChecks/main.swift -o "$build/CoverageChecks"
"$build/CoverageChecks" "$fixture/coverage"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskChangeMonitor.swift Tests/OrbitSubtreeReuseChecks/main.swift -o "$build/SubtreeReuseChecks"
"$build/SubtreeReuseChecks" "$fixture/subtree"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitMappingReuseChecks/main.swift -o "$build/ReuseChecks"
"$build/ReuseChecks" "$fixture/model"
