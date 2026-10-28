#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h:h}"
fixture=$(mktemp -d "${TMPDIR:-/tmp}/OrbitSearchLifetime.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
target="$(uname -m)-apple-macosx14.0"
for optimization in O Onone; do
  build="$fixture/$optimization"
  mkdir -p "$build/data"
  swiftc -assert-config Debug "-$optimization" -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
    -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
    Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
  swiftc -assert-config Debug -parse-as-library "-$optimization" -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
    -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -Xlinker -rpath -Xlinker "$build" \
    Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
    Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
    Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitSearchLifetimeChecks.swift -o "$build/check"
  "$build/check" "$build/data" cancelled
done
