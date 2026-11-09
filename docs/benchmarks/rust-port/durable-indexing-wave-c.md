# Durable indexing: Rust port vs Swift v0.9.15 (Wave C)

Measured 2026-11-09 on an Apple M5 (10 cores, 16 GB). Three fresh-process rounds per shape and version, interleaved, with the existing runner (`scripts/benchmarks/run-durable-indexing.py`) and summarizer. Each fixture was deleted after its result was recorded. Marking used n=9.

**Other builds ran concurrently (load 7–10).** Absolute times are inflated for both sides: Swift's small shape measured 1.37 s here, against its idle median of 1.01 s. The interleaved ratio is the meaningful comparison. An idle-machine rerun is scheduled for the cutover report.

All samples passed validation: exact totals, drained queues, and SQLite `quick_check`.

| Shape | Rust median [range] | Swift median [range] | Speedup | Peak WAL Rust / Swift |
|---|---|---|---:|---|
| 1,000,000 files, one folder | 5.58 s [4.19, 6.04] | 9.12 s [8.78, 13.30] | 1.6× | 65.8 / 59.1 MiB |
| 100k files / 10k folders, gen 0 | 0.700 s [0.696, 0.708] | 1.369 s [1.368, 1.374] | 2.0× | 64.8 / 64.4 MiB |
| Same, gen 1 | 0.694 s [0.694, 0.712] | 1.358 s [1.354, 1.379] | 2.0× | 64.8 / 64.4 MiB |
| 10k empty folders, gen 0 | 0.122 s [0.117, 0.128] | 0.303 s [0.302, 0.309] | 2.5× | 17.2 / 23.7 MiB |
| Same, gen 1 | 0.123 s [0.119, 0.123] | 0.308 s [0.301, 0.309] | 2.5× | 17.2 / 23.7 MiB |
| Refresh marking, 50k dirs | 0.061 s [0.060, 0.064] | 0.106 s [0.088, 0.117] | 1.7× | – |
| Refresh marking, 100k dirs | 0.129 s [0.123, 0.137] | 0.186 s [0.177, 0.201] | 1.4× | – |

Published database size: 171 vs 191 MiB (flat), 20.7 vs 24.0 MiB (small folders).

## What made the difference

New schema:
- one `dirs` row per traversable directory, replacing Swift's `directories`, `directory_paths` and `epochs`
- BLOB names
- no `AUTOINCREMENT`
- directory totals updated by a recursive CTE instead of a trigger
- single-link files charged at insert, after a unique identity probe

Writer:
- keyset range marking: one `MAX(path)` range plus a range `UPDATE`
- fresh-insert and probe statements held for the whole batch

Rejected after measurement:
- identity probe folded into `INSERT … RETURNING` (~50% slower)
- `page_size` 2048 (slower) and 8192 (within noise)

The flat shape is now about 38% I/O (WAL writes and fsync), driven by the two size-ordered indexes.

## Other measurements in this wave
- Discovery into the live inventory (`discovery_probe`, also under load):
  - flat 60k: 70 ms, versus Swift's documented 159 ms
  - mixed 90k: 270 ms, versus 160 ms
  - The mixed gap is under investigation: kernel-serialized directory opens, and ancestor revalidation, now reduced to one `fstatat` (00b66a9).
- Search accelerator, 1M names: build 54–66 ms; typical queries 1–3 ms.
- Name sort (collation keys): 100k in 32 ms; 1M in 427 ms.
- Sunburst hit test: 21 ns/query. Fast-path layout of a 1M-child folder: 1.2 ms.
- GPUI sunburst paint: 0.27 ms/frame for 377 segments; frame interval p99 5.9 ms.
