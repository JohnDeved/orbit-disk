#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitRemovalJournalChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build"
target="$(uname -m)-apple-macosx14.0"
swiftc -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
  Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore \
  -Xlinker -rpath -Xlinker "$build" Tests/OrbitRemovalJournalChecks.swift -o "$build/Checks"
"$build/Checks" parent "$fixture/data"
