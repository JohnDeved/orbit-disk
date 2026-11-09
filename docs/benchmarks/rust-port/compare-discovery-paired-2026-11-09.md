# Rust vs Swift v0.9.15: `cargo xtask bench-compare`

Measured 2026-11-09T18:25:09Z on Apple M5 (10 cores, 16.0 GiB, macOS 27.0.1), commit `1e77b39`, rustc 1.99.1 (58cf8f9c3 2026-10-13).

5 fresh-process round(s), implementations interleaved (order alternates per round). Load average before: 3.83 5.52 8.07; after: 3.90 5.39 7.95.

| Suite | Scenario | Metric | Rust median [min, max] | Swift median [min, max] | Speedup | n (Rust/Swift) |
|---|---|---|---|---|---:|---|
| discovery | flat (4 workers) | discovery | 54.3 ms [53.6 ms, 56.9 ms] | 64.1 ms [64.0 ms, 65.8 ms] | 1.18× | 5/5 |
| discovery | flat (4 workers) | user CPU | 3.0 ms [3.0 ms, 3.1 ms] | 13.7 ms [13.6 ms, 13.8 ms] | 4.52× | 5/5 |
| discovery | flat (4 workers) | system CPU | 51.7 ms [51.1 ms, 54.3 ms] | 52.5 ms [52.4 ms, 54.3 ms] | 1.01× | 5/5 |
| discovery | flat (4 workers) | peak RSS | 18.6 MiB [18.5 MiB, 18.8 MiB] | 16.6 MiB [16.6 MiB, 16.6 MiB] | Swift/Rust 0.89 | 5/5 |
| discovery | flat (8 workers) | discovery | 54.1 ms [53.6 ms, 56.0 ms] | 64.4 ms [64.2 ms, 69.5 ms] | 1.19× | 5/5 |
| discovery | flat (8 workers) | user CPU | 3.2 ms [3.1 ms, 3.3 ms] | 13.4 ms [13.0 ms, 13.7 ms] | 4.20× | 5/5 |
| discovery | flat (8 workers) | system CPU | 52.8 ms [52.2 ms, 54.4 ms] | 53.4 ms [52.9 ms, 58.6 ms] | 1.01× | 5/5 |
| discovery | flat (8 workers) | peak RSS | 19.0 MiB [18.9 MiB, 19.2 MiB] | 16.7 MiB [16.7 MiB, 16.8 MiB] | Swift/Rust 0.88 | 5/5 |
| discovery | flat (1 worker) | discovery | 54.5 ms [52.9 ms, 55.4 ms] | 64.4 ms [63.6 ms, 69.5 ms] | 1.18× | 5/5 |
| discovery | flat (1 worker) | user CPU | 2.9 ms [2.9 ms, 3.0 ms] | 13.6 ms [13.5 ms, 13.8 ms] | 4.69× | 5/5 |
| discovery | flat (1 worker) | system CPU | 51.6 ms [49.9 ms, 52.4 ms] | 52.8 ms [51.7 ms, 58.1 ms] | 1.02× | 5/5 |
| discovery | flat (1 worker) | peak RSS | 18.3 MiB [18.2 MiB, 18.4 MiB] | 16.5 MiB [16.5 MiB, 16.5 MiB] | Swift/Rust 0.90 | 5/5 |
| discovery | mixed (4 workers) | discovery | 198.9 ms [194.1 ms, 208.5 ms] | 265.7 ms [262.0 ms, 278.3 ms] | 1.34× | 5/5 |
| discovery | mixed (4 workers) | user CPU | 15.9 ms [15.6 ms, 17.8 ms] | 95.1 ms [91.7 ms, 104.7 ms] | 5.99× | 5/5 |
| discovery | mixed (4 workers) | system CPU | 216.7 ms [213.3 ms, 254.0 ms] | 309.0 ms [296.2 ms, 326.0 ms] | 1.43× | 5/5 |
| discovery | mixed (4 workers) | peak RSS | 24.6 MiB [24.6 MiB, 24.9 MiB] | 25.0 MiB [24.9 MiB, 25.0 MiB] | Swift/Rust 1.01 | 5/5 |
| discovery | mixed (8 workers) | discovery | 196.6 ms [193.0 ms, 212.9 ms] | 274.9 ms [263.8 ms, 277.0 ms] | 1.40× | 5/5 |
| discovery | mixed (8 workers) | user CPU | 19.0 ms [16.9 ms, 20.5 ms] | 107.9 ms [103.3 ms, 110.1 ms] | 5.67× | 5/5 |
| discovery | mixed (8 workers) | system CPU | 264.1 ms [229.7 ms, 281.3 ms] | 386.4 ms [381.2 ms, 397.2 ms] | 1.46× | 5/5 |
| discovery | mixed (8 workers) | peak RSS | 25.0 MiB [25.0 MiB, 25.2 MiB] | 25.1 MiB [25.0 MiB, 25.2 MiB] | Swift/Rust 1.00 | 5/5 |
| discovery | mixed (1 worker) | discovery | 192.2 ms [190.4 ms, 226.9 ms] | 256.5 ms [255.5 ms, 259.0 ms] | 1.33× | 5/5 |
| discovery | mixed (1 worker) | user CPU | 10.3 ms [10.1 ms, 10.7 ms] | 63.7 ms [63.5 ms, 64.0 ms] | 6.19× | 5/5 |
| discovery | mixed (1 worker) | system CPU | 125.1 ms [124.0 ms, 137.1 ms] | 178.0 ms [177.1 ms, 188.6 ms] | 1.42× | 5/5 |
| discovery | mixed (1 worker) | peak RSS | 24.3 MiB [24.3 MiB, 24.5 MiB] | 24.8 MiB [24.8 MiB, 24.8 MiB] | Swift/Rust 1.02 | 5/5 |
| pipeline | 1000 folders x 10 files | total | 61.8 ms [59.6 ms, 64.9 ms] | 142.8 ms [138.5 ms, 155.2 ms] | 2.31× | 5/5 |
| pipeline | 1000 folders x 10 files | discovery | 20.7 ms [16.5 ms, 23.3 ms] | 32.2 ms [30.3 ms, 34.0 ms] | 1.56× | 5/5 |
| pipeline | 1000 folders x 10 files | ready to publish | 53.1 ms [52.1 ms, 55.4 ms] | 117.6 ms [114.1 ms, 120.5 ms] | 2.21× | 5/5 |
| pipeline | 2000 folders x 10 files | total | 126.2 ms [121.4 ms, 127.8 ms] | 272.7 ms [270.7 ms, 279.9 ms] | 2.16× | 5/5 |
| pipeline | 2000 folders x 10 files | discovery | 39.3 ms [30.4 ms, 41.6 ms] | 63.8 ms [63.4 ms, 67.3 ms] | 1.62× | 5/5 |
| pipeline | 2000 folders x 10 files | ready to publish | 116.5 ms [111.8 ms, 117.7 ms] | 244.9 ms [242.5 ms, 249.6 ms] | 2.10× | 5/5 |

Speedup is the Swift median divided by the Rust median (higher means Rust is faster).

- discovery: discovery into the live inventory only (no writer). Swift runs `AsyncListingProducer` with a `LiveMappingInventory` (Benchmarks/Discovery/DiscoveryProbe.swift); its four-worker clamp is raised in the benchmark copy so 8 workers mean 8. CPU rows are process user/system time over the same span. The unpaired 2026-10-28 Swift numbers in docs/benchmarks/parallel-discovery.json are not comparable (see docs/benchmarks/rust-port/discovery-scaling.md).

Raw samples: `results.jsonl`; probe stderr and runner output: `raw/`; machine details: `environment.json`.
