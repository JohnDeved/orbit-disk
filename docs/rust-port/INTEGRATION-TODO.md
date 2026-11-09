# Integration follow-ups

Seam and wiring changes that agents requested. The integrator applies them once the parallel branches are merged.

## Seams
- [ ] Move `orbit_core::journal::sys` helpers into `orbit_platform::posix`:
  - `open_at`, `mkdir_at`, `unlink_at`, `read_names`, `fchmod`, `fsync(fd)`
  - masked `stat`/`lstat`
- [ ] Platform data for journal canonical aliases:
  - add the macOS `CanonicalAliases` (`/System/Volumes/Data`, `/private`) to `BootLayout` or a `Platform` accessor
  - replace `CanonicalAliases::macos_boot()` in core
- [ ] Move `policy::standardize` into `paths`, which `refresh` also uses.
- [ ] Hoist `icu_collator` / `icu_locale_core` 2.4 into `[workspace.dependencies]`.
- [ ] Optional: add `CoreError` variants for `JournalChanged` and `ChangedDuringReconciliation`. Today both map to `SnapshotChanged`.
- [ ] Dedupe hard-link ordering if the durable-index branch kept a private `stage::link_order`; use `crate::hardlinks`.

## Wiring
- [ ] The removal journal parent must be the Swift app's directory, so Swift journals are found after an update: `~/Library/Application Support/local.orbitdisk.app/Mapping`.
- [ ] Wrap `RemovalRun::execute` in `Platform::without_materialization`.
- [ ] The store implements:
  - `recovery::RecoveryTarget` and `ReplayTarget`
  - `refresh::RefreshFacts`
  - `search::PreparedKey` (database path + file stamps)
  - `discovery::ListingCache`
- [ ] The mapping session replaces `ResumableStage`: discovery + `Mailbox` + `Stage::step` loop + publish + live inventory.
- [ ] A real `orbit_app_state::Backend` over orbit-core.
- [ ] `xtask bundle` stamps the workspace version into Info.plist; the release build disables `runtime-shaders`.
- [ ] Re-enable GPUI `wayland`/`x11` features for a future Linux shell (orbit-ui currently uses `default-features = false`).

## Behavior differences to confirm with the user
- Removal policy is stricter under `/private`: real canonical paths are compared, so `/private/var/...` outside temp roots is rejected. Swift allowed it via Foundation's `/private` stripping.
- Search accelerator budget is 96 MiB, about 860k varied names. Larger maps search the tail with SQL, same as Swift.
