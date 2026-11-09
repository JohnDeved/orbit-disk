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
- [ ] A real `orbit_app_state::Backend` over orbit-core.
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
