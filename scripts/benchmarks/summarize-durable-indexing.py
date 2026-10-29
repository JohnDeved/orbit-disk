#!/usr/bin/env python3
import json
from pathlib import Path
import statistics
import sys

source = Path(sys.argv[1])
rows = [json.loads(line) for line in (source / 'results.jsonl').read_text().splitlines()]
groups = {}
for row in rows:
    groups.setdefault((row['scenario'], row['label']), []).append(row)
summary = []
for (scenario, label), samples in groups.items():
    phases = sorted({name for row in samples for name in row['phase_seconds']})
    elapsed = [row['ingestion_seconds'] for row in samples]
    entry = {
        'scenario': scenario, 'label': label, 'samples': len(samples),
        'ingestion_median_seconds': statistics.median(elapsed),
        'ingestion_range_seconds': [min(elapsed), max(elapsed)],
        'total_ingest_publish_median_seconds': statistics.median(
            row['ingestion_seconds'] + row['phase_seconds']['publication'] for row in samples),
        'records_per_second_median': statistics.median(row['records_per_second'] for row in samples),
        'wal_max_sampled_bytes': max(row['wal_max_sampled_bytes'] for row in samples),
        'phase_median_seconds': {phase: statistics.median(row['phase_seconds'].get(phase, 0)
                                                        for row in samples) for phase in phases},
        'validation_passed': all(row['validation']['quick_check'] == 'ok' and
                                 row['validation']['pending_work'] == 0 for row in samples),
    }
    summary.append(entry)
    print(f'{scenario:14} {label:10} n={len(samples)} '
          f'{entry["ingestion_median_seconds"]:.3f}s '
          f'[{min(elapsed):.3f}, {max(elapsed):.3f}] '
          f'{entry["records_per_second_median"]:,.0f} records/s '
          f'WALmax {entry["wal_max_sampled_bytes"] / 1048576:.1f} MiB')
(source / 'summary.json').write_text(json.dumps(summary, indent=2, sort_keys=True) + '\n')
