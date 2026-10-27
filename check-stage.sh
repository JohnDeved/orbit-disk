#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitStageChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
target="$(uname -m)-apple-macosx14.0"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I Sources/CSQLite Sources/OrbitCore/*.swift Tests/OrbitStageChecks/*.swift -o "$fixture/StageChecks"
"$fixture/StageChecks" "$fixture/data"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I Sources/CSQLite Sources/OrbitCore/*.swift Sources/OrbitDisk/DiskChangeMonitor.swift \
  Tests/OrbitPublicationCrashChecks/main.swift -o "$fixture/PublicationCrashChecks"
"$fixture/PublicationCrashChecks"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I Sources/CSQLite Sources/OrbitCore/*.swift Tests/OrbitLockChecks/main.swift -o "$fixture/LockChecks"
"$fixture/LockChecks" "$fixture/locks"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I Sources/CSQLite Sources/OrbitCore/*.swift Tests/OrbitGenerationChecks/main.swift -o "$fixture/GenerationChecks"
"$fixture/GenerationChecks" "$fixture/generation"
swiftc -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -I Sources/CSQLite Sources/OrbitCore/*.swift Tests/OrbitQueueChecks/main.swift -o "$fixture/QueueChecks"
"$fixture/QueueChecks" "$fixture/queue.sqlite"
