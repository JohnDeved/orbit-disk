# Whole-drive mapping

The figures in the README's "Fast and native" section come from mapping a real, full Mac, not a generated tree.

**Machine:** Apple M5 (10 cores), 16 GB, macOS 27.0.1. Internal 500 GB SSD; the scans could read 5.1 million files on it. It was the development Mac, four hours after a restart, with other apps open. The four runs ran back to back; the load average was 4.5–5.3 while they ran.

**Method:** `e2e_probe run` (crates/orbit-bench) maps a folder exactly as the app does, through `MappingSession`, and then waits until the index is saved. Its root was a symlink to `/`, so it mapped the whole drive (the probe canonicalizes the root). It used the default worker count (8) and a fresh cache, and deleted the cache afterwards. Like the app, it reads views at most once per 120 Hz frame while mapping. The terminal ran without Full Disk Access, so folders macOS protects were skipped, as they are in the app without it. The runs were on 2026-11-14.

```sh
cargo build --release -p orbit-bench --bin e2e_probe
mkdir -p ~/orbit-drive && ln -s / ~/orbit-drive/root
target/release/e2e_probe run --dir ~/orbit-drive
```

| | Run 1 | Run 2 | Run 3 | Run 4 |
|---|---:|---:|---:|---:|
| Files | 5,114,650 | 5,114,650 | 5,114,650 | 5,115,097 |
| First view (engine) | 33 ms | 18 ms | 19 ms | 18 ms |
| Discovery (all readable files found) | 18.5 s | 19.4 s | 18.7 s | 21.0 s |
| Files discovered per second | 276,700 | 264,000 | 273,600 | 243,400 |
| Root map complete | 27.5 s | 29.1 s | 28.5 s | 31.6 s |
| Indexed and saved | 32.2 s | 34.2 s | 37.3 s | 36.4 s |
| Peak memory | 1,691 MiB | 1,637 MiB | 1,635 MiB | 1,627 MiB |
| CPU, user / system | 31.1 / 76.8 s | 32.4 / 87.4 s | 32.7 / 84.9 s | 34.2 / 87.4 s |

The README rounds discovery to about 270,000 files per second and 19 s (the median), and the first view to about 20 ms. "Files" counts what the probe could read without Full Disk Access. "First view" is the engine's first non-empty listing of `/` (`first_visible_view_ms`), measured without app launch or drawing.

## Compared with 0.11.3

The previous figures (same Mac, 5.3 million files, four runs) were 42–45 s to find every file, 50–51 s to a saved map, 714–866 MiB peak memory and an 11–16 ms first view. Three changes account for the difference:

- **Eight discovery workers instead of four.** Discovery alone (`discovery_probe walk --root / --workers N`) found the whole Mac in 26–28 s with four workers and 17–19 s with eight; 12 and 16 were no faster. Opening a folder for reading is serialized across the system (about 18 µs each), and the rest of a listing is disk and kernel time, which more workers overlap.
- **Size indexes built once, at the end.** A new map no longer maintains its two size indexes row by row; they are built when the map is sealed. The sort spills to disk, with a smaller page cache, so it adds about 70 MB instead of about 500 MB.
- **Fewer root checks.** The writer validates the mapped root once per transaction group (and again before it commits) instead of before every folder.

Files are now found faster than the index can save them, so listings queue for the writer. On this Mac the queue peaked at 820–930 MiB, which is most of the higher peak memory. Discovery waits once the queue holds 1 GiB (`PENDING_LIMIT`), which bounds memory on larger drives without slowing this one. Lower limits trade discovery time for memory and save no sooner: with 512 MiB, files were found in 21–22 s and peak memory was 1.4–1.5 GB; with 256 MiB, 24 s and about 1.2 GB. Saving took about 33 s with every limit.

The views read while mapping stayed fast: the median view took 0.11–0.14 ms and the 95th percentile 0.36–0.56 ms.

The 120 Hz figure is from the 0.10.0 release measurements ([release notes](../releases/v0.10.0.md#smoother-interface)): hovering, scrolling, opening folders and dragging had a 99th-percentile frame interval of 6.2 ms, with the machine under load. A 120 Hz frame allows 8.3 ms.

Speed depends on the drive, the number of folders and files, how long the Mac has been running and what else it is doing. On a Mac that had been running for 12 days, the kernel ran folder opens and attribute reads almost serially ([discovery scaling](rust-port/discovery-scaling.md)). A cold start after a reboot, an external disk or a network volume will be slower.

Raw output (`e2e_probe run`, runs 1 and 4, one JSON line per run):

```json
{"cache_hits":0,"directories":708578,"discovery_cancelled":false,"discovery_ms":18487.741875,"files":5114650,"first_full_view_ms":27473.830166,"first_visible_view_ms":32.980916,"peak_pending_mib":834.913122177124,"peak_rss_mib":1690.84375,"snapshot":"63A49ED9-1005-452C-A66F-676126D15D52","start_ms":19.863083,"system_cpu_s":76.777805,"total_to_publish_ms":32177.239166,"user_cpu_s":31.089491,"view_max_ms":6.994958,"view_p50_ms":0.114875,"view_p95_ms":0.35750000000000004,"views_while_mapping":4528,"writer_wait_ms":0.0}
{"cache_hits":0,"directories":708578,"discovery_cancelled":false,"discovery_ms":21012.952208000002,"files":5115097,"first_full_view_ms":31581.961792000002,"first_visible_view_ms":17.613875,"peak_pending_mib":927.3702917098999,"peak_rss_mib":1626.78125,"snapshot":"C32658D6-CF12-412A-B63E-BD35FFAB9541","start_ms":7.927958,"system_cpu_s":87.41466,"total_to_publish_ms":36399.667,"user_cpu_s":34.176122,"view_max_ms":39.035333,"view_p50_ms":0.1385,"view_p95_ms":0.5612090000000001,"views_while_mapping":5628,"writer_wait_ms":0.0}
```
