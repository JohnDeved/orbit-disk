# Rust + GPUI port: conventions

The Swift app (removed at the 0.10.0 cutover; see tag `v0.9.15`) was the **reference implementation** during the port. Port its behavior, not its structure. Read `TECHNICAL-NOTES.md` sections for the code you port; they record invariants that were expensive to find.

## Crate rules
- `orbit-platform` defines the OS boundary (`Platform`, `Shell`, shared types). Change it only with the integrator's agreement; layer crates implement it.
- Only `orbit-platform-<os>` / `orbit-shell-<os>` may call OS-specific APIs. `scripts/checks/check-platform-boundary.sh` enforces this.
- `orbit-core`, `orbit-sunburst`, `orbit-app-state` and `orbit-automation` have no UI dependency and build on Linux. Everything in them is testable without a window.
- Paths and names are raw bytes (`&[u8]` / `Vec<u8>`, `OsStr` at the edges). Never require UTF-8. Device numbers are `u64`.
- Hot paths use generics/associated types, not `dyn`. No allocation per directory entry in the scanner.
- `unsafe` blocks carry a `// SAFETY:` comment.

## Tests: keep only high-signal tests
This follows the rubric from pingdotgg/t3code#22557. `docs/rust-port/test-triage.md` lists which Swift checks to port.

**Write tests for:**
- crash safety and durability (journal, publication, fences, recovery)
- deterministic race fixes, driven by explicit hooks or barriers rather than sleeps
- persistence schema and stage protocol
- hard-link accounting
- symlink, mount and identity safety
- removal-policy guards
- wire-protocol parsing, framing and schema validation
- Unicode fold consistency
- layout invariants
- stale-token rejection

**Do not write:**
- tests that the type system already guarantees
- trivial getters or tests that mirror the implementation
- real-timer or sleep-based tests (inject a clock instead)
- mock-call mirrors
- smoke tests
- duplicate table cases
- wall-clock performance assertions (those belong in `orbit-bench`)

Budget: the whole `cargo nextest run --workspace` stays under 60 s on an M-series Mac.

## Performance
- Every hot path has a criterion bench or a probe in `orbit-bench`.
- Benchmarks run the `orbit-bench` probes in fresh processes (`cargo xtask bench-compare`). They are reports, not CI timing gates. The Swift v0.9.15 probes used for the port's comparisons remain at that tag.
- Baseline numbers are in `docs/benchmarks/*.md|json`. Swift probe binaries are built under `~/orbit-bench/swift-v0.9.15-*`.

## Before handing back
Run each of these and make sure it passes:
- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo nextest run -p <your crates>`
- `scripts/checks/check-platform-boundary.sh`

Commit messages use conventional prefixes (`feat(core): …`, `perf(scan): …`).
