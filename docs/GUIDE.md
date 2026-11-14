# Orbit Disk

A native disk-space explorer for macOS 14 and later, on Apple silicon and Intel Macs, written in Rust with the GPUI framework. Inspired by DaisyDisk's visual exploration and the scan-once, search-an-index architecture of instant file-search tools. This is an independent implementation; it bundles Sparkle for app updates.

Install with `curl -fsSL https://raw.githubusercontent.com/JohnDeved/orbit-disk/main/install.sh | sh`. The [script](../install.sh) downloads the latest release's ZIP, checks it against the release's `SHA256SUMS` and the app's code signature, and installs it in `/Applications` (or `~/Applications` when `/Applications` isn't writable). A download made with curl isn't quarantined, so macOS doesn't ask to approve the first launch. `ORBIT_DISK_VERSION=0.11.4` installs a specific release, `ORBIT_DISK_DIR=PATH` another folder, and `ORBIT_DISK_NO_OPEN=1` skips opening the app. It stops if Orbit Disk is running.

Or download the [universal disk image](https://github.com/JohnDeved/orbit-disk/releases/latest/download/Orbit-Disk.dmg), open it, and drag **Orbit Disk.app** to Applications before opening it. The current distribution is ad-hoc signed, without an Apple Developer ID signature or notarization. macOS may require initial approval in **System Settings → Privacy & Security**. Version 0.8.3 has no updater: install 0.9.0 or later manually once to enable future in-app updates.

Choose a folder or a device, then click a map folder to open it. Click the map center to go up. In the file list, click a row to inspect it; click its arrow or double-click the row to open a folder. The file list also exposes items too small to draw individually.

## App updates

Orbit Disk checks the public GitHub release feed daily while it is open. No GitHub account or login is needed. Turn automatic checks off in **Orbit Disk → Settings…**, or use **Orbit Disk → Check for Updates…** at any time. You choose when to install and restart; automatic installation is disabled. Update feeds and downloaded apps must pass signature verification before installation.

Restarting clears the collections and interrupts active scans. Saved maps remain available, and unfinished mapping can resume from committed progress when you reopen its location. File removal blocks update checks and quitting; a requested update restart waits for removal to finish.

You can start exploring during the first scan. Folders appear as they are discovered, and measured sizes fill in as mapping progresses. **Waiting** means the folder's contents have not been read yet; **so far** marks an unfinished total. You can search the files mapped so far, open folders, preview files, reveal items in Finder, and collect or remove eligible items while mapping continues. Sizes for unfinished folders are incomplete, and removing a folder includes everything inside it, including items not yet shown.

**Stop Mapping** keeps a browsable partial map. **Restart Mapping** resumes from committed progress to finish and save it. Switch to another location and its current scan continues in the background; return to see its latest results and collection. Navigation and selection are retained as new results arrive and when the completed map takes over. The map holds updates during a click or drag so its targets stay in place.

Drag a map segment or a row from the file list into the collection tray to stage that file or folder. Rows in search results work too. Grouped map segments collect the eligible items they represent. Release over the tray to collect, or press Escape to cancel. Files stay in place until you review the collection and confirm a removal action. You can also select a file and use **Add to Collection** or ⌘K.

Collected segments keep their branch color and show diagonal stripes plus a checkmark. The file list labels them **In Collection**, including contents of a collected folder. A minus-circle and **Partly collected** identify folders or groups that are only partly collected. These marks update as you add, remove, or clear entries.

The collection offers **Move to Trash…** and **Delete Permanently…**. Permanent deletion has a separate confirmation listing every item's full path; it removes those items and folder contents immediately, bypasses Trash, and cannot be undone. Both actions recheck file identities and protected locations. Failed items remain in the collection for review.

Click the app background outside a dialog to dismiss it. For a removal confirmation, this cancels the confirmation and returns to your collection. Escape and the Done/Cancel buttons work too. You can close collection review and switch locations while a removal continues.

Hovering a segment highlights and reveals its related row in Contents. For deeper rings, the parent row is highlighted and the exact nested path appears above the list. Hovering a row highlights its segment in return. Selected rows use the same soft coral fill and outline, with a slightly stronger border. Hover does not change your selection or the inspector's action buttons.

The map, list, inspector and action controls keep their positions while navigating, selecting, searching and collecting. Mapping status uses the same space before and after completion. Long names truncate within their slots, and rows keep the same height when collection markers appear.

## What works

- Background metadata scans with native `getattrlistbulk` batching and a POSIX fallback.
- Four-level interactive sunburst with honest aggregation of small items.
- Native map and file-list dragging into a large collection tray, with grouped map items, duplicate prevention, and background validation for large drops.
- Persistent collection marks on the map and file list, including inherited folder coverage and partially collected groups.
- Linked map/list hover, visible nested paths, and labeled actions with larger click targets.
- Sorted file and folder lists, breadcrumbs, and name search across the partial or saved map.
- Logical file sizes and separately reported allocated bytes. Hard-link allocation is counted once.
- Saved maps for each scanned location, restored on launch without another filesystem walk.
- Cached chart layouts and selection lookups, so pointer movement reuses the prepared map.
- Filesystem change tracking, including changes made while closed, with background updates of affected folders.
- Progressive browsing and search, collection and removal of eligible visible items, background mapping for other locations, resumable scans, inaccessible-item reporting, and an explicit full refresh when needed.
- Finder reveal, opening items in their default app, and collection review with confirmed Trash or permanent deletion.
- System folders, Library, hidden configuration directories, the scan root, home, symlinks, and volume roots protected from collection.
- Signed GitHub update checks and native installation, with manual restart and a daily-check preference.

## Shortcuts

| Action | Shortcut |
|---|---|
| Open folder map | ⌘O |
| Search mapped files | ⌘F |
| Refresh full map | ⌘R |
| Go up | ⌘[ |
| Open selected folder | Return or ⌘↓ |
| Reveal selection in Finder | ⇧⌘R |
| Open selected file in its default app | Space |
| Add selection to collection | ⌘K |
| Close a sheet or cancel a scan | Escape |

## Build and verify

Install [Rust](https://rustup.rs) (the toolchain pinned in `rust-toolchain.toml` installs automatically), [cargo-nextest](https://nexte.st) and Python 3, then run from the repository root:

```sh
./build-app.sh
cargo nextest run --workspace
./scripts/checks/check-platform-boundary.sh
python3 -I Tests/test_mcp.py '../Orbit Disk.app/Contents/MacOS/OrbitControl'
python3 scripts/test-release.py
cargo run -p orbit-disk -- --fixture
cargo xtask bench-compare --output /tmp/orbit-bench-results --rounds 3
```

The build script creates `../Orbit Disk.app` with `cargo xtask bundle`, includes the Night app icon and the pinned, checksum-verified Sparkle framework, and signs it ad hoc for local use. It builds for the current Mac's architecture; use `./build-app.sh --universal` for Apple silicon and Intel. With full Xcode and its Metal toolchain, `--release-mode` precompiles the Metal shaders; otherwise the app compiles them at first launch. `--fixture` opens the app on generated sample data. The editable icon and regeneration instructions are in [Assets](../Assets/README.md); ordinary builds use the included ICNS and need no image tooling.

The crates are laid out in [docs/rust-port/README.md](rust-port/README.md). Everything OS-specific lives in `crates/orbit-platform-macos` and `crates/orbit-shell-macos`, and the boundary check enforces that. Benchmarks against the last Swift release (v0.9.15) are in [docs/benchmarks/rust-port](benchmarks/rust-port/).

GitHub Actions builds and checks pushes to `main` and pull requests. Version tags create universal GitHub releases. See [RELEASING.md](../RELEASING.md) for the version, signing, and publishing steps.

Rust tests live next to the code they cover (`crates/*/src/**/tests*.rs` and `crates/*/tests/`). `Tests/` holds the Python protocol and updater-copy tests. Build, packaging and release tools are in `xtask/`, `build-app.sh` and `scripts/`.

The test suite covers the behavior that matters for a tool that deletes files:
- crash safety of the index and removal journal (killed processes, interrupted publication)
- deterministic race fixes
- hard-link accounting
- symlink, mount and firmlink safety
- removal policy
- automation protocol framing and schemas
- Unicode handling
- map geometry and hit-testing
- every stale-result guard in the app state

Real-filesystem tests run against the macOS platform layer. The engine and app-state crates also run on Linux in CI against a portable fallback layer. Which Swift-era checks were carried over, and why the others were dropped, is recorded in [docs/rust-port/test-triage.md](rust-port/test-triage.md).

Headless frame-time checks cover hover, scrolling, navigation and drag:

```sh
cargo nextest run -p orbit-ui --run-ignored only --no-capture headless
```

With the screen unlocked, the live harness measures presented frames:

```sh
OrbitDisk --frame-harness
```

## Scope and limits

The first visit to Home, a device, or another folder still needs a complete metadata scan, with partial results available during the scan. Separate locations can continue mapping while you use another location, as long as the app remains open. The app saves the completed map in a local SQLite cache under `~/Library/Caches/local.orbitdisk.app/Maps` and remembers the last location. Partial previews never replace a saved map. Mapping progress is committed in private batches under `~/Library/Application Support/local.orbitdisk.app/Engine` (the removal journal stays in `…/Mapping`), so a stopped or interrupted scan can resume when you reopen the location, including after the app restarts. When a saved map exists, it remains available while interrupted mapping resumes in the background. Later visits load only the displayed folder and chart detail; search covers discovered items during mapping and the saved index afterward. Opening a subfolder already covered by the current map reuses that index too. macOS filesystem events trigger updates of changed folders, including changes recorded while the app was closed. The map remains usable while updating.

Saved maps appear after checking the root's filesystem identity; event-history validation continues in the background. Chart geometry, group lookup, and collection marks are reused across hover and selection updates. New indexed data rebuilds the chart even when paths and sizes have stayed the same.

A cleared cache without a retained mapping stage, incompatible event history, or filesystem identity change requires a new baseline scan. A failed or incomplete incremental update retains the previous map and pauses updates; use **Refresh Full Map** (⌘R) to retry. Volumes without reusable change history keep the saved map available and require manual refresh. A manually cancelled refresh also pauses automatic updates. Changes are coalesced briefly, so the map can lag active writes. The cache and private mapping stages store paths, names, and size metadata, never file contents, and have owner-only filesystem permissions. macOS may remove cache files; mapping stages are kept separately in Application Support.

Scanning and indexing happen locally. The app contacts GitHub and its download infrastructure for update checks and downloads; it does not upload scanned paths, sizes, or file contents. There is no cloud-storage connector, snapshot manager, or automatic cleanup. Sparkle includes helpers for installing app updates; the scanner does not use a privileged helper.

The map uses logical data-fork sizes. Allocated size is a separate filesystem measurement, not guaranteed reclaimable space. APFS clones, snapshots, directory metadata, inaccessible locations, and concurrent filesystem changes affect how the numbers compare with Finder or device capacity. Nested mounted volumes are skipped to avoid duplicate traversal; scan them separately. Symlinks are displayed but never followed.

macOS may deny access to protected folders. The app shows skipped items and can open Privacy Settings; granting Full Disk Access is optional and remains your choice. Scanning and file removal do not request administrator privileges, and the app never empties Trash. Installing an app update may require authorization if its installation location is not writable.

The generated UI sample folder used for verification contains only test data and is not included in the application.

See [TECHNICAL-NOTES.md](../TECHNICAL-NOTES.md) for primary-source references and measured scan results, and [VERIFICATION.md](../VERIFICATION.md) for build and interaction checks.
