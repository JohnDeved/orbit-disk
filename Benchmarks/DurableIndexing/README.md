# Durable indexing probes

These macOS probes measure the production indexing code with private generated inputs. They do not inspect user folders or use the app's cache. Run one workload at a time, with no concurrent app scans or builds.

`IndexingProbe.swift` constructs metadata in RAM before timing and writes it through the production mirror schema. Shapes cover a million files in one folder, 100,000 files in 10,000 small folders, and 10,000 empty folders. The latter shapes run in generations 0 and 1. Generation marking is drained before ingestion, so it isolates fresh-listing eligibility from enumeration. Files have distinct identities, varied sizes/allocation and some accented names; this throughput fixture does not model hardlinks, deep ancestry or concurrent removals.

`PipelineProbe.swift` exercises `ResumableStage` with actual temporary files. Discovery and writing overlap normally. Test hooks count producer visits, fallback visits and grouped commits without inserting delays. The runner creates the fixture before timing, uses a fresh cache/process each sample and deletes its own fixture afterward. Discovery and writer times overlap and must not be added together.

`MarkingProbe.swift` populates existing directory rows, then measures only full-refresh marking. Setup and clearing the pending bit are excluded. Every round checks the exact new generation and pending-directory count.

All probes keep SQLite FULL synchronization, indexes and normal durability settings. Ingestion and publication have separate timers. Exact root/file/node totals and drained work queues are required; synthetic ingestion also runs SQLite `quick_check`. They are diagnostics, not CI timing gates.

## Reproduce

From the repository root, choose a new output directory outside the repository. `source-root` must contain `Sources/OrbitCore` and `Sources/CSQLite`; a Git archive or separate checkout can supply an older baseline. Each build captures immutable source copies and hashes. Use `single` for a baseline lacking grouped-writer APIs and `grouped` for the current implementation.

```sh
zsh scripts/benchmarks/build-durable-indexing.sh /path/to/baseline /tmp/orbit-bench/baseline single
zsh scripts/benchmarks/build-durable-indexing.sh "$PWD" /tmp/orbit-bench/current grouped
python3 scripts/benchmarks/run-durable-indexing.py \
  --binary baseline=/tmp/orbit-bench/baseline/IndexingProbe \
  --binary current=/tmp/orbit-bench/current/IndexingProbe \
  --rounds 3 --output /tmp/orbit-bench/paired
python3 scripts/benchmarks/summarize-durable-indexing.py /tmp/orbit-bench/paired
```

For the real-file pipeline, build both versions with the same monolithic compiler settings:

```sh
zsh scripts/benchmarks/build-durable-indexing.sh /path/to/baseline /tmp/orbit-bench/pipeline-baseline single pipeline
zsh scripts/benchmarks/build-durable-indexing.sh "$PWD" /tmp/orbit-bench/pipeline-current grouped pipeline
python3 scripts/benchmarks/run-indexing-pipeline.py \
  --binary baseline=/tmp/orbit-bench/pipeline-baseline/PipelineProbe \
  --binary current=/tmp/orbit-bench/pipeline-current/PipelineProbe \
  --rounds 3 --output /tmp/orbit-bench/pipeline-results
```

Build the marking probe with a fourth argument of `marking`, then run `MarkingProbe new-output-directory directory-count rounds` (for example, 50,000 and 100,000 directories, three rounds). Each output directory must be new.

The optional `profile` argument on a direct `IndexingProbe` invocation records SQLite statement counts, VM steps and durations. Profiled throughput is not comparable to ordinary runs. A SELECT left at ROW accumulates time until reset/finalization; that lifetime overlaps other operations and is not query CPU time.

WAL diagnostics sample file size and the duplicated shared-memory index header between synchronous writer calls. They do not open read transactions or force checkpoints. Values are sampled maxima. The older baseline can grow a large WAL, so small-folder runs deliberately stop at 10,000 directories. Generated databases can be deleted after retaining and verifying their JSON results.
