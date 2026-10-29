#!/usr/bin/env python3
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument('--binary', action='append', required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--rounds', type=int, default=3)
parser.add_argument('--directories', type=int, default=1000)
parser.add_argument('--files-per-directory', type=int, default=10)
parser.add_argument('--cwd', type=Path)
args = parser.parse_args()
binaries = [entry.split('=', 1) for entry in args.binary]
args.output.mkdir(parents=True, exist_ok=False)
base = Path(tempfile.mkdtemp(prefix='OrbitPipelineBenchmark-', dir='/private/tmp'))
try:
    root = base / 'root'
    root.mkdir()
    fixture_started = time.monotonic()
    for directory in range(args.directories):
        folder = root / f'folder-{directory}'
        folder.mkdir()
        for file in range(args.files_per_directory):
            (folder / f'file-{file}.bin').write_bytes(b'orbit-benchmark\n')
    fixture_seconds = time.monotonic() - fixture_started
    with (args.output / 'results.jsonl').open('x') as destination:
        for round_index in range(args.rounds):
            ordered = binaries if round_index % 2 == 0 else list(reversed(binaries))
            for label, binary in ordered:
                name = f'r{round_index + 1}-{label}'
                cache = base / name
                started = time.time()
                completed = subprocess.run([
                    binary, str(root), str(cache),
                    str(args.directories * args.files_per_directory), str(args.directories + 1),
                ], cwd=args.cwd, text=True, capture_output=True)
                (args.output / f'{name}.stderr').write_text(completed.stderr)
                if completed.returncode:
                    raise SystemExit(f'{name} failed: {completed.stderr[-2500:]}')
                result = json.loads(completed.stdout)
                result.update(label=label, round=round_index + 1, fixture_seconds=fixture_seconds,
                              wall_start_unix=started, process_seconds=time.time() - started)
                destination.write(json.dumps(result, sort_keys=True) + '\n')
                destination.flush()
                print(f'{name}: discovery {result["discovery_seconds"]:.3f}s, '
                      f'mapping {result["mapping_ready_seconds"]:.3f}s, '
                      f'total {result["total_seconds"]:.3f}s, '
                      f'group commits {result["counters"].get("fresh-group-committed", 0)}', flush=True)
                shutil.rmtree(cache)
finally:
    shutil.rmtree(base)
