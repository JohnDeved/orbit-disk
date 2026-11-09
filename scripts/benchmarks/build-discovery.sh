#!/bin/zsh
# Builds the Swift discovery-only probe (Benchmarks/Discovery/DiscoveryProbe.swift) against the
# OrbitCore sources of SOURCE-ROOT (the v0.9.15 tag for `cargo xtask bench-compare`), with the
# same monolithic compiler settings as build-durable-indexing.sh.
#
# Benchmark-only patch: v0.9.15 clamps `AsyncListingProducer` to at most four workers. The copy
# compiled here raises that clamp to 64 so worker sweeps (1/4/8) measure what they say; four
# workers and fewer run unchanged code. The patch is applied to the build copy only and recorded
# in manifest.json (`patches`, `compiled_source_sha256`).
set -euo pipefail

[[ $# -eq 2 ]] || { print -u2 'Usage: build-discovery.sh source-root new-build-directory'; exit 2; }
repo_root=${0:A:h:h:h}
source_root=${1:A}
build_root=${2:A}
[[ ! -e "$build_root" ]] || { print -u2 'Build directory must be new.'; exit 2; }
harness=DiscoveryProbe.swift
binary=DiscoveryProbe

mkdir -p "$build_root/source/Sources" "$build_root/module-cache"
cp -R "$source_root/Sources/OrbitCore" "$source_root/Sources/CSQLite" "$build_root/source/Sources/"
cp "$repo_root/Benchmarks/Discovery/$harness" "$build_root/$harness"
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
producer = source / 'Sources/OrbitCore/AsyncListingProducer.swift'
text = producer.read_text()
clamp = 'self.workerCount = max(1, min(workerCount, 4))'
if text.count(clamp) != 1:
    sys.exit('AsyncListingProducer.swift: worker clamp not found exactly once; update build-discovery.sh')
producer.write_text(text.replace(clamp, 'self.workerCount = max(1, min(workerCount, 64))'))
PY
swiftc -O -whole-module-optimization -parse-as-library -swift-version 5 \
  -target "$(uname -m)-apple-macosx14.0" -module-cache-path "$build_root/module-cache" \
  -package-name OrbitDisk -I "$build_root/source/Sources/CSQLite" \
  "$build_root"/source/Sources/OrbitCore/*.swift "$build_root/$harness" \
  -o "$build_root/$binary"

python3 - "$build_root" "$harness" "$binary" <<'PY'
import hashlib
import json
from pathlib import Path
import subprocess
import sys

build = Path(sys.argv[1])
harness, binary = sys.argv[2:]
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
record = {
    'probe': 'discovery', 'mode': 'producer+stage',
    'patches': ['AsyncListingProducer worker clamp raised from 4 to 64 (benchmark copy only)'],
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
