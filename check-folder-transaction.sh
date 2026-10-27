#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitFolderTransactionChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build"
target="$(uname -m)-apple-macosx14.0"
swiftc -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
  Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
app_sources=(Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift
  Sources/OrbitDisk/IndexRefreshPlanner.swift)
for check in Notifications Synthetic Route Colors; do
  swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
    -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -Xlinker -rpath -Xlinker "$build" \
    "${app_sources[@]}" "Tests/OrbitFolderTransactionChecks/$check.swift" -o "$build/$check"
done
for action in cancel switch begin-cancel begin-switch finish-cancel finish-switch finish-selection; do
  "$build/Notifications" "$fixture/$action" "$action"
done
"$build/Synthetic" "$fixture/synthetic"
"$build/Colors" "$fixture/colors"
"$build/Route" "$fixture/route"
"$build/Route" "$fixture/route-remove" remove
