#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
fixture=$(mktemp -d "${TMPDIR%/}/OrbitInteractionOwnership.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build"
target="$(uname -m)-apple-macosx14.0"
swiftc -O -swift-version 5 -target "$target" -module-cache-path "$build/module-cache" -D ORBIT_TESTING -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
  Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
app_sources=(Sources/OrbitDisk/*.swift)
app_sources=(${app_sources:#*/OrbitDiskApp.swift})
app_sources=(${app_sources:#*/AppUpdater.swift})
swiftc -parse-as-library -O -swift-version 5 -target "$target" -module-cache-path "$build/module-cache" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -Xlinker -rpath -Xlinker "$build" \
  "${app_sources[@]}" Tests/OrbitInteractionOwnershipChecks/*.swift -o "$build/OwnershipChecks"
"$build/OwnershipChecks" "$fixture/chart" chart
"$build/OwnershipChecks" "$fixture/row" row
"$build/OwnershipChecks" "$fixture/hover" hover
