#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitAutomationChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build/module-cache"
export CLANG_MODULE_CACHE_PATH="$build/module-cache"
export SWIFT_MODULECACHE_PATH="$build/module-cache"
target="$(uname -m)-apple-macosx14.0"
swiftc -assert-config Debug -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -module-cache-path "$build/module-cache" -emit-library -emit-module -module-name OrbitCore \
  -I Sources/CSQLite Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
swiftc -assert-config Debug -O -swift-version 5 -target "$target" \
  -module-cache-path "$build/module-cache" -emit-library -emit-module -module-name OrbitAutomation \
  Sources/OrbitAutomation/*.swift -o "$build/libOrbitAutomation.dylib"
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING \
  -module-cache-path "$build/module-cache" -enable-testing -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitAppTestSupport \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -lOrbitAutomation \
  -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Sources/OrbitDisk/AutomationController.swift \
  -o "$build/libOrbitAppTestSupport.dylib"
print '@testable import OrbitAppTestSupport' > "$build/AutomationChecks.swift"
cat Tests/OrbitAutomationChecks/main.swift >> "$build/AutomationChecks.swift"
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING \
  -module-cache-path "$build/module-cache" -package-name OrbitDisk \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -lOrbitAutomation -lOrbitAppTestSupport \
  -Xlinker -rpath -Xlinker "$build" "$build/AutomationChecks.swift" -o "$build/AutomationChecks"
"$build/AutomationChecks" "$fixture/data"
