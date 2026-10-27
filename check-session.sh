#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitSessionChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build"
target="$(uname -m)-apple-macosx14.0"
swiftc -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
  Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore \
  -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitSessionChecks/main.swift \
  -o "$build/SessionChecks"
"$build/SessionChecks" "$fixture"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore \
  -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitCollectionSessionChecks/main.swift \
  -o "$build/CollectionSessionChecks"
mkdir -p "$fixture/collection-session"
"$build/CollectionSessionChecks" "$fixture/collection-session"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore \
  -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitRemovalStartChecks/main.swift \
  -o "$build/RemovalStartChecks"
"$build/RemovalStartChecks" "$fixture/removal-start"
for suite in OrbitRemovalFinishChecks OrbitRemovalFinishExtraChecks; do
  swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
    -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore \
    -Xlinker -rpath -Xlinker "$build" \
    Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
    Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
    Sources/OrbitDisk/IndexRefreshPlanner.swift "Tests/$suite/main.swift" -o "$build/$suite"
  "$build/$suite" "$fixture/$suite"
done
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore \
  -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitVolumeChecks/main.swift \
  -o "$build/VolumeChecks"
"$build/VolumeChecks" "$fixture/volume-checks"

swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore \
  -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitFailureRecoveryChecks/main.swift \
  -o "$build/FailureRecoveryChecks"
"$build/FailureRecoveryChecks" "$fixture/failure-recovery"
