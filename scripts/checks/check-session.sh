#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitSessionChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build"
target="$(uname -m)-apple-macosx14.0"
swiftc -assert-config Debug -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
  Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING \
  -enable-testing -package-name OrbitDisk -emit-library -emit-module -module-name OrbitAppTestSupport \
  -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -Xlinker -rpath -Xlinker "$build" \
  Sources/OrbitDisk/DiskModel.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Sources/OrbitDisk/CollectionCoverage.swift Sources/OrbitDisk/PreparedMapHoverGroup.swift \
  Sources/OrbitDisk/IndexRefreshPlanner.swift -o "$build/libOrbitAppTestSupport.dylib"

compile_session_check() {
  local check_source=$1 check_name=$2 optimization=${3:--O}
  print '@testable import OrbitAppTestSupport' > "$build/$check_name.swift"
  cat "$check_source" >> "$build/$check_name.swift"
  swiftc -assert-config Debug -parse-as-library "$optimization" -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
    -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -lOrbitAppTestSupport \
    -Xlinker -rpath -Xlinker "$build" "$build/$check_name.swift" -o "$build/$check_name"
}

# Only the large test harness skips optimization; the app module above stays optimized.
compile_session_check Tests/OrbitSessionChecks/main.swift SessionChecks -Onone
"$build/SessionChecks" "$fixture"
compile_session_check Tests/OrbitQueuedCollectionChecks/main.swift QueuedCollectionChecks
mkdir -p "$fixture/queued-collection"
"$build/QueuedCollectionChecks" "$fixture/queued-collection"
compile_session_check Tests/OrbitCollectionSessionChecks/main.swift CollectionSessionChecks
mkdir -p "$fixture/collection-session"
"$build/CollectionSessionChecks" "$fixture/collection-session"
compile_session_check Tests/OrbitRemovalStartChecks/main.swift RemovalStartChecks
"$build/RemovalStartChecks" "$fixture/removal-start"
for suite in OrbitRemovalFinishChecks OrbitRemovalFinishExtraChecks; do
  compile_session_check "Tests/$suite/main.swift" "$suite"
  "$build/$suite" "$fixture/$suite"
done
compile_session_check Tests/OrbitVolumeChecks/main.swift VolumeChecks
"$build/VolumeChecks" "$fixture/volume-checks"
compile_session_check Tests/OrbitFailureRecoveryChecks/main.swift FailureRecoveryChecks
"$build/FailureRecoveryChecks" "$fixture/failure-recovery"
