#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
temp_root=${TMPDIR:-/tmp}
fixture=$(mktemp -d "${temp_root%/}/OrbitStageSearchFast.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
build="$fixture/build"
mkdir -p "$build"
python3 - "$fixture" <<'PY'
import os, sys
from pathlib import Path
base = Path(sys.argv[1])
root = base / "varied" / "root"
root.mkdir(parents=True)
for i in range(20_000):
    path = root / f"file-{i:05d}"
    path.touch()
    os.truncate(path, i * 811 % 50_000)
root = base / "concurrent" / "root"
(root / "nested").mkdir(parents=True)
for i in range(2_000):
    (root / f"file-{i:04d}").touch()
    (root / "nested" / f"file-{i:04d}").touch()
root = base / "normalized" / "root"
root.mkdir(parents=True)
for i in range(1_400):
    (root / f"file-{i:04d}").touch()
root = base / "equal-cancel" / "root"
root.mkdir(parents=True)
for i in range(4_000):
    (root / f"file-{i:05d}").touch()
root = base / "parity" / "root"
root.mkdir(parents=True)
for folder, suffix in (("a", "été"), ("b", "K"), ("c", "नमस्ते"), ("d", "が")):
    path = root / folder
    path.mkdir()
    for i in range(300):
        (path / f"file-{suffix}-{i:04d}").touch()
for i in range(1_000):
    path = root / f"file-{i:04d}"
    path.touch()
    os.truncate(path, i % 41)
os.link(root / "file-0000", root / "file-hardlink")
(root / "file-folder").mkdir()
(root / "file-folder" / "file-child").write_bytes(b"abc")
PY
target="$(uname -m)-apple-macosx14.0"
swiftc -assert-config Debug -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
  -emit-library -emit-module -module-name OrbitCore -I Sources/CSQLite \
  Sources/OrbitCore/*.swift -o "$build/libOrbitCore.dylib"
for suite in cancel concurrent damage parity normalized equal-cancel; do
  swiftc -assert-config Debug -parse-as-library -O -swift-version 5 -target "$target" -D ORBIT_TESTING -package-name OrbitDisk \
    -I "$build" -I Sources/CSQLite -L "$build" -lOrbitCore -Xlinker -rpath -Xlinker "$build" \
    "Tests/OrbitStageSearchFastChecks/$suite.swift" -o "$build/$suite"
  case "$suite" in
    cancel|damage) "$build/$suite" "$fixture/varied" ;;
    concurrent) "$build/$suite" "$fixture/concurrent" ;;
    parity) "$build/$suite" "$fixture/parity" ;;
    normalized) "$build/$suite" "$fixture/normalized" ;;
    equal-cancel) "$build/$suite" "$fixture/equal-cancel" ;;
  esac
done
