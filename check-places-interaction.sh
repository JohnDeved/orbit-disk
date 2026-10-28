#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
fixture=$(mktemp -d "${TMPDIR:-/tmp}/OrbitPlacesInteractionChecks.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
python3 - "$fixture/PlacesChecks.swift" <<'PY'
from pathlib import Path
import sys
source = Path('Sources/OrbitDisk/ContentView.swift').read_text()
rows = source[source.index('enum OrbitStyle {'):source.index('struct ContentView: View {')]
Path(sys.argv[1]).write_text('import AppKit\nimport SwiftUI\n' + rows + Path('Tests/OrbitPlacesInteractionChecks.swift').read_text())
PY
mkdir -p .build/module-cache
swiftc -parse-as-library -swift-version 5 -module-cache-path "$PWD/.build/module-cache" -target "$(uname -m)-apple-macosx14.0" \
  "$fixture/PlacesChecks.swift" Sources/OrbitDisk/PointingCursor.swift -o "$fixture/check"
"$fixture/check"
