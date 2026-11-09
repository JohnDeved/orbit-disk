# Integration follow-ups

Seam and wiring changes that agents requested. The integrator applies them once the parallel branches are merged.

## Seams
- [x] Move `orbit_core::journal::sys` helpers into `orbit_platform::posix`:
  - `open_at`, `mkdir_at`, `unlink_at`, `read_names`, `fchmod`, `fsync(fd)`
  - masked `stat`/`lstat`
- [x] Platform data for journal canonical aliases (`Platform::path_aliases` → `PathAliases`; `CanonicalAliases` is an alias of it):
  - add the macOS `CanonicalAliases` (`/System/Volumes/Data`, `/private`) to `BootLayout` or a `Platform` accessor
  - replace `CanonicalAliases::macos_boot()` in core
- [x] Move `policy::standardize` into `paths`, which `refresh` also uses.
- [x] Hoist `icu_collator` / `icu_locale_core` 2.4 into `[workspace.dependencies]`.
- [x] Optional: add `CoreError` variants for `JournalChanged` and `ChangedDuringReconciliation`.
- [x] Dedupe hard-link ordering (the writer already uses `crate::hardlinks`).

## Wiring
- [x] The removal journal parent must be the Swift app's directory, so Swift journals are found after an update: `~/Library/Application Support/local.orbitdisk.app/Mapping`.
- [x] Wrap `RemovalRun::execute` in `Platform::without_materialization` (`removal::remove`).
- [x] The store implements (`saved`, `session`, `stage::cache`):
  - `recovery::RecoveryTarget` and `ReplayTarget`
  - `refresh::RefreshFacts`
  - `search::PreparedKey` (database path + file stamps)
  - `discovery::ListingCache`
- [x] The mapping session replaces `ResumableStage`: discovery + `Mailbox` + `Stage::step` loop + publish + live inventory.
- [x] A real `orbit_app_state::Backend` over orbit-core (`orbit-backend::EngineBackend`;
  the app builds it with `EngineBackend::new(Arc::new(Native::default()))`).
- [ ] `xtask bundle` stamps the workspace version into Info.plist; the release build disables `runtime-shaders`.
- [ ] Re-enable GPUI `wayland`/`x11` features for a future Linux shell (orbit-ui currently uses `default-features = false`).

## Behavior differences to confirm with the user
- Removal policy is stricter under `/private`: real canonical paths are compared, so `/private/var/...` outside temp roots is rejected. Swift allowed it via Foundation's `/private` stripping.
- Search accelerator budget is 96 MiB, about 860k varied names. Larger maps search the tail with SQL, same as Swift.

## From the durable-index merge
- [x] Dedupe `lease::sys` and `journal::sys` into `orbit_platform::posix`:
  - `openat`, `mkdirat`, `unlinkat`, `renameat`, `O_EXCL` create
  - masked `fstat`/`fstatat`/`lstat` returning mode, uid, nlink, size, mtime, ctime
  - `geteuid`; fd read/write/fsync
- [ ] Optional: a set of inaccessible/unavailable directories on `MailboxSnapshot`.
- [x] Ported in the session wave:
  - saved-map and staged search over SQL (`query`, `SavedMaps::search`) using `search::NameIndex`
  - `StageReuse` (`reuse`)
  - feeding the local pass into the live inventory (`stage::LocalObserver`, `LiveInventory::relist`)
  - `StageRemovalReplay` wiring (`ReplayTarget` over the session's stage)
  - `DiscoveryCacheReader` (`stage::cache::StageListingCache`)
  - `MappingCoverage` (`SavedMaps::coverage`)
  - path canonicalization (`IndexPaths::for_platform`, `store::canonical_location`)

## From the session wave
- [ ] Swift format-5 stages in `~/Library/Application Support/local.orbitdisk.app/Mapping/.orbit-stages`
  are never resumed and are not deleted (only the `Caches/…/Indexes` folder is discarded). Decide
  whether to delete them on first launch; the journal next to them must stay.
- [ ] StageReuse checks a reused map's removal-journal cursor once, when candidates are chosen;
  Swift also re-checked it under the fence before each copied listing.
- [ ] Staged search (stage database, used only after discovery was abandoned for a full
  reconciliation) runs SQL without a prepared prefix; Swift built one when due.
- [ ] Refresh re-lists every planned subtree; a pure-SQL subtree replacement may be faster for
  huge unchanged subtrees (not measured).
- [ ] Workspace clippy/nextest for this wave excluded `orbit-ui` (GPUI does not fit the free disk
  space in this worktree); no API it uses changed.

## From the engine backend (`orbit-backend`)
- [ ] Wire `EngineBackend` into the app: the host runs `orbit_app_state::perform` on the background
  executor and forwards `Notifier` events to the UI thread.
- [ ] `ProgressSnapshot::current_path` is the mapped root: `MappingSession` does not expose the
  folder being listed. Swift showed the current path in the footer.
- [ ] `SearchSource::Saved { stable_order }` is ignored. The saved-map order (size, then path) is
  already deterministic; confirm this matches the Swift tie order across the preview hand-off.
- [ ] Journal recovery (`SavedMaps::recover`) runs every time a non-forced mapping starts for a
  root that has a saved map, not just the first time. It is cheap when the map is up to date.
  A recovery that cannot repair the map (`FullRefresh`) re-maps the root instead.
- [ ] `MappingSession::start` retries a busy stage for up to 2 s and cannot be cancelled during
  that time, so a stop issued then waits for the retry.
- [ ] Not yet asserted in orbit-core (see `orbit-app-state/TRIAGE-COVERAGE.md`, backend rows):
  - cumulative overflow across refreshes
  - stage-level recreated identity after a kill
  - a cancel inside `publish_recovery_copy`

## Wave E: polish and parity, collected from the UI panel agents
- [ ] Live frame harness with the screen unlocked (`OrbitDisk --frame-harness`); target p99 < 8.3 ms. All UI numbers so far are headless.
- [ ] Visual parity pass against the Swift app and DESIGN.md: screenshots side by side, light and dark art.
- [ ] Finder folder drops on the explorer area should add Places again. Reuse `sidebar::add_dropped_folders` / `track_folder_drag`.
- [ ] First-scan full-screen view: item count, path being read, file-size figure. App-state needs to expose progress bytes, and `MappingSession` the current path.
- [ ] Escape stops mapping (Swift parity) without stealing Escape from sheets and drags.
- [ ] Tray "Removing from collection…" state: app-state accessor.
- [ ] Skipped-items sheet lists the affected paths: the engine and app-state need to carry issue paths.
- [ ] Search field: undo/redo, word-wise movement, grapheme clusters (`unicode-segmentation`).
- [ ] Reduce Motion re-read at runtime; menu items disabled while a sheet is open.
- [ ] Map center: middle truncation and round line caps for the badge glyphs (GPUI limits; custom text shaping if needed).
- [ ] Release pipeline:
  - `xtask bundle` stamps the version and builds without `runtime-shaders`
  - swap `scripts/release.sh` and the workflows from SwiftPM to cargo
  - rewrite `capture-marketing.sh` for GPUI
- [ ] Idle-machine benchmark rerun of every Rust vs Swift comparison for the cutover report (`docs/benchmarks/rust-port.md`).
