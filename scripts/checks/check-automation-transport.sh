#!/bin/zsh
set -euo pipefail
repo=${0:A:h:h:h}
build=$(mktemp -d "${TMPDIR:-/tmp}/OrbitAutomationChecks.XXXXXX")
trap 'rm -rf "$build"' EXIT
cd "$repo"
common=(-swift-version 5 -target "$(uname -m)-apple-macosx14.0" -module-cache-path "$build/modules" -parse-as-library)
swiftc "${common[@]}" -emit-module -emit-library -module-name OrbitAutomation Sources/OrbitAutomation/*.swift -o "$build/libOrbitAutomation.dylib" -emit-module-path "$build/OrbitAutomation.swiftmodule"
swiftc "${common[@]}" -I "$build" -L "$build" -lOrbitAutomation -Xlinker -rpath -Xlinker "$build" Tests/OrbitAutomationTransportChecks/*.swift -o "$build/checks"
"$build/checks"
