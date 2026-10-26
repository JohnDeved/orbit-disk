# Verification

Verified on 26 October 2026 on an Apple Silicon Mac running macOS 27.0.1, with Swift 6.3.3. The application targets macOS 14 or later.

## Build and scanner

- PASS: production Swift package build and `.app` bundle launch.
- PASS: `codesign --verify --deep --strict` on the delivered app. This is ad-hoc signing for local use, not notarization.
- PASS: 19 dependency-free checks in `OrbitCoreChecks`, with zero failures: hierarchy, empty and hidden items, symlink loops/broken links, hard links, sparse files, bulk/POSIX parity across multiple batches, resource forks, cancellation, invalid roots, permissions, progress throttling, cleanup path protection, persistent index/search roundtrips, incremental totals/hard links, folder identities, stale revisions, and atomic cancellation.
- PASS: chart geometry assertions for proportional angles, incomplete descendant totals, empty roots, 10,000-file grouping, the 768-segment cap, and hit testing.
- PASS: a generated 20,006-file fixture matched independent `lstat` totals exactly. Release bulk runs took 0.218s and 0.197s, with 19.5 MB peak resident memory. A forced POSIX fallback run also matched. These are observed cached-fixture measurements, not a whole-disk guarantee.
- PASS: native `FileManager.trashItem` moved a uniquely named generated fixture to Trash; the returned Trash URL contained the expected bytes; moving it back restored those bytes. No personal files were moved or deleted.

## Recorded native interactions

| Control/state | Observed result |
|---|---|
| Welcome / Choose Folder | Opens the native directory picker; selected sample folder produces a real map and list. |
| Open Folder / ⌘O | Opens the same native picker. |
| Sunburst selection | Highlights the chosen sector and updates size/name details. |
| Sunburst single-click | Opens the exact folder represented by that sector immediately, without waiting for a double-click. |
| Sunburst center / Up | The center circle and labeled Up button return to the parent; the folder just left remains selected. |
| File list | Pointer and arrow keys select rows and populate the inspector. |
| Folder row arrow / double-click | A single click on the arrow opens the folder; native row double-click also opens it. |
| List context menu | Right-click on a file exposes Reveal in Finder, Quick Look, and Add to Collection. |
| Return / ⌘↓ | Opens the selected folder with the list focused. |
| Open / breadcrumbs / Back | Changes the displayed folder and parent path correctly. |
| ⌘F / search | Queries the saved index; no-result query shows “No matching files.” |
| Search selection | Focus moves to the list; selection remains stable after leaving the text field. |
| Clear search | Restores current-folder contents. |
| Sort menu | “Name” sorts alphabetically; “Largest first” restores size order; color mapping remains stable. |
| Finder reveal | Finder opens the sample folder and selects the exact file. |
| Quick Look | Native preview displays generated text-file contents; Escape closes it. |
| Collect | Adds the selected item; duplicate collection is disabled; no filesystem change occurs. |
| Review Collection | Displays names, full paths, and file sizes. |
| Move to Trash… | Opens an explicit confirmation; Cancel returns to review without moving anything. The native Trash API was separately verified with a generated fixture. |
| Clear Collection | Empties the review list while files remain in place. |
| Empty folder | Shows a zero-data chart and “This folder is empty.” |
| Inaccessible folder | Shows “Unavailable” and “Not scanned”; opening it says “Contents unavailable.” |
| Skipped items | Opens a sheet identifying the permission-denied fixture path. |
| Privacy Settings | Opens System Settings at Full Disk Access; no permission was changed. |
| Refresh Full Map / ⌘R | Refreshes data; restoring fixture permissions removes the skipped-item report. |

The 1280 × 810 window was visually inspected. The minimum 1060 × 680 size is enforced in code; arbitrary window sizes, 200% accessibility text scaling, VoiceOver, older macOS versions, and other hardware have not been exhaustively tested. Device and place shortcuts share the tested scan method; whole-disk scans were not run during UI verification. Cancellation and fatal scan errors are covered by core checks; their UI handlers were also reviewed.

The unified runtime log was inspected. macOS emitted AppIntents registration/service-connection errors (4097), BaseBoard task-port errors, and cache warnings. No application crash occurred during the recorded workflows. The app does not implement App Intents; these host-service messages were not changed or suppressed.

## Design delivery gate

- PASS, content and states: all displayed data comes from filesystem metadata; the preview uses explicitly named generated sample files. Empty, scanning, cancellation, error, partial and no-result states exist. No invented testimonials, claims, branding assets or inactive navigation were added.
- PASS, interactions: the table above records the tested workflows. Remaining equivalent scan triggers and error/cancellation controls were inspected in source. Final Trash dispatch was tested through its native API rather than by moving personal files.
- PASS, contrast: muted text on the inspector computes to 8.36:1; dark text on coral actions to 8.15:1. The chart palette is at least 3.63:1 on the canvas, and dimmed hover sectors remain at least 3.16:1. Chart secondary text is 7.96:1. Native focus and explicit search focus remain visible.
- PASS, purpose: DESIGN.md records the palette, typography, grouping, circular map, spacing, and motion decisions. The multi-hue palette identifies filesystem branches; it is not decorative UI coloring.
- PASS, visual direction: ENERGY 2 / RHYTHM 2 / MOTION 1. The disk map is the focal point, paired with quieter navigation and a dense file inspector.
- PASS, build and code hygiene: the shipped app builds and launches; no web wrapper, external dependencies, source-rewriting scripts, fake controls, or network calls are part of the implementation.

This gate is scoped to the native desktop build and the evidence above. It is not a certification of all assistive technologies or distribution readiness. Public distribution still requires the developer's signing identity and notarization.

Sidebar refinement: PASS. Removed the redundant “On your Mac” footer and description, rebuilt the release app, and verified their absence in the running sample scan. No functional code changed; the filesystem checks were not rerun for this removal.

Navigation refinement: PASS. Reduced header, wordmark, and breadcrumb padding, and verified the compact layout at 1280 × 810. Verified single-click map navigation, the full center Up target, row selection, row-arrow navigation, native double-click, context menus, and Return in the rebuilt app using generated sample files. The heading and path update with the current folder. Scanner code was unchanged; scanner checks were not rerun for this UI iteration.

Cursor refinement: production build and signature verification passed. Replaced one-shot `NSCursor.set()` calls with native SwiftUI pointer styles on macOS 15+ and AppKit cursor rectangles on macOS 14. Action controls honor their enabled state. The map derives its cursor and hover display from one retained pointer location and current geometry, including after folder navigation. Map clicks, row-arrow navigation, and center Up were checked in a separate test copy, preserving the user's running app and collection. Automation pointer warps did not reproduce physical mouse-hover updates, so a complete visual cursor-transition check remains unverified. The macOS 14 fallback compiled but was not exercised on macOS 14. Scanner code was unchanged.


## Persistent indexing update

- PASS: 19 core checks after the compact SQLite schema and directory-identity changes.
- PASS: real filesystem-event checks for live changes, offline history replay, cache-event exclusions, compatible root-volume history, identity mismatch handling, and three unsupported-volume starts without a rescan loop.
- PASS: refresh planning checks for overlapping paths, deleted folders, new parents, literal Unicode paths, symlink escapes and APFS mount boundaries.
- PASS: model integration checks cover one-time indexing, same-location reuse, indexed search, deep folder navigation, live updates, retained collection/navigation, cached relaunch, offline replay, covered-subfolder reuse, search cancellation and a full refresh that keeps the map visible while preserving navigation made during scanning. A cached model reopened in 34 ms in the generated small fixture.
- Scanner optimization benchmark: nine interleaved release runs of the same generated 20,006-file tree reduced the median scan time from 176 ms to 112 ms (36.4%). Totals remained exactly 88,077,752 logical bytes and 65,576,960 allocated bytes. These are warm fixture results.
- Saved-index benchmark: a separate process opened a synthetic 200,000-file index with a median folder load of 14.3 ms and search of 22.3 ms; peak reader RSS was 18.8 MB. This does not measure an initial whole-home filesystem scan. See TECHNICAL-NOTES.md for methodology and initial persistence costs.

The first scan remains a full metadata traversal. Normal reopen and covered-subfolder navigation reuse the saved index. Invalid event history, volume identity changes, a cleared cache, or explicit full refresh may require another baseline. Incomplete incremental scans retain the old map and pause updates rather than committing incomplete totals. No whole-home or whole-Mac latency guarantee is claimed.


Native saved-map verification: PASS. A separate bundle identifier preserved the user's running app and collection. The final build reopened the generated 19-file map with “Saved map” shown, and the SQLite revision remained unchanged. During a live session, adding a generated 450 KB file changed Downloads from two files/1.1 MB to three files/1.6 MB without moving navigation; indexed search found it.

Watcher responsiveness: PASS. Native testing exposed a macOS Documents access request blocking synchronous FSEvents creation. Stream setup, filesystem identity inspection and teardown now run off the main thread; initial events buffer until attachment. The map, folder navigation and folder picker stayed responsive during the pending access request, and switching to the temporary fixture started a functioning new watcher. No extra folder permission was granted. Twenty-five cancellation/restart checks passed without stale callbacks or missing history completion. A pending kernel permission wait itself cannot be forcibly interrupted; cancelled setup is prevented from attaching or delivering events afterward.


## Mouse collection and linked hover (0.3.0)

- PASS: production build, bundle property-list validation, and strict ad-hoc signature verification.
- PASS: app integration checks for private one-use drag payloads, cancellation, duplicate/ancestor deduplication, protected items, stale file/directory identities, a 72-item background collection batch, cancellation during validation, and preservation of newer status messages. Collecting leaves fixture files in place.
- PASS: map/list relationship checks for nested sectors and grouped sectors. Hover does not change selection. Unchanged hover remains linked across a live filesystem update; navigation clears it.
- PASS: six native interaction/layout assertion groups cover segment membership, grouped-member matching, center exclusion, click-versus-drag threshold, the retained starting sector, rejected drags, stale-root clicks and hover exit.
- PASS: native interaction inspection at 1280 × 810. A graph sector highlighted its related Contents row and path; outer-ring file selection populated the inspector. Labeled Add to Collection, Review Collection, Remove, row-folder navigation and center Up worked in a separate test bundle. The primary action's text now explicitly uses the dark foreground on coral.
- PASS: a physical-mouse drag was recorded through the instrumented test bundle without changing the product path: the tray became targeted, onDrop accepted the declared collection type, the 36-byte token loaded, collection increased to one item, and AppKit ended with the copy operation. No replay workaround was involved. The user also confirmed that dragging adds the item. Only collection staging was exercised; no files were moved by this check.
- PASS: the proprietary collection type is exported in Info.plist and resolves as a declared data type. No file URLs are placed on the drag pasteboard.
- PASS: computed contrast for new states: hover outline 5.93:1; secondary text on the hover fill 6.66:1; idle tray outline 4.90:1; active drop outline 6.23:1; secondary text on the drop fill 6.99:1; primary label 8.15:1.

CUA's synthetic drag can deliver mouse-up before AppKit's asynchronous session-start callback. This prevented reliable automated end-to-end drops; diagnostic event traces distinguished that from the successful ordinary mouse path. Escape cancellation was observed. A diagnostic-only replay experiment was not included in the delivered app. Full physical cursor transitions, VoiceOver and every window size remain outside the recorded coverage. The earlier preview image records the saved-index iteration, before this collection UI update.

Verification used a separate bundle identifier and did not quit or replace the user’s running session. Diagnostic files are outside the delivered source; the app contains no diagnostic event logger.


## Permanent deletion option (0.4.0)

- PASS, build: production app build and strict ad-hoc signature verification.
- PASS, removal checks: generated files and a folder were permanently removed through the app model. A nested symlink's outside target survived. Replaced files, directories and a replaced scan root were rejected; a mixed successful/failed collection retained its failed items and refreshed the index. These checks never targeted personal files.
- PASS, regression checks: app indexing/collection checks, six chart interaction assertion groups, and all 19 core checks completed with zero failures.
- PASS, native confirmation: the review sheet shows both Delete Permanently… and Move to Trash…. The permanent confirmation lists the generated file's name and full path, explains folder-content removal and bypassing Trash, and has Cancel/Delete Permanently controls. Escape returned to review with the file intact. The Trash confirmation still opened and Cancel preserved the collection. The final native deletion button was not clicked; its model action was exercised with generated fixtures through the test executable.
- PASS, failure visibility: failed entries remain collected; a bounded, selectable error panel in the review sheet avoids relying on an alert behind a sheet. Source review confirmed parent refresh is queued before each removal attempt, including operations that may fail after partial recursive deletion.
- PASS, design gate: the existing dark surfaces, system type, contrast-checked text and restrained accent remain in use. DESIGN.md records the action hierarchy and confirmation purpose. Labels describe both actions explicitly; Escape cancels, and the permanent action has no default Return shortcut. Empty/disabled, busy and failure states are implemented.

The native confirmation was visually inspected at 640 × 430; the collection review was inspected at 720 × 470. No personal file was deleted during verification. The user's existing app session was not quit.


## Persistent collection visibility (0.5.0)

- PASS, build: production app rebuild, bundle property-list validation, and strict ad-hoc signature verification.
- PASS, coverage: focused assertions cover exact paths, inherited folder membership, partial ancestors, root handling, prefix collisions, literal Unicode paths, mixed groups, and immediate add/remove/clear updates. Collection coverage is rebuilt only when collection entries change; drawing performs no filesystem queries.
- PASS, regression: app model checks completed with zero failures, including saved-index reuse, search, live changes, drag payloads, grouped collection, stale identities, and removal safeguards. Seven interaction/layout assertion groups passed, including grouped collection state and badge-fit checks. Scanner sources were unchanged, so the standalone core suite was not rerun for this UI update.
- PASS, native UI: collecting Media immediately added hatching/checks throughout that branch and In Collection labels in Contents. Opening Media showed inherited labels on its children. Removing Media from review removed the labels; Clear Collection removed all hatching and markers. Collecting only Archives/Installers marked Archives with a minus badge and Partly collected label while leaving Backups unmarked. Searching for Tools retained both its In Collection label and parent path. All actions staged generated sample data only.
- PASS, final visual check: the original partial-status wording clipped in a row; the final build uses Partly collected in the row, map center, and legend. The full/partial distinction remains visible with no selection or hover, and the shorter labels fit at 1280 × 810.
- PASS, design gate: existing branch colors, native text, focus and action hierarchy remain intact. Dark hatching provides a non-color cue; check/minus symbols and text distinguish full versus partial membership. Hatch/badge contrast is at least 3.49:1 against the dimmed sector palette, and the white badge glyph is 20.17:1 against its background. Tiny included sectors retain hatching when a badge cannot fit.

Verification used an isolated bundle identifier and a temporary generated fixture. The user's running app session was not quit. Native drag behavior was unchanged; this update verified staging via the existing collection action, with physical-mouse drag confirmation recorded under 0.3.0.

## File-list dragging and selected rows (0.6.0)

- PASS, production build: the native app builds, its bundle property list validates, and strict ad-hoc signature verification succeeds.
- PASS, model checks: the indexed-search regression drags one result while a different result is selected, stages the exact source through the private payload, rejects a duplicate drag, and verifies both files remain intact. Existing cache, live-update, grouped collection, cancellation, identity and removal checks pass.
- PASS, interaction checks: ten assertion groups pass in both debug and optimized builds. New checks cover native list focus, arrow exclusion, context clicks, double-click versus drag, retained source metadata, and recycled rows. The first run exposed an invalid empty-table test fixture; adding a real column and data-source row corrected the fixture without changing production behavior.
- PASS, native row controls: a separate test bundle with generated files showed the coral selection fill and outline with no blue selection block. Down Arrow moved that highlight and updated the inspector. Right-click showed the existing context menu; the folder arrow opened Archives; double-click opened Backups. The native selection remains exposed in the accessibility tree.
- PASS, physical-mouse drop: the user confirmed dragging January.bin from the file list into the tray adds it. A subsequent native inspection showed January.bin marked In Collection in both the selected row and map, and the tray showed one item totaling 2.9 MB. Only generated sample data was staged.
- PASS, visual check: the selected and collected sample row showed its coral outline, matching map marker, and collection tray count.
- PASS, purpose and visual gate: selected rows reuse the map-linked hover treatment, with a 2-point outline to indicate persistent selection. The change is scoped to this file list; collection review and native pickers retain their own selection appearance. Secondary text on the selected fill computes to 6.64:1, and its outline to 5.92:1. Existing branch colors, collection patterns, focus, spacing and system typography remain in use.

List dragging uses the original mouse-down event, a five-point movement threshold, a private one-use payload, and the native session-end callback. Folder arrows and context clicks pass through the row drag overlay; wheel events reach the enclosing scroll view. The native drag source offers only in-application collection copying. No files move when staged.

CUA synthetic gestures exhibited the same early-release timing limitation documented under 0.3.0; Escape ended the synthetic attempt without adding an item. The physical-mouse check above verifies the actual drop. Scanner code was unchanged and its standalone suite was not rerun. The selection adapter uses the public NSTableView highlight API on the native table enclosing the row; a future SwiftUI backing-view change would fall back to native selection paint. Older macOS versions and VoiceOver were not exercised in this update.

## Faster mapping and interaction (0.7.0)

- PASS: release build, bundle property-list validation, strict ad-hoc signature verification, and 19 core checks, including bulk/fallback metadata parity, cancellation, persistence, incremental updates, and stale-index protection.
- PASS: final optimized app/model suite against the updated core. New assertions cover immediate saved-root display, background journal validation, replaced-root rejection, publication revisions, same-query search replacement, row sorting, and same-folder refresh invalidation. Existing live/offline event, collection, drag-token, removal, and navigation-retention checks remain passing.
- PASS: 12 native chart/list interaction groups. New cache assertions cover reuse through selection and collection changes, root-path changes, equal-size/same-path member replacements, and stationary hover receiving a new revision's members. Existing click/drag thresholds, center targeting, row focus, and stale-gesture protections pass.
- PASS: native inspection in a separate test bundle. The generated 18-file map reopened with Saved map shown; its database snapshot ID and scan timestamp remained unchanged through relaunch and navigation. Collection staging updated the row label, chart hatch, checkmark and tray count. Adding a generated 450 KB file updated Downloads from two files/1.1 MB to three files/1.6 MB, retained navigation and the collected item, and appeared in indexed search. Name sorting and chart-to-row highlighting worked after the update.
- PASS: before/after warm release measurements and independent fixture totals. The 20,006-file scan improved from 129.0 to 104.3 ms; saved-root loading from 12.5 to 6.2 ms; persistence from 272.8 to 207.6 ms. Chart preparation is retained across pointer updates. TECHNICAL-NOTES.md records fixture sizes, repeated-run methodology, and remaining first-calculation costs.
- PASS: read-only review of cache invalidation and metadata identity handling found no actionable issue. Colors, typography, focus, and interaction targets are unchanged in this iteration.

Native testing preserved the user's other running app sessions and used generated files only. No personal files were moved or deleted. Physical drag/drop was not repeated in this iteration; the user's earlier mouse confirmation and the updated interaction regressions remain the evidence for that path. These timings are warm fixture and calculation measurements, not cold whole-Mac scans or frame-latency guarantees. Older macOS versions and VoiceOver remain untested.

## Usable maps during scanning (0.8.0)

- PASS: final release app build, bundle property-list validation and strict ad-hoc signature verification.
- PASS: all 24 core checks, including progressive discovery, pending/scanning/complete states, exact completed totals, hard-link allocation, bounded preview hydration, cancellation and unchanged final scan results. Existing index, stale-write and metadata checks remain passing.
- PASS: all seven app/model check groups. A deliberately held scan published a browsable preview in about 65 ms. Checks cover pending-folder navigation, completed-item collection, incomplete-item rejection, removal locked until persistence, deferred updates throughout a mouse gesture, completion handoff, cancellation, restart route and selection retention, stale root-switch suppression, and browsable results after a failed save. Existing indexing, monitoring, collection and removal regressions pass.
- PASS: all 17 native interaction/layout assertion groups in debug and optimized builds. Mouse activity is released after clicks, rejected drags, native drag completion and view teardown. Reordered rows cannot make the second click open a different folder. Pending chart states, stable branch ordinals and existing hit-testing/collection behavior are covered.
- PASS: native click-through in an isolated test bundle with a generated fixture and an intentionally paused scanner. The explorer displayed a completed file and waiting folders while scanning continued. Double-clicking Downloads opened its waiting state, and the center returned to the root. Collecting the completed file updated the chart, row and tray. Quick Look opened and closed normally. Review Collection remained available, with both removal actions disabled and an explanation visible.
- PASS: native Stop Mapping retained the open Downloads folder and exposed Restart Mapping. Restart finished the scan while retaining Downloads and the collected file. The final folder showed its two files and 1.1 MB; indexed search then found the collected file with its In Collection marker. Releasing the older cancelled worker did not replace this result.
- PASS: design gate. The established surfaces, system typography and action hierarchy remain in use. Waiting and unfinished sizes have text labels, and pending folders are not represented by fabricated chart sectors. Controls explain unavailable actions. The compact progress footer leaves browsing available; colors and pressed targets stay stable while results update. Secondary text computes to 9.15:1 against the main surface and 8.36:1 against the raised surface.

The scanner gate and injected delay exist only in the verification harness, outside the delivered source. The final build also corrects singular file labels and derives the partial-footer count from the displayed preview. Tests used generated files and did not quit the user's other app sessions. Physical drag/drop was not repeated; earlier user-confirmed drops and the updated native gesture regressions cover that path. Whole-Mac first-scan latency, older macOS versions and VoiceOver were not measured in this iteration. Partial previews remain session-only; a finished scan is still required to save the index.

## Outside-click dismissal (0.8.1)

- PASS, build: final release app compiles, its property list validates, and strict ad-hoc signature verification succeeds.
- PASS, native interaction: outside clicks dismissed collection review, Quick Look, skipped-item details, the permanent-deletion confirmation, and the native Trash confirmation. An inside click left the permanent confirmation open. Nested outside clicks cancelled only the confirmation and retained collection review. The collected file remained staged and intact. Done and Escape still dismissed review.
- PASS, implementation review: local mouse-down events are restricted to the sheet's own window family and consumed on dismissal. A parent sheet with an attached child ignores the click. Monitors are removed when inactive, detached or dismantled. Dismissal stays disabled during removal. Error alerts wait until preview/details sheets close so dismissal cannot clear the wrong presentation state.
- PASS, design gate: the existing layout, contrast-checked colors, typography and keyboard controls are unchanged. The new behavior provides a direct mouse exit with no additional visual controls; its purpose is recorded in DESIGN.md. Build and native click-through evidence cover the changed interaction.

Verification used an isolated bundle and generated fixtures; the user's running app remained open. The launch-error alert had no reachable app background for CUA's outside click, so it was closed with OK and that outside-click case remains unverified. Older macOS versions, VoiceOver and removal-in-progress were not exercised in this iteration; the removal lock was checked in source. Scanner/model suites were not rerun because those files are unchanged.

## Stable layout during interaction (0.8.2)

- PASS, release: final production build, property-list validation and strict ad-hoc signature verification.
- PASS, measured frames: 39 rendered states in 1280 × 810 and 1060 × 680 content areas showed zero measured movement in the tracked header/search/actions, breadcrumbs, map, metrics, panel, inspector/actions, footer and tray. Cases included file/folder/long-name selection, collection, normal/empty/long-name folder navigation, no-result and over-300-result searches, hover, drag feedback, partial mapping and completion. Rows remained 54 points high. Collection header/list/actions retained their frames for normal, error and busy states.
- PASS, scrollbar regression: the initial measurements found a 17-point size/action-column shift when a folder stopped needing a scrollbar. The enclosing Contents scroll view now reserves its scrollbar gutter without changing the system scrollbar style. Strengthened checks include size/action column positions; all passed with zero movement.
- PASS, transitions: 24 intermediate frame-preference updates across 16 navigation/search transitions showed zero column movement, including root to a one-file folder, returning to the root, and newly created lists after empty folders or no-result searches. These checks observe intermediate updates as well as settled frames.
- PASS, native click-through: separate verification bundles displayed root and folder views, file selection, 350-match search, collected search results and long names. At the minimum supported size, the final build retained the scrollbar gutter for a two-file folder, kept long names within the header and rows, and allowed selection and collection without moving panels or controls. The footer was tightened and native row insets removed so two full rows remain visible at that size.
- PASS, map center: rendered checks at 210-point and 400-point chart heights covered root/nested navigation, long-name hover, groups, pending and scanning states. Fixed text and Up slots keep their baselines steady; compact labels fit inside the smaller ring.
- PASS, hard gate and purpose: existing keyboard navigation, focus, Done/Cancel controls and contrast-checked palette remain in use. Fixed regions prevent moving targets; full paths remain available through the inspector and row help. No new assets or unsupported claims were added.
- PASS, direction and craft: the existing charcoal/coral design, system typography and restrained motion remain intact. DESIGN.md records the purpose of each reserved region. The app builds and the changed interactions were exercised in native windows.

Measurements came from instrumented copies of the views with frame-preference probes, hosted by native SwiftUI in a separate verification process. The reproducible harness is retained under Tests/OrbitLayoutChecks; generated views, fixtures and measurements stay in .build. Geometry values are logical points. These checks cover the recorded states and transitions, not every possible macOS/font/accessibility configuration. Window resizing, meaningful map-sector changes and intentional hover-driven scrolling are not treated as layout defects. Scanner/removal code was unchanged and its suites were not rerun; no personal files were moved or deleted, and the user's running session stayed open.

## Night app icon (0.8.3)

- PASS, source fidelity: all ten generated icon variants match the approved SVG renders pixel for pixel at their seven distinct pixel dimensions, from 16 through 1024. Dimensions and transparent corners were checked. The SVG itself is unchanged.
- PASS, packaging: the release app declares `CFBundleIconFile = OrbitDisk`; its bundled `OrbitDisk.icns` matches the generated asset's SHA-256. All ten standard 1×/2× variants are included. Normal app builds copy the prepared ICNS without requiring Node or image libraries.
- PASS, native resolution: `NSWorkspace.icon(forFile:)` resolves the actual delivered app to the Night icon. Its 512-pixel render was visually inspected for geometry, materials and transparent margins.
- PASS, native sizes: AppKit rendered the ICNS and source PNGs identically at 16, 32, 64, 128, 256, 512 and 1024 pixels, with nonempty artwork and transparent margins confirmed. `iconutil` extraction alone changes RGB values at 44 partially transparent edge pixels in each legacy 16- and 32-pixel 1× representation; alpha is unchanged. All other extracted variants match exactly. Native drawing confirms that the extraction discrepancy does not alter the displayed icon.
- PASS, release: production build, property-list validation and strict ad-hoc signature verification succeeded.
- PASS, design gate: the user-approved Night geometry, palette and material treatment remain intact. The asset has no external resources. The icon adds product identity to Finder and the Dock without changing in-app layout or interactions.

The editable SVG, ICNS and regeneration script are included in Assets; the iconset is generated on demand. Scanner, collection and layout code were unchanged, so their suites were not rerun. Verification read the bundle without launching or quitting the user's app; an already-running Dock icon may need an app relaunch to refresh.

## Repository cleanup and publication

- PASS: a fresh release app build, property-list validation and strict ad-hoc signature verification.
- PASS: all 24 core checks, all seven app/model groups and all 17 native interaction checks.
- PASS: the retained `check-layout.sh` suite reproduced 39 states and 16 transitions with zero measured structural/column movement, 54-point rows and no failures. The harness now asserts that collection and drag cases actually execute; its temporary fixture lives outside hidden build directories so collection policy does not reject it.
- PASS: source review found no credentials, personal scan data or machine-specific user paths. Build products, node modules and generated iconsets are ignored by Git.

The repository retains source, tests, build scripts, design/technical notes, the editable icon and its bundled ICNS. Superseded icon concepts, screenshots, diagnostic binaries, sample fixtures, logs and build caches were removed. Historical screenshot references were replaced by the recorded observations. Direct test compilation now explicitly targets macOS 14, matching SwiftPM. Generated layout probes and reports are reproducible under `.build`; they are not committed.
