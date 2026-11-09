# UI frame timing (GPUI, live)

Measured 2026-11-09 on an Apple M5 (10 cores, 16 GB) with the release build: `OrbitDisk --frame-harness`, the built-in scenario, and the fixture backend with 190k entries.

**Two Rust builds were compiling at the same time (load average about 25).** These are worst-case conditions, not an idle machine.

What the columns measure:
- **Frame interval:** time between presented frames.
- **Frame work:** our CPU time from root render to last paint.

| Step | Frames | Interval p50 / p95 / p99 / max | Work p50 / p99 / max | Regions re-rendered |
|---|---:|---|---|---|
| Hover sweep over the map | 360 | 3.58 / 5.05 / 5.68 / 6.08 ms | 0.27 / 2.70 / 3.12 ms | map 31, list 31, footer 63 (fixture progress) |
| Scroll the list | 240 | 3.59 / 5.90 / 6.44 / 8.54 ms | 0.69 / 1.04 / 1.21 ms | list only |
| Navigate in/out ×8 | 240 | 3.59 / 5.53 / 6.39 / 6.75 ms | 0.13 / 1.98 / 2.44 ms | header, sidebar, map, list, inspector, tray |
| Drag to tray ×4 | 160 | 3.58 / 5.33 / 5.73 / 6.15 ms | 0.13 / 1.01 / 1.06 ms | map, list, tray |
| Idle | 30 | 3.59 / 5.46 / 5.97 ms | 0.15 / 0.20 ms | none |
| **Overall** | 1,029 | **p99 6.19 ms** (target < 8.3 ms: PASS) | p99 2.26 ms | |

## Headless worst cases (CPU only, debug build)
- 1,000,000-row list:
  - scroll: list region p99 0.39 ms
  - hover: p99 0.38 ms
  - far-row reveal in name order: p99 0.35 ms, down from 7–8 ms after the inverse-rank index (1b9bf92)
- First row hover in a folder with 1M grouped files: worst frame 0.87 ms, down from 45 ms after the background member-index warm-up (1b9bf92).
- Map: base-layer rebuild at 768 segments p99 0.56 ms (release); hover sweep with zero base rebuilds.
