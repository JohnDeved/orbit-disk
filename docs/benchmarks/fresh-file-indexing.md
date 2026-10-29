# Fresh-file indexing follow-up

Measured on October 29, 2026. This pass compares the final writer with the previous optimized implementation (`f09e48e`) and published v0.9.14 in the same measurement window. The [earlier indexing measurements](durable-indexing.md) remain unchanged.

The changes use a smaller INSERT after confirming a file has no existing row, skip unnecessary deletion bookkeeping when a completed listing contains no stale entries, and avoid opening a second directory descriptor when the writer only needs validation. Existing-file updates, hardlink accounting, revision checks, rollback and crash recovery retain their existing paths. SQLite FULL synchronization and the 16,384-page checkpoint setting are unchanged from `f09e48e`.

## Already-discovered records

Three fresh-process runs per version and shape were interleaved in alternating order. Metadata generation happens before timing. All 45 samples passed exact file/node/root totals, empty pending-work queues and SQLite `quick_check`.

The table reports median **ingestion plus publication** time. Setup and verification are excluded and retained separately in the raw data.

| Input | v0.9.14 | Previous optimized | Final | Cumulative speedup |
|---|---:|---:|---:|---:|
| 1,000,000 files in one folder | 6.636 s | 6.657 s | 5.560 s | 1.19× |
| 100,000 files in 10,000 folders, generation 0 | 3.143 s | 1.114 s | 1.013 s | 3.10× |
| Same, generation 1 | 5.459 s | 1.108 s | 1.043 s | 5.23× |
| 10,000 empty folders, generation 0 | 1.563 s | 0.357 s | 0.333 s | 4.69× |
| Same, generation 1 | 1.825 s | 0.392 s | 0.332 s | 5.50× |

The clearest incremental gain is the million-file folder: 16.5% less time than `f09e48e`, with non-overlapping total ranges of 5.434–5.569 s versus 6.557–6.758 s. The small-folder medians improve by 6–9%; their ranges overlap. Empty-folder runs are noisier, so their small incremental differences should not be treated as stable forecasts. The larger cumulative gains include the earlier grouped commits, statement reuse, refresh ordering and checkpoint fixes.

Final publication medians were 11–17 ms. Sampled peak WAL remained 59.1 MiB for the million-file folder and 64.4 MiB for fresh small folders, versus 1,390 MiB and 1,465 MiB in v0.9.14. These samples are not hard journal-size limits; readers can still delay checkpoints.

## Actual mapping pipeline

Five alternating runs per variant used 2,000 real folders containing ten files each. Discovery and writing overlap normally through `ResumableStage`; fixture creation is excluded. Both revisions and the intermediate variants use matching optimized monolithic builds.

| Initialization through publication | Median | Range |
|---|---:|---:|
| v0.9.14 | 0.767 s | 0.726–0.799 s |
| Previous optimized (`f09e48e`) | 0.356 s | 0.324–0.386 s |
| Directory-validation change alone | 0.355 s | 0.324–0.467 s |
| Final combined changes | 0.335 s | 0.277–0.360 s |

The final median is 2.29× faster than v0.9.14 and about 6% lower than the previous optimized implementation. The directory-validation change alone has no convincing standalone timing gain in this fixture. All 20 runs published the exact counts, visited all 2,001 directories once through the producer, and required no serial fallback. The optimized variants made 63 grouped leaf commits per run. Discovery and writer intervals overlap and must not be added together. SwiftUI delivery and whole-Mac scans are outside this test.

## Stale records and rejected approach

A separate control seeds 40,000 files outside the timer, then relists 20,000 retained files and removes the other half from the index. Timing includes trim, hardlink-owner reconciliation and publication. Three alternating pairs gave 1.615 s for `f09e48e` and 1.597 s for the final writer, with exact published totals and clean queues. This control did not show a stale-cleanup regression.

An earlier candidate added a separate “any stale row?” query before the existing cleanup queries. In its own paired window, that increased stale-cleanup total time by 3.8%, from 1.957 s to 2.032 s. It was rejected. The final version reuses the existing stale-directory and sum queries, then skips the dirty-link/delete queries when the sum reports no stale files. It adds no extra prefix scan to a nonempty cleanup batch.

SQL operation-count profiling supported the changes: fresh-file insertion fell from 107 to 95 SQLite VM steps per file, and the complete 100,000-file profile fell from 16.22 million to 14.02 million VM steps. These are operation counts, not CPU time. PROFILE durations for SELECT statements include their lifetimes until reset or finalization and were not summed as execution time. Profiled runs are excluded from the timing tables.

## Evidence and reproduction

[Raw samples, source hashes and controls](fresh-file-indexing.json) include all 45 final synthetic samples, 20 pipeline samples, six stale-control samples, intermediate results and the exact benchmark patch for the stale fixture. The public file contains repository-relative source paths. Generated databases are not included.

Use the [standard probe commands](../../Benchmarks/DurableIndexing/README.md) with three labeled binaries: v0.9.14, `f09e48e`, and the final source. Use three rounds for the five synthetic shapes; use five rounds and `--directories 2000` for the pipeline. Build the stale fixture from the recorded harness patch, then run `flat 20000 0 0` three times per revision in alternating order. All variants retain the same compiler flags. The [previous marking-only evidence](durable-indexing.md#refresh-marking) isolates the keyset refresh improvement; it was not remeasured in this pass.

Measurements used an Apple Silicon Mac with 16 GiB RAM and approximately 5.4 GiB of preexisting swap usage. No app scan or other team compiler/test ran during the final timing window. Absolute times vary with filesystem caching and machine activity; comparisons above use interleaved controls from the same window. They do not establish cold-disk, deep-tree, hardlink-heavy, cloud-provider or concurrent-deletion throughput.
