#!/bin/zsh
set -euo pipefail
cd "${0:A:h:h:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitBootVolumeChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$(uname -m)-apple-macosx14.0" \
  -D ORBIT_TESTING -package-name OrbitDisk -I Sources/CSQLite \
  Sources/OrbitCore/*.swift Tests/OrbitBootVolumeChecks/main.swift -o "$fixture/BootVolumeChecks"
"$fixture/BootVolumeChecks" "$fixture/data"
