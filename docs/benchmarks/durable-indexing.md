# Durable indexing measurements

Measured on October 29, 2026, against the published v0.9.14 core. The selected candidate uses grouped leaf-listing commits, group-scoped prepared statements, keyset refresh marking, and fixes for retained SQLite statements and repeated refresh enumeration. Automatic checkpoints run at 16,384 pages (about 64 MiB with the measured 4 KiB pages); SQLite FULL synchronization remains enabled.

Three fresh-process runs per version and shape were interleaved in alternating order. Inputs were already in memory before the synthetic ingestion timer. All 30 final samples passed exact file/node/root totals, drained work queues and SQLite `quick_check`. These are local measurements, not universal speed claims or CI timing limits.

The table includes ingestion **and publication**. Setup and verification remain separate in the raw data.

| Input | v0.9.14 median | Candidate median | Change |
|---|---:|---:|---:|
| 100,000 files in 10,000 small folders, generation 0 | 3.814 s | 1.370 s | 2.78× faster |
| Same, generation 1 | 5.803 s | 1.163 s | 4.99× faster |
| 10,000 empty folders, generation 0 | 1.416 s | 0.381 s | 3.72× faster |
| Same, generation 1 | 1.801 s | 0.389 s | 4.63× faster |
| 1,000,000 files in one folder | 8.357 s | 8.244 s | Similar |

For the flat shape, the final ingestion range was 8.222–8.739 s versus baseline 8.006–8.755 s. The small difference in medians is not a convincing flat-folder speedup. Final publication medians across the shapes were 10–18 ms, so the small-folder savings remain after finalization.

Sampled peak WAL fell from 1,390 MiB to 59.1 MiB for the million-file shape and from 1,469 MiB to 64.8 MiB for fresh small folders. These are sampled maxima, not a guaranteed global size cap: readers can still delay checkpoint progress.

## Checkpoint selection

Fixing active SQLite reads makes checkpoints run during ingestion; the old writer deferred substantial work and grew a much larger journal. Keeping the original 1,000-page threshold regressed flat ingestion. A separate interleaved control tested both larger thresholds without changing FULL synchronization.

| Flat-folder control | Ingestion + publication median | Publication median | Sampled peak WAL |
|---|---:|---:|---:|
| v0.9.14 | 8.357 s | 5.0 ms | 1,390 MiB |
| Pooled writer, 1,000 pages | 11.353 s | 9.7 ms | 4.6 MiB |
| Pooled writer, 4,096 pages | 9.588 s | 13.7 ms | 17.3 MiB |
| Selected writer, 16,384 pages | 8.244 s | 15.4 ms | 59.1 MiB |

The 16,384-page setting retained flat throughput while reducing journal growth substantially. Its fresh small-folder total was 1.370 s versus 1.534 s for the original-threshold pooled writer, so it did not trade away that gain. Controls and rejected candidates remain in the evidence.

## Actual mapping pipeline

A second fixture uses 1,000 real folders containing 10 files each. It starts discovery and immediately advances `ResumableStage`, so enumeration and writing overlap normally. Both versions use matching optimized monolithic builds; fixture creation is outside the timer.

| Metric | v0.9.14 median | Candidate median |
|---|---:|---:|
| Mapping ready | 0.321 s | 0.135 s |
| Initialization through publication | 0.346 s | 0.154 s |

The candidate performed 32 grouped commits in each run. Both versions visited all 1,001 directories exactly once through the producer, with no serial fallback, and published the exact expected counts. The candidate's total range was 0.146–0.162 s. Discovery and writer intervals overlap and must not be added together. This confirms the optimization reaches the actual mailbox, secure-cursor and callback path; it does not include SwiftUI delivery or a whole-Mac scan.

## Refresh marking

The marking-only fixture starts with existing directory rows and excludes population and pending-bit reset from timing. Every round verifies the exact generation and pending count.

| Existing directories, including root | Previous marking median | Keyset median | Change |
|---|---:|---:|---:|
| 50,001 | 0.492 s | 0.110 s | 4.46× faster |
| 100,001 | 1.670 s | 0.223 s | 7.49× faster |

The previous marking implementation is unchanged from v0.9.14; this isolated comparison uses a retained module containing the earlier checkpoint fix. Both marking variants retained the original 1,000-page checkpoint threshold, isolating the keyset change from later tuning. Keyset traversal avoids repeatedly scanning rows already marked in the same refresh. A separate 257-directory resumed-stage check found that the old generation-1 path enumerated every directory twice. The candidate enumerates each once; that improvement is not included in the synthetic ingestion table.

## Evidence and limits

[Raw results and exact source hashes](durable-indexing.json) include all final synthetic and real-pipeline samples, marking samples, and compact summaries of the intermediate candidates. Full intermediate records are retained in the local work evidence. The first checkpoint-only candidate reduced WAL growth but slowed fresh small-folder ingestion; grouping and statement reuse were measured separately afterward.

The frozen baseline was originally captured at v0.9.13, which was not published. All 39 core/SQLite source hashes match published v0.9.14 exactly. Final baseline and candidate harnesses use the same generated metadata and compiler settings. Machine load and substantial preexisting swap affected absolute results across measurement windows, so comparisons use interleaved controls within each window. Tests do not establish cold-disk, deep-tree, hardlink-heavy or concurrent-deletion throughput.

[Probe source and reproduction commands](../../Benchmarks/DurableIndexing/README.md) document construction, timing boundaries, correctness checks and cleanup. Generated databases are excluded from the repository; numerical evidence and source hashes are retained.
