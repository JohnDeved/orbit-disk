#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
swift build -c release --target OrbitCore
build_path=$(swift build -c release --show-bin-path)
mkdir -p .build/module-cache
swiftc -parse-as-library -O -swift-version 5 \
  -module-cache-path .build/module-cache -target "$(uname -m)-apple-macosx14.0" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/SunburstView.swift Sources/OrbitDisk/MapInteractionView.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift Sources/OrbitDisk/PointingCursor.swift \
  Tests/OrbitRenderChecks.swift "$build_path/OrbitCore.build/"*.swift.o \
  -o "$build_path/OrbitRenderChecks"
"$build_path/OrbitRenderChecks"
swiftc -parse-as-library -O -swift-version 5 \
  -module-cache-path .build/module-cache -target "$(uname -m)-apple-macosx14.0" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/SunburstView.swift Sources/OrbitDisk/MapInteractionView.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift Sources/OrbitDisk/PointingCursor.swift \
  Tests/OrbitRootGroupChecks/main.swift "$build_path/OrbitCore.build/"*.swift.o \
  -o "$build_path/OrbitRootGroupChecks"
"$build_path/OrbitRootGroupChecks"
