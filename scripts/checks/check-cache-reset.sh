#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
build_dir=$(mktemp -d /private/tmp/orbit-cache-reset-checks.XXXXXX)
trap 'rm -rf "$build_dir"' EXIT
swiftc -parse-as-library -swift-version 5 -module-cache-path "$build_dir/module-cache" Sources/OrbitDisk/MappingCacheReset.swift Tests/OrbitCacheResetChecks.swift -o "$build_dir/check"
"$build_dir/check"
