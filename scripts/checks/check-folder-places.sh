#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
build_dir=$(mktemp -d /tmp/orbit-folder-places.XXXXXX)
trap 'rm -rf "$build_dir"' EXIT
swiftc -parse-as-library -swift-version 5 Sources/OrbitDisk/FolderPlaces.swift Tests/OrbitFolderPlacesChecks.swift -o "$build_dir/check"
"$build_dir/check"
