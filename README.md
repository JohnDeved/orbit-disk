# Orbit Disk

A native Swift / SwiftUI disk-space explorer for macOS 14 and later. Inspired by DaisyDisk's visual exploration and the scan-once, search-an-index architecture of instant file-search tools. This is an independent implementation with no third-party runtime dependencies.

Build with `./build-app.sh`, then open **Orbit Disk.app** beside the repository folder. Choose a folder or a device, then click a map folder to open it. Click the map center to go up. In the file list, click a row to inspect it; click its arrow or double-click the row to open a folder. The file list also exposes items too small to draw individually.

You can start exploring during the first scan. Folders appear as they are discovered, and measured sizes fill in as mapping progresses. **Waiting** means the folder's contents have not been read yet; **so far** marks an unfinished total. You can open folders, preview files, reveal items in Finder, and collect finished items. Search and removal become available when the completed map is saved.

**Stop Mapping** keeps a browsable partial map for the current session. **Restart Mapping** starts a fresh scan to finish and save it. Navigation and selection are retained as new results arrive and when the completed map takes over. The map holds updates during a click or drag so its targets stay in place.

Drag a map segment or a row from the file list into the collection tray to stage that file or folder. Rows in search results work too. Grouped map segments collect the eligible items they represent. Release over the tray to collect, or press Escape to cancel. Files stay in place until you review the collection and confirm a removal action. You can also select a file and use **Add to Collection** or ⌘K.

Collected segments keep their branch color and show diagonal stripes plus a checkmark. The file list labels them **In Collection**, including contents of a collected folder. A minus-circle and **Partly collected** identify folders or groups that are only partly collected. These marks update as you add, remove, or clear entries.

The collection offers **Move to Trash…** and **Delete Permanently…**. Permanent deletion has a separate confirmation listing every item's full path; it removes those items and folder contents immediately, bypasses Trash, and cannot be undone. Both actions recheck file identities and protected locations. Failed items remain in the collection for review.

Click the app background outside a dialog to dismiss it. For a removal confirmation, this cancels the confirmation and returns to your collection. Escape and the Done/Cancel buttons work too. Collection review stays open while a removal is running.

Hovering a segment highlights and reveals its related row in Contents. For deeper rings, the parent row is highlighted and the exact nested path appears above the list. Hovering a row highlights its segment in return. Selected rows use the same soft coral fill and outline, with a slightly stronger border. Hover does not change your selection or the inspector's action buttons.

The map, list, inspector and action controls keep their positions while navigating, selecting, searching and collecting. Mapping status uses the same space before and after completion. Long names truncate within their slots, and rows keep the same height when collection markers appear.

## What works

- Background metadata scans with native `getattrlistbulk` batching and a POSIX fallback.
- Four-level interactive sunburst with honest aggregation of small items.
- Native map and file-list dragging into a large collection tray, with grouped map items, duplicate prevention, and background validation for large drops.
- Persistent collection marks on the map and file list, including inherited folder coverage and partially collected groups.
- Linked map/list hover, visible nested paths, and labeled actions with larger click targets.
- Sorted file and folder lists, breadcrumbs, and SQLite name search across the saved index.
- Logical file sizes and separately reported allocated bytes. Hard-link allocation is counted once.
- Saved maps for each scanned location, restored on launch without another filesystem walk.
- Cached chart layouts and selection lookups, so pointer movement reuses the prepared map.
- Filesystem change tracking, including changes made while closed, with background updates of affected folders.
- Progressive first-scan browsing, completed-item collection, cancellation, inaccessible-item reporting, and an explicit full refresh when needed.
- Finder reveal, native Quick Look, and collection review with confirmed Trash or permanent deletion.
- System folders, Library, hidden configuration directories, the scan root, home, symlinks, and volume roots protected from collection.

## Shortcuts

| Action | Shortcut |
|---|---|
| Open folder map | ⌘O |
| Search saved index | ⌘F |
| Refresh full map | ⌘R |
| Go up | ⌘[ |
| Open selected folder | Return or ⌘↓ |
| Reveal selection in Finder | ⇧⌘R |
| Quick Look selected file | Space |
| Add selection to collection | ⌘K |
| Close a sheet or cancel a scan | Escape |

## Build and verify

Install Apple's Swift command-line tools or Xcode, then run from this folder:

```sh
./build-app.sh
swift run OrbitCoreChecks
./check-app.sh
./check-layout.sh
swift run -c release OrbitScan /path/to/folder
swift run -c release OrbitScan --posix /path/to/folder
```

The build script creates `../Orbit Disk.app`, includes the Night app icon, and signs it ad hoc for local use. It builds for the current Mac's architecture. The package opens in Xcode through `Package.swift`. The editable icon and regeneration instructions are in [Assets](Assets/README.md); ordinary builds use the included ICNS and need no image tooling. The optional layout checks require Python 3 and a macOS desktop session; their generated files stay in `.build`.

`OrbitCoreChecks` is a dependency-free test executable so the tests run with Command Line Tools alone, even when XCTest is unavailable. It creates temporary fixtures and checks metadata, bulk/fallback parity, resource forks, sparse files, hard links, Unicode, cancellation, unreadable roots, cleanup policy, persistent indexes, incremental updates, stale revisions, and cancellation during index writes. Progressive checks cover discovery states, preview bounds and exact completed totals. `check-app.sh` checks the app model with real filesystem events, including live changes, cached relaunch, offline replay, and navigation/collection retention. It also checks partial browsing, stop/restart, save failures, gesture-held updates, linked hover, private drag tokens, grouped collection, duplicate prevention, stale identities, large-drop validation/cancellation, and native chart hit testing and gesture thresholds.

## Scope and limits

The first visit to Home, a device, or another folder still needs a complete metadata scan, with partial results available during the scan. The app saves the completed map in a local SQLite cache under `~/Library/Caches/local.orbitdisk.app/Indexes` and remembers the last location. Partial previews are never written over a saved map. Later visits load only the displayed folder and chart detail; search reads the saved index. Opening a subfolder already covered by the current map reuses that index too. macOS filesystem events trigger updates of changed folders, including changes recorded while the app was closed. The map remains usable while updating.

Saved maps appear after checking the root's filesystem identity; event-history validation continues in the background. Chart geometry, group lookup, and collection marks are reused across hover and selection updates. New indexed data rebuilds the chart even when paths and sizes have stayed the same.

A cleared cache, incompatible event history, or filesystem identity change requires a new baseline scan. A failed or incomplete incremental update retains the previous map and pauses updates; use **Refresh Full Map** (⌘R) to retry. Volumes without reusable change history keep the saved map available and require manual refresh. A manually cancelled refresh also pauses automatic updates. Changes are coalesced briefly, so the map can lag active writes. The cache stores paths, names, and size metadata, never file contents, and has owner-only filesystem permissions. Cache files may be removed by macOS.

There is no privileged helper, cloud-storage connector, snapshot manager, or automatic cleanup. No network calls are used by the app.

The map uses logical data-fork sizes. Allocated size is a separate filesystem measurement, not guaranteed reclaimable space. APFS clones, snapshots, directory metadata, inaccessible locations, and concurrent filesystem changes affect how the numbers compare with Finder or device capacity. Nested mounted volumes are skipped to avoid duplicate traversal; scan them separately. Symlinks are displayed but never followed.

macOS may deny access to protected folders. The app shows skipped items and can open Privacy Settings; granting Full Disk Access is optional and remains your choice. It never requests administrator privileges or empties Trash.

The app is locally signed, not notarized for public distribution. The generated UI sample folder used for verification contains only test data and is not included in the application.

See [TECHNICAL-NOTES.md](TECHNICAL-NOTES.md) for primary-source references and measured scan results, and [VERIFICATION.md](VERIFICATION.md) for build and interaction checks.
