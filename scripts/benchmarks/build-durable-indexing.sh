#!/bin/zsh
set -euo pipefail

[[ $# -ge 2 ]] || { print -u2 'Pass a source root and new build directory.'; exit 2; }
repo_root=${0:A:h:h:h}
source_root=${1:A}
build_root=${2:A}
mode=${3:-grouped}
probe=${4:-indexing}
[[ "$mode" == single || "$mode" == grouped ]] || {
  print -u2 'Usage: build-durable-indexing.sh source-root new-build-directory [single|grouped] [indexing|pipeline|marking]'
  exit 2
}
[[ ! -e "$build_root" ]] || { print -u2 'Build directory must be new.'; exit 2; }
case "$probe" in
  indexing) harness=IndexingProbe.swift; binary=IndexingProbe ;;
  pipeline) harness=PipelineProbe.swift; binary=PipelineProbe ;;
  marking) harness=MarkingProbe.swift; binary=MarkingProbe ;;
  *) print -u2 'Unknown probe.'; exit 2 ;;
esac

mkdir -p "$build_root/source/Sources" "$build_root/module-cache"
cp -R "$source_root/Sources/OrbitCore" "$source_root/Sources/CSQLite" "$build_root/source/Sources/"
cp "$repo_root/Benchmarks/DurableIndexing/$harness" "$build_root/$harness"
python3 - "$build_root" <<'PY'
import hashlib
import json
from pathlib import Path
import sys
build = Path(sys.argv[1])
source = build / 'source'
hashes = {str(path.relative_to(source)): hashlib.sha256(path.read_bytes()).hexdigest()
          for path in sorted(source.rglob('*')) if path.is_file()}
(build / 'source-hashes.json').write_text(json.dumps(hashes, indent=2, sort_keys=True) + '\n')
PY
if [[ "$probe" == indexing ]]; then
  cat "$repo_root/Benchmarks/DurableIndexing/ProfileExtension.swift.inc" >> "$build_root/source/Sources/OrbitCore/WALListingStage.swift"
fi
flags=()
[[ "$probe" == indexing && "$mode" == grouped ]] && flags+=(-D GROUPED_WRITER)
[[ "$probe" == pipeline ]] && flags+=(-D ORBIT_TESTING)
swiftc -O -whole-module-optimization -parse-as-library -swift-version 5 \
  -target "$(uname -m)-apple-macosx14.0" -module-cache-path "$build_root/module-cache" \
  -package-name OrbitDisk -I "$build_root/source/Sources/CSQLite" "${flags[@]}" \
  "$build_root"/source/Sources/OrbitCore/*.swift "$build_root/$harness" \
  -o "$build_root/$binary"

python3 - "$source_root" "$build_root" "$harness" "$binary" "$mode" "$probe" <<'PY'
import hashlib
import json
from pathlib import Path
import subprocess
import sys

source, build = map(Path, sys.argv[1:3])
harness, binary, mode, probe = sys.argv[3:]
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
record = {
    'probe': probe, 'mode': mode,
    'swift': subprocess.check_output(['swiftc', '--version'], text=True).strip(),
    'macos': subprocess.check_output(['sw_vers'], text=True).strip(),
    'source_sha256': json.loads((build / 'source-hashes.json').read_text()),
    'compiled_source_sha256': {str(path.relative_to(build)): digest(path)
                               for path in sorted((build / 'source').rglob('*')) if path.is_file()},
    'harness_sha256': digest(build / harness), 'binary_sha256': digest(build / binary),
}
(build / 'manifest.json').write_text(json.dumps(record, indent=2, sort_keys=True) + '\n')
PY
print "$build_root/$binary"
