#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h:h}"
fixture=$(mktemp -d "${TMPDIR:-/tmp}/OrbitStageRemoval.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build"
target="$(uname -m)-apple-macosx14.0"
swiftc -assert-config Debug -O -swift-version 5 -target "$target" -D ORBIT_TESTING -enable-testing -package-name OrbitDisk -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
for source in Tests/OrbitStageRemovalChecks/*.swift; do
 name=${source:t:r}
 swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -Xlinker -rpath -Xlinker "$build" "$source" -o "$build/$name"
 mkdir "$fixture/$name"
 "$build/$name" "$fixture/$name"
done
