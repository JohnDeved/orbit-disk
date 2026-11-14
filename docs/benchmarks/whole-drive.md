# Whole-drive mapping

The figures in the README's "Fast and native" section come from mapping a real, full Mac, not a generated tree.

**Machine:** Apple M5 (10 cores), 16 GB, macOS 27.0.1. Internal 500 GB SSD; the Data volume had 354 GB in use, and the scans could read 5.1 million files on it. It was the development Mac, with other apps open.

**Method:** `e2e_probe run` (crates/orbit-bench) maps a folder exactly as the app does, through `MappingSession`, and then waits until the index is saved. Its root was a symlink to `/`, so it mapped the whole drive (the probe canonicalizes the root). It used the default worker count and a fresh cache under `/tmp`, and deleted the cache afterwards. The terminal ran without Full Disk Access, so folders macOS protects were skipped, as they are in the app without it. The two runs were back to back on 2026-11-13; the second was slower even though more metadata was cached, so read the spread as load from other apps.

```sh
cargo build --release -p orbit-bench --bin e2e_probe
mkdir -p /tmp/orbit-drive && ln -s / /tmp/orbit-drive/root
target/release/e2e_probe run --dir /tmp/orbit-drive
```

| | Run 1 | Run 2 |
|---|---:|---:|
| Files | 5,130,575 | 5,130,784 |
| Folders | 701,461 | 701,496 |
| First view (engine) | 11 ms | 9 ms |
| Discovery (all readable files found) | 53.4 s | 55.2 s |
| Files discovered per second | 96,100 | 93,000 |
| Root map complete | 59.4 s | 81.7 s |
| Indexed and saved | 60.0 s | 82.1 s |
| Folder view while mapping, p50 / p95 / max | 0.04 / 0.14 / 8.3 ms | 0.07 / 0.23 / 73 ms |
| Peak memory | 708 MiB | 686 MiB |
| CPU, user / system | 74.4 / 66.0 s | 81.4 / 80.3 s |

The README rounds discovery to about 95,000 files per second and 55 s. "Files" counts what the probe could read without Full Disk Access. "First view" is the engine's first non-empty listing of `/` (`first_visible_view_ms`), measured without app launch or drawing. Saving the index varied more between the runs (60 s and 82 s) than finding the files did.

The 120 Hz figure is from the 0.10.0 release measurements ([release notes](../releases/v0.10.0.md#smoother-interface)): hovering, scrolling, opening folders and dragging had a 99th-percentile frame interval of 6.2 ms, with the machine under load. A 120 Hz frame allows 8.3 ms.

Discovery is bound by the kernel: opening folders and reading attributes are serialized system-wide, so more workers don't speed up an internal SSD ([discovery scaling](rust-port/discovery-scaling.md)). Speed depends on the drive, the number of folders and files, and what else the Mac is doing. A cold start after a reboot, an external disk or a network volume will be slower.

Raw output (`e2e_probe run`, one JSON line per run):

```json
{"cache_hits":0,"directories":701461,"discovery_cancelled":false,"discovery_ms":53363.14204100001,"files":5130575,"first_full_view_ms":59359.435249999995,"first_visible_view_ms":11.046,"peak_rss_mib":708.0625,"snapshot":"E20D1DCF-AB37-4E70-ACAF-965919706222","start_ms":10.425208,"system_cpu_s":66.030603,"total_to_publish_ms":60036.294499999996,"user_cpu_s":74.383658,"view_max_ms":8.260375,"view_p50_ms":0.042874999999999996,"view_p95_ms":0.139958,"views_while_mapping":696446}
{"cache_hits":0,"directories":701496,"discovery_cancelled":false,"discovery_ms":55166.748333999996,"files":5130784,"first_full_view_ms":81666.81854200001,"first_visible_view_ms":8.985458,"peak_rss_mib":686.234375,"snapshot":"D1170C2E-0B01-4429-8D47-6D179BE9C4B9","start_ms":8.672125,"system_cpu_s":80.322071,"total_to_publish_ms":82146.927542,"user_cpu_s":81.374688,"view_max_ms":73.129333,"view_p50_ms":0.074667,"view_p95_ms":0.230292,"views_while_mapping":633322}
```
