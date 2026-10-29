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
- PASS: the retained `scripts/checks/check-layout.sh` suite reproduced 39 states and 16 transitions with zero measured structural/column movement, 54-point rows and no failures. The harness now asserts that collection and drag cases actually execute; its temporary fixture lives outside hidden build directories so collection policy does not reject it.
- PASS: source review found no credentials, personal scan data or machine-specific user paths. Build products, node modules and generated iconsets are ignored by Git.

The repository retains source, tests, build scripts, design/technical notes, the editable icon and its bundled ICNS. Superseded icon concepts, screenshots, diagnostic binaries, sample fixtures, logs and build caches were removed. Historical screenshot references were replaced by the recorded observations. Direct test compilation now explicitly targets macOS 14, matching SwiftPM. Generated layout probes and reports are reproducible under `.build`; they are not committed.

## Public updates and tagged releases (0.9.0)

- PASS, package: universal arm64/x86_64 app builds with macOS 14 minimum, preserved Sparkle framework symlinks and license, concise English strings, and strict deep code-signature verification.
- PASS, regressions: 24 core checks, seven app/model groups, 17 native interaction groups, and 39 rendered layout states across 16 transitions. Tracked structural/column movement remains 0.0 points and rows remain 54 points high. The layout harness still emits the existing AppKit table reentrancy warning on this macOS version; assertions pass.
- PASS, updater guards: manual/background/probe checks, late update discovery, termination protection, postponed restart, repeated removal, exactly-once continuation and abort cleanup are covered by the focused updater suite.
- PASS, release validation: 13 tests cover metadata, version progression, archive URL/length, signing requirements, and complete-or-absent Apple credentials. Two copy tests check vendor-key matching, preserved strings and format placeholders. A source-only checkout resolved Sparkle and passed copy tests without pre-existing artifacts.
- PASS, native update: an isolated 0.8.9/build 11 test app downloaded a signed 0.9.0/build 12 archive from a loopback feed, showed Update and Update and Restart, replaced itself, relaunched, and reopened its saved sample map. The installed bundle reports build 12 and passes strict deep signature verification. A subsequent manual check reports it is current. Settings toggled automatic checks on and off.
- PASS, tamper rejection: changing signed feed build metadata caused a feed-signature error. Changing one archive byte without changing its length caused an archive-signature error. Both left the installed build at 11. The valid archive then installed successfully. Only generated test data and a separate bundle/preferences/cache identity were used.
- PASS, hard gate: the final update notice and settings were visually inspected in native windows, with no clipped text or dead actions. Native controls retain keyboard/accessibility behavior. No assets, fabricated claims, or decorative UI were added.
- PASS, purpose gate: Settings keeps update preferences out of the explorer layout. The notice states versions once and labels the download and restart actions explicitly. Skip This Version retains its actual meaning. DESIGN.md records these decisions.
- PASS, liveliness gate: the existing ENERGY 2 / RHYTHM 2 / MOTION 1 direction remains intact. The app icon identifies the update dialog and the primary action supplies its focal point; native spacing and focus remain in use.
- PASS, craft gate: the checked-in English overlay is merged into the staged framework before re-signing. Tests fail if expected Sparkle keys change. Other locales and installer behavior remain unchanged.

Apple Developer ID signing and notarization are implemented as an optional pipeline path, but cannot be tested against Apple's service without credentials. Current releases are ad-hoc signed; initial internet downloads require macOS approval. The native update test ran on Apple silicon; Intel is cross-compiled and checked for architecture, not executed. VoiceOver, older macOS releases, and updates requiring administrator authorization were not exercised. The published workflow runs the same suites on macOS 26 with Xcode 26.6; its results are available in GitHub Actions.


## Steady hover and faster pointer feedback (0.9.1)

- PASS, universal release build: Swift 6.3.3 compiled Apple silicon and Intel executables with the macOS 14 deployment target; the packaged app passed strict deep signature verification.
- PASS, native pixels: 30 rendering checks cover 144 interior sample points across eight hover/gap states, simultaneous hover and selection outlines, collection hatching/badges, and base-layer invalidation. Fixed code changed interior colors by 0/255. The same pixel assertions against the previous committed graph failed six checks with color shifts up to 24/255. The new suite runs in both GitHub workflows.
- PASS, native interaction: all 19 groups pass, including exact cached/uncached path-containment parity at sector and gap boundaries, revision/resize invalidation, stationary hover, click/drag thresholds and retained drag sources.
- PASS, model: the full isolated app-model suite passes. New checks cover repeated grouped hover, changed equal-count membership and metadata, navigation, search replacement, leaving search and sort order. Existing progressive mapping, refresh, collection and removal safeguards pass.
- PASS, measured layout: 39 rendered states and 16 transitions at 1280 × 810 and 1060 × 680 retain 0-point maximum measured structural/column movement, 54-point rows and no failures.
- PASS, measured work: exact hit testing over 1,024 positions improved from 30.836 to 1.865 ms with 72 sectors and 317.754 to 25.652 ms with 768 sectors, across five alternating optimized trials with matching results. Re-entering a cached 100,000-member group improved from 146.67 to 0.0018 ms median. Initial group relation work remains unchanged; these are calculation timings, not end-to-end input latency or FPS.
- PASS, final native inspection: an isolated packaged app mapped generated files, opened a folder from its sector, selected a list item, added it to collection, and navigated up through the center. The selected outline, full collection hatching/checkmark and partial ancestor badge remained visible and aligned with the list. No personal files were moved or deleted.
- PASS, release validation and copy: 13 release-script tests, two updater-copy tests, version/tag validation and whitespace checks pass.
- PASS, hard/purpose/liveliness/craft gates: branch color stays fixed while the white outline locates the hovered item. Native navigation, selection and collection affordances remain clear, without new controls or layout changes. ENERGY 2 / RHYTHM 2 / MOTION 1 remains the design direction. Cached drawing and hit paths invalidate when their data or geometry changes; independent source review found no actionable regressions.

Pixel checks use native offscreen rendering and interaction checks use AppKit events. A physical-mouse FPS/latency measurement was not performed. The packaged app was exercised on Apple silicon; Intel and older macOS versions were not run locally.


## Native toolbar and quieter list scrolling (0.9.3)

- PASS, native top-edge inspection: sidebar and canvas backgrounds continue through the transparent title bar, with the sidebar divider intact. The header has no icon or wordmark. The compact toolbar no longer uses containers taller than the title bar. Focused 1280→1060→1280 measurements place both 40-point containers at y6–46 inside the 52-point bar, leaving six points of clearance above and below, with zero layout shifts; these bounds are now asserted by the full suite. Search click/type/clear and ⌘F focus worked, Open Folder opened the native picker and Escape closed it, and Refresh updated the generated fixture's index. Toolbar accessibility inspection exposes distinct Refresh and Open Folder labels.
- PASS, measured native layout: 40 rendered states and 16 navigation/search transitions retain 0-point maximum structural/column movement and 54-point rows. At 1280 × 810 and 1060 × 680, the location starts at x238, matching the content navigation; toolbar centers align within 0.5 points of the traffic lights. All controls remain visible through shrinking and expanding, and the final action keeps a 20-point trailing margin. The pre-macOS-26 toolbar branch also passed six states on the current OS; older macOS itself was not run.
- PASS, selection and collection styling: native inspection shows a warm selected row with a thick coral outline; temporary hover uses a neutral fill and thin slate outline. Selected text contrast is at least 6.14:1, hovered text at least 7.40:1, and outlines at least 5.47:1 and 3.71:1 respectively. The collection footer has no idle inner box; its existing drop area and drag feedback remain. A fixed 12-point gap separates the index status from the drop zone. The redundant file-size explanation was removed from the status line.
- PASS, app regressions: the full app-model suite and all 19 native interaction groups pass, including selection, navigation, linked hover and collection drag behavior.
- PASS, focused scroll checks: nine optimized native checks cover phase-less wheels, gesture and momentum overlap, scrollbar live scrolling, gesture cancellation, event scope, clip-origin changes, actual-pointer row replacement, inactive-window guards, teardown and the real quiet timer. The tracker does not retain discarded lists or publish row churn during scrolling.
- PASS, implementation review: independent review found no actionable regressions in the list tracker, row appearance adapter or toolbar changes. Normal selection, collection drag payloads, scanner and removal behavior retain their existing implementations.

### Design delivery gate

- PASS, R-02/03/17/18/23/24/27/28/33/34/36/38: changes contain no marketing claims, fabricated content, new visual assets or dead navigation. Native empty, mapping and error states remain; this is a desktop-only app. Production changes are checked-in Swift source.
- PASS, R-26/32/35 and C-2/4: the universal app was built and exercised in native windows. The recorded search, clear, keyboard-focus, folder-picker, Escape and Refresh checks cover the changed toolbar controls; selection and scrolling retain native behavior.
- PASS, R-25: measured row text and outline contrast exceeds the applicable threshold; collection text remains at least 6.57:1 during drop feedback and 8.74:1 at rest.
- PASS, purpose rules R-01/04/06/07/08/09/10/12/13/14/19/22: DESIGN.md records the map's data purpose, branch colors, native action symbols, system type, persistent selection and temporary hover. The icon was removed from the header as requested; no decorative replacement was added.
- PASS, R-37 and liveliness: the established ENERGY 2 / RHYTHM 2 / MOTION 1 direction remains. The map is the focal point, coral marks actions and selection, and the fixed sidebar and inspector organize navigation and details.
- PASS, C-1/3/5 and consistency rules R-05/11/15/16/20/21/29/30/31: the existing disk explorer composition, native labels, typography and charcoal/coral palette remain; graph colors encode filesystem branches. The footer keeps its functional drop area while removing its idle box.

Tests use generated files in isolated bundles. Physical-device frame rate, VoiceOver and older macOS releases were not measured. Intel is compiled and packaged but not run locally.


## Large Contents lists (0.9.4)

- PASS, native row work: the final implementation mounted 11 row bodies initially and evaluated 132 bodies across 12 hover or selection updates in synthetic 190-, 1,000- and 10,000-item folders. The previous implementation evaluated 2,000 rows initially and 12,000 per update set at 1,000 items, despite realizing only 11 native rows. All entries remain available in the native table.
- PASS, measured interaction work: removing the redundant row identity modifier and extracting the row view reduced 12 hover updates from 8.12 to 0.99 seconds and 12 selection updates from 9.57 to 1.09 seconds in the 1,000-item fixture. A later adjacent 10,000-item comparison isolated the additional stored-ID cache: hover 921→667 ms and selection 947→685 ms. These optimized native-host measurements include forced layout/display passes and asynchronous waits; they are not physical-device input latency or FPS. Runs made during concurrent compilation were excluded.
- PASS, model freshness: cached identifiers are reused during hover and selection, and replaced after navigation, sorting, entering/leaving search, and refreshed folder or search metadata with unchanged paths. The full app-model suite passes, including filesystem event delivery and collection safeguards.
- PASS, layout regression: 40 rendered states and 16 transitions retain 0-point measured structural movement and 54-point rows. Nine new phases on a 10,000-item native list pass: initial, hover/exit, selection/clear, graph hover to last/first, and native scrolling to last/first. Hover and selection evaluated 11 bodies each; distant graph hover evaluated 22, returning 10. Native scrolling reused existing bodies. Target-row visibility and actual row hover/selection values were asserted.
- PASS, packaged native app: an isolated production build mapped 1,001 generated files. Clicking a list row and pressing Down selected the next row and updated the inspector. Selecting a folder and pressing Return opened its contents. The existing selection outline and compact layout were visually inspected. No personal files were moved or deleted.
- PASS, independent implementation review: no actionable regressions found in identity, selection, graph scroll targeting, native drag adapters, cache invalidation or ownership. The row view preserves the existing interaction and accessibility modifiers.

The unchanged charcoal/coral palette, typography, spacing and controls retain the previous design gate. This change removes unnecessary rendering work without adding visual elements. Physical-device frame rate, VoiceOver and older macOS releases were not measured; Intel execution remains outside local coverage.


### Dense graph hover

- PASS, actual pointer path: synthetic AppKit mouse-move events through the real map interaction view reached the model in full native ContentView hosts with 72 and 768 segments. Across 12 hover updates, the base Canvas draw count, base body count and hit-region rebuild count remained zero; only 12 active outline paths were constructed. The graph's cached base rendering was retained.
- PASS, prepared group semantics: optimized model checks compare prepared relationships against the previous member walk for root, nested and single-member groups; exact search members and ancestors; excluded descendants and unrelated siblings; refreshed results and metadata; stale revision/root/path-boundary rejection; direct mutations; and filesystem root `/`. The full model suite passes.
- PASS, measured cold-group cost: three alternating trials with 100,000 members reduced root-group relationship calculation from 359.9 ms to 0.012 ms and descendant-group calculation from 306.7 ms to 0.020 ms. One-time preparation increased from 106.5 to 136.3 ms. Timings exclude native drawing, scrolling and input-device latency.
- PASS, independent review: no actionable correctness findings in prepared group ancestry, search equivalence, invalidation or ownership. Metadata preparation does not change graph geometry, collection eligibility, drag payloads or removal behavior.
- PASS, final graph regressions: all 30 native rendering checks and 19 interaction checks pass with prepared groups. The 144 interior pixel samples per state show a maximum hover color change of 0/255; collection hatching, selection and hover outlines remain intact.

- PASS, native scrolling: two reversed 10,000-item comparisons reduced repeated distant graph-hover reveals from about 302–409 ms to 58–72 ms. The first distant reveal in one pair improved from 1,334 to 154 ms. At 1,000 rows, the first reveal improved from 176 to 90 ms. A 48-row dense-graph comparison was slower with native scrolling, so lists below 1,000 retain the original SwiftUI path. The threshold runs before the row-index lookup.
- PASS, final integrated layout and hover checks: all 40 rendered states, 16 structural transitions and 12 large-list phases pass with 0-point measured layout movement and 54-point rows. Same-count sort/search replacements reveal the exact expected native row identity; selection and grouped-hover ordering remain intact. All ten native hover-check groups pass, including threshold, stale-count, invalid-index, visible-row and detach cases.
- PASS, final packaged build: 0.9.4/build 16 builds and passes strict deep ad-hoc signature verification. An isolated native app reopened its saved 1,001-file fixture, preserved keyboard selection while scrolling, and returned to the first related rows when its graph group was activated. Neutral linked-hover outlines and the distinct selected outline remained visible together.
- PASS, release metadata: all 13 release-validation tests, two updater-copy tests, tag/version progression validation and whitespace checks pass.

## Earlier mapping-progress designs (unreleased)

- PASS, portable progress checks: all eight portable groups pass, including cold and previous-map estimates, revised totals, uncertain frontiers, cancellation and independent save-work accounting. Sparse and directory-only samples can estimate without a minimum file count; a single unfinished flat listing retains an unknown total. Session checks verify positive estimates below 1% display as “<1% estimated”.
- PASS, asynchronous mapping: discovery completes with the writer held; cache counts survive resume; retries and publication report their actual phases. Count-only reads preserve replacement/removal identities and explicitly avoid full metadata/hardlink aggregate construction.
- PASS, discovery completion after writing starts: the regression first commits a real listing batch, then holds the writer while remaining discovery finishes. It reproduced the old failure and now reports memory at 100% with cache progress still behind. Shared-map reuse keeps memory complete through cache import and successful journal settlement; a genuine deferred directory retry remains incomplete until it is reread. Cache-only uncertainty still suppresses an unreliable cache percentage.
- PASS, app sessions: the real model reports full memory progress after SQLite ingestion starts while the writer is held and cache progress remains behind. Progress stays with its owning root during child navigation and location switches. Full refresh counts its replacement inventory while retaining the old visible map; incremental refresh keeps completed memory filled across navigation. Returning after offscreen discovery completes refreshes the count without another discovery event. Cancelled and failed sessions release their stage leases. Existing session, preview-batch and folder-transaction checks also pass.
- PASS, native layout: the overlapping-bar baseline covers 76 states at 1060 × 680 and 1280 × 810, plus 15 lazy-list phases at 10,000 rows, with zero measured structural movement and 54-point rows. After regrouping the text and adding unknown-total activity, 11 focused native states retain fixed count anchors, trailing status edge, track, action slot and footer geometry. The shared-track progress content is 51 points high, with a 12-point gap above the collection tray. File size's label and value align toward the center divider. Native sizing checks keep short root names beside the index date and cap long names. An explicit empty action slot preserves status-row geometry after completion.
- PASS, native controls and pixels: Stop, Resume, refresh Pause and completed-state action suppression work in isolated fixtures. Overlapping-fill pixel checks verify the shared scale: cache covers the saved share of the memory extent and never extends beyond it. A 52% memory estimate with 63% of known save work complete paints about 32.76% orange; a caught-up cache fully covers the memory fill. A completed map fills it orange. Nested transforms preserve the bound during animation. The final unknown-total treatment uses ivory activity stripes: four native captures show movement in normal mode and identical pixels with the explicit Reduce Motion branch. Unknown-to-known-to-unknown transitions stop and restart activity. Toggling the reduced-motion input during activity stops the existing repeating animation immediately; toggling it off restarts motion without moving the track or action slot. The focused fixture substitutes the read-only environment accessor to exercise these branches; system preference toggling, VoiceOver and physical-device frame rate were not measured.
- PASS, packaged app: the final local progress build completes and passes strict deep ad-hoc signature verification.

Checks used generated fixtures and isolated cache directories. Discovery estimates remain approximate; no new whole-Mac performance or estimate-accuracy benchmark is claimed. README images were not regenerated.

## Files found and saved (unreleased)

The user selected a shared track for the files found so far and their saved share. This replaces the earlier estimated-total track described above. The count labels are “found” and “saved”; one lifecycle phase distinguishes ongoing discovery, cache saving, completion and updates. Unknown saved counts use a static hatch rather than a false zero percentage.

- PASS, live model: current SessionChecks cover discovery with cache writing held, caught-up counts while discovery is still active, empty saved maps, incompatible count snapshots, pending deletion, incremental refresh, offscreen navigation and stop/resume. A queued filesystem event after initial discovery retains the updating phase until it settles; publication alone is not enough to label a map saved while that event is pending.
- PASS, native rendering: 32 state/width cases and six reduced-motion cases keep the footer and progress footprint fixed at 794- and 1,174-point content widths. Pixel checks verify the saved-share boundary, contrasting divider, unknown-count hatch and empty-map states. Reduced-motion idle captures are identical. These use generated sample counts, and the read-only Reduce Motion environment accessor is substituted in the separate reduced-motion fixture; the system preference was not changed.
- PASS, native actions: CUA clicked the production footer in an isolated fixture. Stop incremented its cancellation counter; Escape incremented it again; Resume invoked continuation; Pause invoked cancellation; Refresh invoked the existing full-refresh action. The completed state removes the action. Accessibility exposes both count labels and the current phase. Full Keyboard Access and VoiceOver were not exercised.
- PASS, palette: muted text, ivory and coral have respective contrast ratios of 9.15:1, 13.85:1 and 8.15:1 against the charcoal canvas. A one-point charcoal divider separates the adjacent fills.
- PASS, installed app: the release build passed strict deep ad-hoc signature verification. The installed executable UUID matches the compiled executable. After restart, the saved Downloads map displayed 184 found, 184 saved and “Map saved”. No personal files were deleted or cache cleared.

The bar is a saved share of the known inventory, not an estimate of completion through the whole folder. Its share can decrease when discovery finds more files. No discovery-speed improvement or new whole-Mac benchmark is claimed by this UI change. README images were not regenerated.

### Compact completion and indexed wording

- PASS, native transitions: four focused renders using the unchanged production view cover indexing, completed with timestamp, updating, and completed without timestamp. Progress pixels disappear in both completed states and return during updates. Content remains 51 points high, with the same 12-point bottom padding. The displayed counts now say “found” and “indexed”; related phase, help and accessibility text use the same wording.
- PASS, completion timestamp: the installed app reopens Downloads with “Indexed · updated … ago”, preserving the saved index date. The progress counts are absent from its accessibility tree. The nil-date fixture shows only “Indexed”.
- PASS, local delivery: the final release build passes strict deep ad-hoc signature verification, and the installed executable UUID matches the compiled executable. The new build is open on Downloads. No cache was cleared or personal files deleted.

## Whole-drive coverage and Places reuse (0.9.14)

- PASS, native boot-volume traversal: bulk, streaming and fallback enumeration normalize verified firmlink identities before secure opening. Checks cover Home/Documents and nested System/Library/Caches coverage, stale identities, ordinary volume boundaries and duplicate physical alias rejection.
- PASS, totals and durable queueing: a generated inventory using actual boot identities counts native and unique Data-volume files once in live views, count-only reads and SQLite. The Data mount remains accessible and queued; mirrored physical folders do not enter the queue.
- PASS, asynchronous mapping regressions: the complete native suite covers secure cursor reuse and root replacement, browsing/search/removal before cache writes, held writers, resumed mappings, wide listings, backlog and interrupted-process recovery.
- PASS, cloud exclusions: native bulk/fallback, synchronous/asynchronous inventory, saved search and obsolete-cache replacement retain cloud exclusion behavior.
- PASS, old-map presentation: four isolated model cases cover an absent/v1 boot marker, a current v2 map and a resumed old stage. Old maps retain their saved database while exposing the new live preview; current maps retain normal refresh presentation. The scanner was gated before real boot enumeration.
- PASS, completed-child reuse: coverage and subtree checks retain cancellation, interrupted imports, removal fences, hardlink accounting and replacement-source behavior. Native event-history checks accept a quiet directory and reject changed history; host and device cursors are not compared. Legacy root stages preserve their rows and upgrade once. A blocked candidate lookup does not serialize other discovery workers. Monitor checks cover the root-version refresh and unique physical Data events.
- PASS, model reuse: the complete mapping-reuse script passes, including existing ancestor-session navigation, completed descendant imports and retained cache behavior.
- PASS, installed app: the release build and strict deep signature verification pass; installed and compiled executable UUIDs match. Clicking Macintosh HD exposes accessible Users and Applications while mapping continues. Opening Home retains `/` as the mapping root and reports no error; returning to Macintosh HD retains that session. The app is left open on the drive. No personal files were removed or caches cleared.

These checks use generated fixtures and native metadata. They do not claim a new whole-Mac throughput measurement. README images were not regenerated.

### Published-map progress handoff (0.9.14)

The v0.9.13 release gate caught a saved-state race after deleting the last file while switching Places. The index had published, but the displayed counts still belonged to the preceding map. The model now keeps the updating phase until the installed folder and session snapshot match the published revision.

- PASS: a synchronous progress observer reproduces premature completion against the tagged v0.9.13 model and passes with the fix. The full session harness also passes, including detached removal, navigation, interrupted mapping and progress ownership. This comparison used the same optimized core module, generated files and an unoptimized app-model harness.
- PASS: 13 release-metadata checks, two updater-copy checks and version/build validation. v0.9.13 was blocked before publication; v0.9.14 includes the same changes plus this handoff fix.
