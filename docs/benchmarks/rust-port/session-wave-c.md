# Mapping session: Rust port vs Swift v0.9.15 (Wave C integration)

Measured 2026-11-09 on an Apple M5 (10 cores, 16 GB), release builds (`lto = "fat"`), fresh
processes. Load average 1.7–3.5 during the runs (other agents were editing, not building).
Fixtures were deleted after each measurement.

## Pipeline probe (discovery and writer overlapping through the session)

`crates/orbit-bench/src/bin/pipeline_probe.rs` is the port of
`Benchmarks/DurableIndexing/PipelineProbe.swift` (tag `v0.9.15`): a full `MappingSession` (stage open, removal
journal anchoring, discovery workers, one writer thread, publication under the journal fence),
then verification of the published database. Run interleaved with the Swift binary by
`scripts/benchmarks/run-indexing-pipeline.py --rounds 3`. Like Swift, the probe keeps the stage
after publication (the app's session retires it, untimed).

| Shape | Rust total, median [range] | Swift total, median [range] | Speedup | Rust / Swift discovery | Rust / Swift ready-to-publish |
|---|---|---|---:|---|---|
| 1,000 folders × 10 files | 0.091 s [0.076, 0.100] | 0.166 s [0.152, 0.190] | 1.8× | 0.018 / 0.026 s | 0.071 / 0.118 s |
| 2,000 folders × 10 files | 0.219 s [0.210, 0.244] | 0.350 s [0.347, 0.370] | 1.6× | 0.030 / 0.051 s | 0.183 / 0.268 s |

Swift's documented idle medians are 0.154 s and 0.335 s, so the Swift side ran near idle speed.
Publication: 0.011 / 0.017 s (1k) and 0.041 / 0.046 s (2k). In both versions the writer, not
discovery, bounds the pipeline (Rust drains 0.15 s after discovery ends at 2k folders).

## End-to-end session (`e2e_probe`)

Generated tree: 179,965 files in 4,463 folders, mixed depth up to 8, mixed widths (a few
folders with 400–2,000 files), NFD names; files are empty or sparse so the fixture costs no
blocks. One reader polls the root and a second-level folder once per change (at most every
16 ms), like a UI. Three fresh-process runs:

| Metric | Rust (3 runs) | Swift reference (real Documents, 179k files) |
|---|---|---|
| First visible view (root with children) | 3.7 / 3.9 / 9.0 ms | – |
| First full view (root complete) | 201 / 213 / 219 ms | 1.68 s |
| Discovery | 197 / 210 / 210 ms | 1.46 s |
| Total to publication | 3.16 / 3.35 / 5.08 s | 9.74 s |
| Peak RSS | 64.5–64.8 MiB | ~116 MB |
| User / system CPU | 0.90 / 2.49 s | – |
| View latency while mapping, p50 / p95 / max | 0.03 / 0.15 / 0.64 ms (≈4,100 views) | – |

The trees differ (Swift's was a real Documents folder with more, smaller folders), so the
comparison is indicative only. System CPU is dominated by the writer's WAL writes and `fsync`s
(`synchronous=FULL`); the 5.1 s run coincided with a load spike.

## `discovery_probe` after the `child_identity` change (00b66a9)

`discovery_probe bench --workers 4 --samples 7`, discovery into the live inventory only:

| Fixture | Rust now, median (range) | Wave C (under load 7–10) | Swift documented |
|---|---|---|---|
| mixed, 90,068 records | 42.4 ms (41.8–42.6) | 270 ms | 160 ms |
| flat, 60,008 records | 51.8 ms (51.7–52.5) | 70 ms | 159 ms |

The mixed-shape gap reported in Wave C is gone: with ancestor revalidation reduced to one
`fstatat` and an idle machine, mixed discovery is 3.8× faster than Swift.

## Search with removals

Saved-map search with removals projects in Rust (one pass over the matches with their parent
paths, top-k on the fly) instead of Swift's TEMP tables. Not separately benchmarked yet; the
cost is one `instr` scan plus a hash lookup per ancestor of each match.
