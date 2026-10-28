#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h:h}"
if (( $# > 1 )) || [[ -n "${1:-}" && "${1:-}" != --native-only ]]; then
  print -u2 'Usage: ./scripts/checks/check-app.sh [--native-only]'
  exit 2
fi
swift build -c release --target OrbitCore -Xswiftc -DORBIT_TESTING
build_path=$(swift build -c release --show-bin-path)
package_name=$(python3 scripts/swift-package-name.py "$build_path/description.json")
target="$(uname -m)-apple-macosx14.0"
swiftc -parse-as-library -O -assert-config Debug -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name "$package_name" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitAppChecks.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitAppChecks"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitAppChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
"$build_path/OrbitAppChecks" "$fixture"
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name "$package_name" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitLargeCollectionChecks.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitLargeCollectionChecks"
mkdir -p "$fixture/large-collection"
"$build_path/OrbitLargeCollectionChecks" "$fixture/large-collection"
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name "$package_name" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift Tests/OrbitCollectionRemovalChecks.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitCollectionRemovalChecks"
mkdir -p "$fixture/collection-removal"
"$build_path/OrbitCollectionRemovalChecks" "$fixture/collection-removal"
swiftc -assert-config Debug -O -swift-version 5 -target "$target" \
  -I "$build_path/Modules" -I Sources/CSQLite \
  Sources/OrbitDisk/SunburstView.swift Sources/OrbitDisk/MapInteractionView.swift \
  Sources/OrbitDisk/ListRowInteractionView.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/PointingCursor.swift Tests/OrbitInteractionChecks/main.swift \
  "$build_path/OrbitCore.build/"*.swift.o -o "$build_path/OrbitInteractionChecks"
"$build_path/OrbitInteractionChecks"
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" \
  Sources/OrbitDisk/ListHoverTracking.swift Tests/OrbitListHoverChecks.swift \
  -o "$build_path/OrbitListHoverChecks"
"$build_path/OrbitListHoverChecks"

swiftc -assert-config Debug -O -swift-version 5 -target "$target" -package-name OrbitDisk -I Sources/CSQLite \
  Sources/OrbitCore/*.swift \
  Tests/OrbitPreviewCancellationChecks/main.swift -o "$build_path/OrbitPreviewCancellationChecks"
"$build_path/OrbitPreviewCancellationChecks"

if [[ "${1:-}" != --native-only ]]; then
  ./scripts/checks/check-preview-batches.sh
  ./scripts/checks/check-preview-search.sh
  ./scripts/checks/check-stage-search-fast.sh
  ./scripts/checks/check-saved-search.sh
  ./scripts/checks/check-session.sh
  ./scripts/checks/check-resumable.sh
  ./scripts/checks/check-mapping-reuse.sh
  ./scripts/checks/check-stage.sh
  ./scripts/checks/check-removal-journal.sh
  ./scripts/checks/check-stage-removal.sh
  ./scripts/checks/check-durable-removal.sh
  ./scripts/checks/check-folder-transaction.sh
fi

swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING \
  Sources/OrbitDisk/DiskChangeMonitor.swift Tests/OrbitMonitorChecks.swift \
  -o "$build_path/OrbitMonitorChecks"
"$build_path/OrbitMonitorChecks"
./scripts/checks/check-interaction-ownership.sh
