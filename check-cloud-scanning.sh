#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
fixture=$(mktemp -d "${TMPDIR%/}/OrbitCloudScanningChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
target="$(uname -m)-apple-macosx14.0"
mkdir -p "$fixture/module-cache"
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" \
  -module-cache-path "$fixture/module-cache" -D ORBIT_TESTING -package-name OrbitDisk \
  -I Sources/CSQLite Sources/OrbitCore/*.swift Tests/OrbitCloudScanningChecks/main.swift \
  -o "$fixture/CloudScanningChecks"
"$fixture/CloudScanningChecks" "$fixture/data"
