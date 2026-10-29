#!/usr/bin/env python3
import argparse
import json
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument('--binary', action='append', required=True, help='label=/absolute/path/IndexingProbe')
parser.add_argument('--rounds', type=int, default=3)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--shapes', nargs='+', default=['flat-million', 'small-g0', 'small-g1', 'empty-g0', 'empty-g1'])
args = parser.parse_args()
binaries = [value.split('=', 1) for value in args.binary]
shapes = {'flat-million': ('flat', 1000000, 0, 0), 'small-g0': ('small', 100000, 10000, 0),
          'small-g1': ('small', 100000, 10000, 1), 'empty-g0': ('empty', 0, 10000, 0),
          'empty-g1': ('empty', 0, 10000, 1)}
args.output.mkdir(parents=True, exist_ok=False)
with (args.output / 'results.jsonl').open('x') as destination:
    for round_index in range(args.rounds):
        ordered = binaries if round_index % 2 == 0 else list(reversed(binaries))
        for shape_name in args.shapes:
            shape, files, directories, generation = shapes[shape_name]
            for label, executable in ordered:
                name = f'r{round_index + 1}-{shape_name}-{label}'
                started = time.time()
                completed = subprocess.run([executable, str((args.output / name).absolute()), shape,
                                            str(files), str(directories), str(generation)],
                                           capture_output=True, text=True)
                (args.output / f'{name}.stderr').write_text(completed.stderr)
                if completed.returncode:
                    raise SystemExit(f'{name} failed: {completed.stderr[-2000:]}')
                result = json.loads(completed.stdout)
                result.update(label=label, round=round_index + 1, scenario=shape_name,
                              wall_start_unix=started, process_seconds=time.time() - started)
                destination.write(json.dumps(result, sort_keys=True) + '\n'); destination.flush()
                print(f'{name}: {result["ingestion_seconds"]:.3f}s ingestion, '
                      f'{result["phase_seconds"]["publication"]:.3f}s publish, '
                      f'{result["wal_max_sampled_bytes"] / 1048576:.1f} MiB WAL, '
                      f'{result["records_per_second"]:,.0f} records/s', flush=True)
