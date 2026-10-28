#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitResumableChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build"
target="$(uname -m)-apple-macosx14.0"
swiftc -assert-config Debug -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
  Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore \
  -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitResumableChecks/main.swift \
  -o "$build/SessionChecks"
"$build/SessionChecks" "$fixture"
