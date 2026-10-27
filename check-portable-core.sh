#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
temp_root=${TMPDIR:-/tmp}
build=$(mktemp -d "${temp_root%/}/OrbitPortableCoreChecks.XXXXXX")
trap 'rm -rf "$build"' EXIT
compiler_flags=(-package-name OrbitDisk)
if [[ "$(uname -s)" == Linux ]]; then compiler_flags+=(-Xcc -D_GNU_SOURCE); fi
swiftc -O -assert-config Debug -parse-as-library -swift-version 5 \
  "${compiler_flags[@]}" \
  -module-cache-path "$build/module-cache" \
  Sources/OrbitCore/DiskNode.swift Sources/OrbitCore/ScanRemoval.swift \
  Sources/OrbitCore/HardLinkPathOrder.swift Sources/OrbitCore/StageCheckpoint.swift \
  Sources/OrbitCore/DiskRemovalProjection.swift Sources/OrbitDisk/CollectionCoverage.swift \
  Sources/OrbitCore/CompactNameIndex.swift Tests/OrbitPortableCoreChecks/CompactNameChecks.swift \
  Tests/OrbitPortableCoreChecks/main.swift -o "$build/PortableCoreChecks"
"$build/PortableCoreChecks"
