#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
swift build -c release --target OrbitCore -Xswiftc -DORBIT_TESTING
build_path=$(swift build -c release --show-bin-path)
package_name=$(python3 scripts/swift-package-name.py "$build_path/description.json")
target="$(uname -m)-apple-macosx14.0"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name "$package_name" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitAppChecks.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitAppChecks"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitAppChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
"$build_path/OrbitAppChecks" "$fixture"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name "$package_name" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitLargeCollectionChecks.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitLargeCollectionChecks"
mkdir -p "$fixture/large-collection"
"$build_path/OrbitLargeCollectionChecks" "$fixture/large-collection"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name "$package_name" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitCollectionRemovalChecks.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitCollectionRemovalChecks"
mkdir -p "$fixture/collection-removal"
"$build_path/OrbitCollectionRemovalChecks" "$fixture/collection-removal"
swiftc -O -swift-version 5 -target "$target" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/SunburstView.swift Sources/OrbitDisk/MapInteractionView.swift \
  Sources/OrbitDisk/ListRowInteractionView.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/PointingCursor.swift Tests/OrbitInteractionChecks/main.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitInteractionChecks"
"$build_path/OrbitInteractionChecks"
swiftc -parse-as-library -O -swift-version 5 -target "$target" \
  Sources/OrbitDisk/ListHoverTracking.swift Tests/OrbitListHoverChecks.swift \
  -o "$build_path/OrbitListHoverChecks"
"$build_path/OrbitListHoverChecks"

swiftc -O -swift-version 5 -target "$target" -package-name OrbitDisk -I Sources/CSQLite \
  Sources/OrbitCore/*.swift \
  Tests/OrbitPreviewCancellationChecks/main.swift -o "$build_path/OrbitPreviewCancellationChecks"
"$build_path/OrbitPreviewCancellationChecks"

./check-preview-batches.sh
./check-preview-search.sh
./check-stage-search-fast.sh
./check-saved-search.sh
./check-session.sh
./check-resumable.sh
./check-stage.sh
./check-removal-journal.sh
./check-stage-removal.sh
./check-durable-removal.sh
./check-folder-transaction.sh

swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING \
  Sources/OrbitDisk/DiskChangeMonitor.swift Tests/OrbitMonitorChecks.swift \
  -o "$build_path/OrbitMonitorChecks"
"$build_path/OrbitMonitorChecks"
./check-interaction-ownership.sh
