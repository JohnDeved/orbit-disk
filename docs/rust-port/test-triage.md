# Test triage for the Rust port

Method, after [pingdotgg/t3code#22557](https://github.com/pingdotgg/t3code/pull/22557):
- Every Swift check case (787 after merging near-duplicates) was read by two independent Claude Haiku 5.5 passes against one rubric (`docs/rust-port/README.md`).
- Cases both passes found and agreed on are final (488). The integrator settled the 28 disagreements as a third vote.
- Cases only one pass delimited (271, mostly granularity differences inside large `main.swift` files) keep that pass's verdict.

**Result: port 733, drop 54.**

Unlike t3code, this suite is dominated by durability, race and protocol checks, which the rubric keeps. The savings come from three things instead:
- dropping AppKit/SwiftUI mechanics that the GPUI design removes
- replacing sleep-polling with injected clocks and deterministic hooks
- collapsing several Swift cases into one Rust test where they share a fixture

A "keep" row is a behavior that must be covered, not a 1:1 Rust test. Owners may merge rows into one test. Machine-readable copy: `test-triage.json`.

| Kept, by target crate | Cases |
|---|---:|
| `orbit-app-state` | 321 |
| `orbit-core` | 281 |
| `orbit-platform-macos` | 55 |
| `orbit-automation` | 36 |
| `orbit-sunburst` | 24 |
| `orbit-ui` | 15 |
| `orbit-shell-macos` | 1 |

| Dropped, by reason | Cases |
|---|---:|
| appkit-mechanics | 31 |
| duplicate | 10 |
| mirrors-impl | 6 |
| smoke | 3 |
| copy-text | 2 |
| perf-assert | 2 |

## Dropped

| Source | Reason | Why |
|---|---|---|
| `OrbitAppChecks.swift:182` | mirrors-impl | Asserts cached buffer pointer identity; allocation detail rather than user-visible behavior. |
| `OrbitAppChecks.swift:568` | mirrors-impl | Asserts prepared row buffer pointer identity; allocation detail, not behavior. |
| `OrbitAppChecks.swift:608` | mirrors-impl | Asserts identity of row buffers across sort changes; allocation detail, not behavior. |
| `OrbitAppChecks.swift:1275` | duplicate | Same as cached-map hover hold case at line 74. |
| `OrbitCollectionSessionChecks/main.swift:229` | appkit-mechanics | Counts objectWillChange sends; GPUI notify batching is structural. |
| `OrbitFolderPlacesChecks.swift:33` | appkit-mechanics | NSItemProvider and pasteboard file-URL decoding; typed in-app drops replace it. |
| `OrbitFolderTransactionChecks/Notifications.swift:31` | smoke | Prints around a selection change during root install; asserts nothing. |
| `OrbitFolderTransactionChecks/Notifications.swift:31` | smoke | Prints around a selection change during beginPreview; asserts nothing. |
| `OrbitInteractionChecks/main.swift:256` | appkit-mechanics | NSView hitTest on an overlay view; analytic hit-testing replaces it. |
| `OrbitInteractionChecks/main.swift:260` | mirrors-impl | Asserts cached hit-region object identity, restating the caching implementation. |
| `OrbitInteractionChecks/main.swift:362` | appkit-mechanics | NSView mouseMoved and mouseExited tracking callbacks. |
| `OrbitInteractionChecks/main.swift:366` | duplicate | Duplicates OrbitAppChecks checkPreparedGroupHover stale-revision rejection case. |
| `OrbitInteractionChecks/main.swift:423` | appkit-mechanics | NSTableView focus restoration and native arrow/context-click targets; NSView-specific. |
| `OrbitInteractionChecks/main.swift:461` | appkit-mechanics | NSTableView gutter hit geometry. |
| `OrbitInteractionOwnershipChecks/HoverRefreshHostedChecks.swift:24` | appkit-mechanics | Hosted NSHostingController with NSWindow pointer re-hit; model variant kept separately. |
| `OrbitInteractionOwnershipChecks/main.swift:73` | duplicate | Mirror of chart-completion-keeps-new-row-press; kept case covers session isolation. |
| `OrbitInteractionOwnershipChecks/main.swift:88` | duplicate | Mirror of chart-activation-reentrant-press; kept case covers reentrancy. |
| `OrbitInteractionOwnershipChecks/main.swift:111` | duplicate | Mirror of reentrant-chart-drag-setup-no-stale-session; kept case covers it. |
| `OrbitInteractionOwnershipChecks/main.swift:173` | appkit-mechanics | NSDraggingSession end callbacks; stale-completion invariant kept by old-drag-completion case. |
| `OrbitLayoutChecks/LayoutChecks.swift:181` | appkit-mechanics | NSToolbar and traffic-light frame probes; no GPUI equivalent. |
| `OrbitLayoutChecks/LayoutChecks.swift:247` | appkit-mechanics | Observes SwiftUI preference-update stream during navigation transitions. |
| `OrbitLayoutChecks/LayoutChecks.swift:262` | appkit-mechanics | SwiftUI preference-frame snapshots; fixed dimensions move to the DESIGN.md parity checklist. |
| `OrbitLayoutChecks/LayoutChecks.swift:269` | appkit-mechanics | SwiftUI metric alignment frames. |
| `OrbitLayoutChecks/LayoutChecks.swift:315` | appkit-mechanics | SwiftUI footer height across progress phases; layout is view-framework specific. |
| `OrbitLayoutChecks/LayoutChecks.swift:331` | appkit-mechanics | SwiftUI device row frame stability across volume loading states. |
| `OrbitLayoutChecks/LayoutChecks.swift:396` | appkit-mechanics | SwiftUI frame stability on a partial map during discovery; view-framework specific. |
| `OrbitLayoutChecks/LayoutChecks.swift:412` | appkit-mechanics | SwiftUI row column frames. |
| `OrbitLayoutChecks/LayoutChecks.swift:428` | appkit-mechanics | SwiftUI sheet frame probes for error and busy states; view mechanics only. |
| `OrbitLayoutChecks/LayoutChecks.swift:496` | appkit-mechanics | Pixel sampling of the review button; disabled gating is a view concern. |
| `OrbitLayoutChecks/LayoutChecks.swift:567` | appkit-mechanics | Counts SwiftUI body evaluations for NSTableView lazy rows; no GPUI analogue. |
| `OrbitLayoutChecks/LayoutChecks.swift:588` | appkit-mechanics | NSTableView row view rendering; hover model logic covered by OrbitAppChecks. |
| `OrbitLayoutChecks/LayoutChecks.swift:627` | duplicate | Duplicates OrbitAppChecks checkLargeListHoverLookup row index invalidation case. |
| `OrbitLayoutChecks/LayoutChecks.swift:689` | duplicate | Duplicates OrbitAppChecks checkLargeListHoverLookup search-change invalidation case. |
| `OrbitListHoverChecks.swift:124` | appkit-mechanics | NSEvent window routing and NotificationCenter observers. |
| `OrbitListHoverChecks.swift:149` | appkit-mechanics | NSClipView bounds-change notifications drive the check. |
| `OrbitListHoverChecks.swift:158` | appkit-mechanics | NSWindow key-state notifications. |
| `OrbitListHoverChecks.swift:170` | appkit-mechanics | NotificationCenter observer attach and teardown. |
| `OrbitListHoverChecks.swift:185` | appkit-mechanics | Timer and event-monitor retention of an NSView-owned tracker. |
| `OrbitListHoverChecks.swift:195` | appkit-mechanics | scrollRowToVisible row math; uniform_list scroll strategy covers it. |
| `OrbitPlacesInteractionChecks.swift:66` | appkit-mechanics | NSEvent clicks on SwiftUI place rows and frame preferences; GPUI hit-testing differs. |
| `OrbitRemovalJournalChecks.swift:167` | duplicate | Same as waiting-fence cancellation at line 169. |
| `OrbitRenderChecks.swift:122` | smoke | Only checks that rendered pixels are non-dark; no behavior asserted. |
| `OrbitRenderChecks.swift:123` | appkit-mechanics | NSHostingView pixel sampling across hover states; not portable. |
| `OrbitRenderChecks.swift:134` | appkit-mechanics | Pixel sampling of hover outline and selection rendering. |
| `OrbitRenderChecks.swift:141` | appkit-mechanics | Pixel sampling of collection hatch; mark geometry covered by interaction markings case. |
| `OrbitRootGroupChecks/main.swift:105` | mirrors-impl | Asserts buffer pointer identity. |
| `OrbitRootGroupChecks/main.swift:114` | mirrors-impl | Asserts buffer pointer identity of prepared arrays; an implementation detail, not output. |
| `OrbitSavedSearchChecks/core.swift:49` | duplicate | Same as PreviewSearch core cancellation case. |
| `OrbitSessionChecks/main.swift:286` | copy-text | Asserts formatted estimate strings; the 99 percent clamp is presentation only. |
| `OrbitSessionChecks/main.swift:844` | duplicate | Same as active-root replacement at line 802. |
| `OrbitStageSearchFastChecks/parity.swift:4` | perf-assert | Asserts amortized preparation work bound on a schedule function; belongs in benchmarks. |
| `OrbitVolumeChecks/main.swift:40` | perf-assert | Wall-clock startup bound; behavior kept at line 43. |
| `test_update_copy.py:26` | copy-text | Asserts localized Sparkle copy values; merge and idempotence are build tooling. |
| `test_update_copy.py:38` | appkit-mechanics | Patches Sparkle framework resources; no Rust counterpart. |

## Keep → `orbit-app-state`

| Source | Behavior to cover |
|---|---|
| `OrbitAppChecks.swift:70` | Collected-ancestor lookup matches Foundation for normalized, odd, Unicode and relative paths. |
| `OrbitAppChecks.swift:99` | Coverage respects path component boundaries, Unicode spellings, shared ancestors and root. |
| `OrbitAppChecks.swift:113` | Canonically equivalent but byte-distinct path spellings never merged in coverage. |
| `OrbitAppChecks.swift:138` | Group presentation reuse invalidates on membership and metadata change; navigation and search refresh lists. |
| `OrbitAppChecks.swift:185` | Hover relations and descriptions follow navigation and same-path child replacement. |
| `OrbitAppChecks.swift:198` | Search start, change, and clear update rows; equal-count search changes invalidate group relations. |
| `OrbitAppChecks.swift:212` | Sorting reorders cached rows; group relations follow the new order rather than pinning the old one. |
| `OrbitAppChecks.swift:218` | Same-ID folder and search refreshes replace cached size and identity of rows. |
| `OrbitAppChecks.swift:298` | Row index for hovered item invalidated by sort, folder refresh, and search changes. |
| `OrbitAppChecks.swift:338` | Prepared group relations and descriptions match the member-walk for root and deep groups. |
| `OrbitAppChecks.swift:353` | Search-filtered group hover includes members and their ancestors, excluding descendants and siblings. |
| `OrbitAppChecks.swift:358` | Prepared hover uses fresh search results; same-ID metadata changes replace old members. |
| `OrbitAppChecks.swift:363` | Prepared group from a stale revision or different graph root is rejected; current hover kept. |
| `OrbitAppChecks.swift:371` | Prepared group whose parent is a path-prefix sibling is rejected. |
| `OrbitAppChecks.swift:377` | Direct hover assignment clears prepared group membership so stale relations cannot reappear. |
| `OrbitAppChecks.swift:408` | First partial map is browsable before scan completion and before any index is persisted. |
| `OrbitAppChecks.swift:421` | Visible incomplete folders collectable; partial search works; pending folder navigation resolves. |
| `OrbitAppChecks.swift:433` | Held map gesture freezes revision; completion keeps collection, selection, colors. Inject clock for sleeps. |
| `OrbitAppChecks.swift:451` | Cancelled partial map stays browsable with updates paused and the route retained. |
| `OrbitAppChecks.swift:462` | Restart before parent rediscovery keeps existing route; final persistence retains visited folder. |
| `OrbitAppChecks.swift:479` | Restarted scan keeps partial map and selected item until completion. |
| `OrbitAppChecks.swift:502` | Stale preview work from an abandoned root never replaces a newly chosen root. |
| `OrbitAppChecks.swift:519` | In-memory map stays browsable and removable when index save fails; removal lock holds. |
| `OrbitAppChecks.swift:564` | Saved folder with 3000 files exposes every row, color assignment, and lookup. |
| `OrbitAppChecks.swift:572` | Filtered subfolder refresh keeps search results, selection, and lookups valid. |
| `OrbitAppChecks.swift:592` | Cached folder reopen prepares in background; superseded preparation cannot replace newer navigation. |
| `OrbitAppChecks.swift:605` | Name sort runs in background, is cancellable, defers during held gesture, uses localized order. |
| `OrbitAppChecks.swift:638` | Same-count folder metadata refresh replaces cached rows and refreshed search lookups. |
| `OrbitAppChecks.swift:653` | Large preview and its final handoff deferred while a graph gesture is held. |
| `OrbitAppChecks.swift:703` | Prior search rows keep selection, hover, and collection membership until the replacement search lands. |
| `OrbitAppChecks.swift:718` | Superseded search lookups released; clearing search keeps collected items. |
| `OrbitAppChecks.swift:744` | Identity-matched cached map shows before reconciliation; hover and selection do not invalidate prepared data. |
| `OrbitAppChecks.swift:761` | Completed refresh bumps map revision, preserves existing colors, and prefers refreshed metadata. |
| `OrbitAppChecks.swift:778` | Replacing the navigation node with same folder and count still invalidates graph child IDs. |
| `OrbitAppChecks.swift:786` | Root replaced at the same path is scanned fresh instead of displaying the old saved map. |
| `OrbitAppChecks.swift:826` | Deep segment relates to its visible ancestor row; grouped segments relate every represented row. |
| `OrbitAppChecks.swift:833` | Filtering clears prior hover; search rows relate only to the exact hovered match. |
| `OrbitAppChecks.swift:845` | Search result updates invalidate cached rows and hover relations even when the query text is unchanged. |
| `OrbitAppChecks.swift:853` | List hover preserves cached sort order and graph relation; sort change invalidates it. |
| `OrbitAppChecks.swift:861` | Dropped search row collects its own source, not the selection; files untouched. Typed drag replaces pasteboard token. |
| `OrbitAppChecks.swift:888` | Descendants collapse into ancestors; protected and root items rejected; coverage updated. |
| `OrbitAppChecks.swift:900` | Collection total and coverage track removals and clearing immediately. |
| `OrbitAppChecks.swift:911` | Collecting a nested file marks each ancestor as containing collected items. |
| `OrbitAppChecks.swift:919` | Drag token consumed once; unknown token rejected; starting or ending a drag never collects. |
| `OrbitAppChecks.swift:936` | Directory replaced at the indexed path is rejected rather than collected by old identity. |
| `OrbitAppChecks.swift:945` | File replaced at the indexed path is rejected; fixture files stay untouched. |
| `OrbitAppChecks.swift:957` | Seventy-two item drop validates in background, applies one batch, keeps newer status. |
| `OrbitAppChecks.swift:971` | Deferred grouping keeps duplicate and rejected-item counts for mixed drops. |
| `OrbitAppChecks.swift:979` | Dropping items already collected never duplicates them in the collection. |
| `OrbitAppChecks.swift:985` | Later reentrant direct assignment wins with consistent coverage and total. |
| `OrbitAppChecks.swift:998` | Queued collection work retries after a reentrant mutation and ends with consistent coverage. |
| `OrbitAppChecks.swift:1015` | Collection replaced while validation is paused; accepted work retries and reflects the replacement. |
| `OrbitAppChecks.swift:1032` | Accepted collection work resumes when the same root is restored during validation. |
| `OrbitAppChecks.swift:1043` | Reentrant collection mutation during validation triggers a clean retry with correct total. |
| `OrbitAppChecks.swift:1062` | Reentrant root detach during validation keeps accepted collection and resumes on return. |
| `OrbitAppChecks.swift:1079` | Pausing mapping retains accepted collection work and does not overwrite newer status. |
| `OrbitAppChecks.swift:1094` | Starting a new drag invalidates the previous drag token. |
| `OrbitAppChecks.swift:1097` | Changing the scan root invalidates pending drag payloads and drop acceptance. |
| `OrbitAppChecks.swift:1136` | Mapped root and protected folders never enter the collection or permanent removal. |
| `OrbitAppChecks.swift:1155` | Confirmed permanent removal runs in background and removes the selected file and folder. |
| `OrbitAppChecks.swift:1161` | Removing a directory unlinks nested symlinks without deleting their targets. |
| `OrbitAppChecks.swift:1164` | Replaced file or folder identities refused without touching either version; failures stay reviewable. |
| `OrbitAppChecks.swift:1171` | Removal outcome and failure state survive the map refresh that follows removal. |
| `OrbitAppChecks.swift:1181` | Saved index drops removed paths and refreshes rejected replacement paths after removal. |
| `OrbitAppChecks.swift:1193` | Replaced mapped root blocks removal even when the collected file identity still matches. |
| `OrbitAppChecks.swift:1214` | Collection totals saturate instead of overflowing; membership and Unicode ancestry match the collection. |
| `OrbitAppChecks.swift:1229` | Collection total with Int64.max sizes saturates correctly and tracks removal. |
| `OrbitAppChecks.swift:1252` | Same-root click does not rescan; nested location reuses the parent index; indexed search works. |
| `OrbitAppChecks.swift:1262` | Search over the saved index returns the named file. |
| `OrbitAppChecks.swift:1266` | Opening a location already inside the index reuses it and saves no duplicate index. |
| `OrbitAppChecks.swift:1271` | Selected item in a nested folder validates and enters the collection. |
| `OrbitAppChecks.swift:1280` | Automatic incremental refresh deferred during held drag; map and rows stay unchanged until release. |
| `OrbitAppChecks.swift:1289` | Published refresh preserves navigation, collection, hover, and the list rows for the same folder. |
| `OrbitAppChecks.swift:1299` | Deferred refresh cannot overwrite newer navigation; pausing completes search without restarting mapping. |
| `OrbitAppChecks.swift:1314` | Pause completes search on the retained map without restarting the scan. |
| `OrbitAppChecks.swift:1344` | Full refresh keeps the map visible and preserves navigation changed during the scan. |
| `OrbitAutomationChecks/main.swift:216` | A later native removal cannot overwrite the automated operation receipt. |
| `OrbitAutomationChecks/main.swift:235` | Partial-map items can be collected and deleted without waiting for discovery to finish. |
| `OrbitCollectionRemovalChecks.swift:41` | Permanent delete is refused while collection edits are pending; the file survives. |
| `OrbitCollectionRemovalChecks.swift:44` | Drop during a pending edit is queued, not lost; typed drop replaces the pasteboard token. |
| `OrbitCollectionRemovalChecks.swift:47` | A drop queues during in-flight edits; queued removals and the addition all apply. |
| `OrbitCollectionRemovalChecks.swift:54` | Pending removals keep applying after the review sheet closes. |
| `OrbitCollectionRemovalChecks.swift:62` | Rapid collection removals survive closing the collection sheet. |
| `OrbitCollectionRemovalChecks.swift:72` | An accepted removal survives switching locations and returning to the root. |
| `OrbitCollectionRemovalChecks.swift:88` | A location switch inside the launch notification does not lose the accepted removal. |
| `OrbitCollectionRemovalChecks.swift:97` | An accepted removal survives a same-root refresh. |
| `OrbitCollectionRemovalChecks.swift:105` | An older edit does not restore entries after the collection is cleared. |
| `OrbitCollectionSessionChecks/main.swift:62` | A restored map attaches only its own pending validation, never the other session's collection. |
| `OrbitCollectionSessionChecks/main.swift:72` | While reopening, B shows only its own committed collection; review stays usable. |
| `OrbitCollectionSessionChecks/main.swift:78` | After root publication B keeps its collection and busy state and reports the rejected item. |
| `OrbitCollectionSessionChecks/main.swift:80` | Undismissed rejection feedback stays with its root across switches. |
| `OrbitCollectionSessionChecks/main.swift:88` | A deferred rejection cannot overwrite a newer error. |
| `OrbitCollectionSessionChecks/main.swift:95` | An error set during deferred rejection publication is preserved. |
| `OrbitCollectionSessionChecks/main.swift:106` | A rejection from a held job surfaces when returning to its root. |
| `OrbitCollectionSessionChecks/main.swift:114` | An accepted addition stays queued when a row edit re-enters during launch. |
| `OrbitCollectionSessionChecks/main.swift:119` | Row edit fired during a notification keeps an accepted addition queued until the edit ends. |
| `OrbitCollectionSessionChecks/main.swift:126` | An older job cannot overwrite a newer status with identical text. |
| `OrbitCollectionSessionChecks/main.swift:134` | A forced refresh keeps the inherited queued collection status. |
| `OrbitCollectionSessionChecks/main.swift:151` | Offscreen collection completion never publishes items or feedback into another root. |
| `OrbitCollectionSessionChecks/main.swift:169` | A review may close after root invalidation without clearing unowned feedback. |
| `OrbitCollectionSessionChecks/main.swift:173` | Closing a review after root invalidation does not clear feedback it does not own. |
| `OrbitCollectionSessionChecks/main.swift:192` | Review actions carrying a stale owner token are no-ops for the current session. |
| `OrbitCollectionSessionChecks/main.swift:198` | A stale confirmation cannot act on a collection that changed after it was opened. |
| `OrbitCollectionSessionChecks/main.swift:201` | Review confirmation captures collection revision; a changed collection makes it a no-op. |
| `OrbitCollectionSessionChecks/main.swift:211` | A reopened review sheet is a distinct instance from the dismissed one. |
| `OrbitCollectionSessionChecks/main.swift:226` | Row action carrying a dismissed review token leaves the reopened review untouched. |
| `OrbitCollectionSessionChecks/main.swift:248` | A rejected outer review open does not claim a review that was reentrantly opened. |
| `OrbitCollectionSessionChecks/main.swift:257` | Outer review open that is rejected does not steal a review opened reentrantly. |
| `OrbitDurableRemovalChecks/main.swift:129` | Replaced removal journal forces reconciliation before deletion state is retired. |
| `OrbitDurableRemovalChecks/main.swift:131` | Older cursor from a published snapshot cannot retire a newer removal; survivor sizes stay visible. |
| `OrbitDurableRemovalChecks/main.swift:175` | More affected parents than the recovery limit withholds removal and completes via full refresh. |
| `OrbitDurableRemovalChecks/main.swift:260` | Cancelling refresh before recovery publication keeps the old snapshot; retry is clean. |
| `OrbitDurableRemovalChecks/main.swift:263` | Cancelling during private recovery publication leaves the published snapshot unchanged. |
| `OrbitDurableRemovalChecks/main.swift:326` | Removals accumulated across refreshes past the page limit reconcile to a stale-free snapshot. |
| `OrbitDurableRemovalChecks/main.swift:352` | After kill, parent reconciles from journal without a full scan; untouched sibling stays. |
| `OrbitDurableRemovalChecks/main.swift:386` | Journal polls with no new operation do not invalidate the removal epoch. |
| `OrbitDurableRemovalChecks/main.swift:390` | Live barrier does not pass an overlapping operation until its own completion. |
| `OrbitDurableRemovalChecks/main.swift:400` | Retried refresh publishes both removals; the saved folder excludes them. |
| `OrbitDurableRemovalChecks/main.swift:409` | Pending intent that fails is restored in the live map and saved snapshot. |
| `OrbitDurableRemovalChecks/main.swift:453` | Stage-mode removal killed mid-operation reconciles on reopen with no stale deleted row. |
| `OrbitDurableRemovalChecks/main.swift:460` | Recreated file with a new inode appears with its new size; the deleted identity does not. |
| `OrbitDurableRemovalChecks/main.swift:461` | Saved-map removal reconciles durably after kill when no event history remains. |
| `OrbitDurableRemovalChecks/main.swift:461` | Overlapping parent and nested saved maps both reconcile the removal after kill and reopen. |
| `OrbitFailureRecoveryChecks/main.swift:38` | A failed saved-map refresh keeps the committed stage row and resumes it after rescan. |
| `OrbitFailureRecoveryChecks/main.swift:45` | A failure while another root is opened resumes its committed stage later. |
| `OrbitFailureRecoveryChecks/main.swift:49` | A visible mapping failure keeps the committed stage and resumes after a new scan. |
| `OrbitFailureRecoveryChecks/main.swift:52` | A failure during newer folder resolution does not clear the newer opening request. |
| `OrbitFailureRecoveryChecks/main.swift:61` | Old scan failure feedback is not applied to a reentrantly selected root. |
| `OrbitFailureRecoveryChecks/main.swift:75` | A rescan after failure reopens the committed stage row and completes. |
| `OrbitFolderPlacesChecks.swift:21` | Selected place is the deepest containing folder; similar sibling names do not match. |
| `OrbitFolderPlacesChecks.swift:26` | Only unique folders are accepted, excluding files, URLs and built-ins; places survive relaunch. |
| `OrbitFolderPlacesChecks.swift:40` | Removing a place persists the removal and leaves the folder on disk intact. |
| `OrbitFolderTransactionChecks/Colors.swift:12` | A stale preparation keeps newer color assignments and appends its own. |
| `OrbitFolderTransactionChecks/Colors.swift:17` | A no-op publication changes presentation revision but keeps the color revision and assignments. |
| `OrbitFolderTransactionChecks/Colors.swift:34` | A delayed result after clear and reinstall appends to the new color map. |
| `OrbitFolderTransactionChecks/Colors.swift:43` | A delayed result after a presentation-only clear does not overwrite the reinstalled map. |
| `OrbitFolderTransactionChecks/Notifications.swift:47` | Root switch inside the install notification completes; second folder becomes root. |
| `OrbitFolderTransactionChecks/Notifications.swift:47` | Root switch inside the beginPreview transition completes synchronously. |
| `OrbitFolderTransactionChecks/Notifications.swift:47` | Root switch inside the finishInstall transition completes synchronously. |
| `OrbitFolderTransactionChecks/Notifications.swift:53` | A synchronous root switch triggered during initial install completes to the new root. |
| `OrbitFolderTransactionChecks/Notifications.swift:55` | A cancelled initial install does not publish its old folder. |
| `OrbitFolderTransactionChecks/Notifications.swift:59` | A cancelled final handoff does not install its stale folder. |
| `OrbitFolderTransactionChecks/Notifications.swift:66` | A selection change during handoff is retried and the handoff completes. |
| `OrbitFolderTransactionChecks/Route.swift:42` | A removal during saved-folder preparation reconciles before the handoff. |
| `OrbitFolderTransactionChecks/Route.swift:57` | A route changed during saved-folder preparation hands off to the latest route. |
| `OrbitFolderTransactionChecks/Synthetic.swift:18` | Reinstalling the same folder keeps hover and selection; a reentrant list reset is rejected. |
| `OrbitFolderTransactionChecks/Synthetic.swift:23` | Install triggered inside the list-reset callback is rejected; reentrant selection change is kept. |
| `OrbitFolderTransactionChecks/Synthetic.swift:39` | Clearing the synthetic root is rejected when a notification has already switched roots. |
| `OrbitInteractionChecks/main.swift:49` | Grouped coverage equals per-member coverage across Unicode spellings, ancestors and descendants. |
| `OrbitInteractionChecks/main.swift:91` | Exhaustive mask parity of grouped coverage across both fixtures and duplicate members. |
| `OrbitInteractionChecks/main.swift:333` | Click with movement under the drag threshold opens the sector. |
| `OrbitInteractionChecks/main.swift:429` | Double-click opens once; a rejected row drag never opens. |
| `OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:7` | Grouped hover refresh over reorder, identity, canonical bytes, collisions, filter, sort and exit. |
| `OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:72` | Same-membership refresh with reorder, name sort, or metadata change keeps grouped hover. Buffer identity asserts dropped. |
| `OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:84` | Pointer exit after a deferred refresh leaves no hover relations behind. |
| `OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:87` | Colliding sibling identities reject prepared hover and fall back to the baseline behavior. |
| `OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:89` | Replaced member identity, shrunk folder, or changed root clears prepared grouped hover. |
| `OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:91` | Hover moved to another member after a refresh reports the refreshed size and identity. |
| `OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:94` | Nil identity, growth, nesting, indirect parent, and filtered refreshes fall back to exact members. |
| `OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:97` | After a canonical-equal ID refresh, hover members carry the new name bytes. |
| `OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:101` | A late hover callback from an old prepared group does not replace the fresh hover. |
| `OrbitInteractionOwnershipChecks/main.swift:63` | Old chart completion cannot clear a newer row press. |
| `OrbitInteractionOwnershipChecks/main.swift:81` | Chart activation that re-enters a press does not end the newer press. |
| `OrbitInteractionOwnershipChecks/main.swift:97` | Drag setup that re-enters with a newer drag does not start a stale session. |
| `OrbitInteractionOwnershipChecks/main.swift:125` | Reentrant drag start cannot replace its newer owner; newer token survives and drops. |
| `OrbitInteractionOwnershipChecks/main.swift:142` | Old drag completion cannot clear a newer drag; tokenless completion ends nothing. |
| `OrbitInteractionOwnershipChecks/main.swift:153` | A stale drag-session end cannot finish a reused press. |
| `OrbitInteractionOwnershipChecks/main.swift:193` | Drop arriving after source completion collects the right item; consumed tokens reject. |
| `OrbitInteractionOwnershipChecks/main.swift:204` | Old root drag completion cannot clear a new root drag; new root drop succeeds. |
| `OrbitInteractionOwnershipChecks/main.swift:218` | Completion from a stopped session cannot clear a new interaction; repeat completion is idempotent. |
| `OrbitInteractionOwnershipChecks/main.swift:225` | Reentrant first and second publications hold later stages until the gesture ends. |
| `OrbitLargeCollectionChecks.swift:41` | A small drop onto a 1200-item collection is appended with correct totals. |
| `OrbitLargeCollectionChecks.swift:46` | Collection replaced mid-validation; accepted addition survives alongside the replacement items. |
| `OrbitLargeCollectionChecks.swift:51` | Held validation merges with a replaced collection without losing either side. |
| `OrbitLargeCollectionChecks.swift:59` | Pausing mapping keeps the accepted collection job running to completion. |
| `OrbitLargeCollectionChecks.swift:74` | A reentrant scan pause during launch preserves the accepted addition. |
| `OrbitLargeCollectionChecks.swift:99` | Completing validation does not overwrite a status set after the job started. |
| `OrbitLargeCollectionChecks.swift:119` | Old job rejection cannot replace an error published after the job began. |
| `OrbitLargeCollectionChecks.swift:128` | A mapped folder cannot be collected for removal and the rejection is reported. |
| `OrbitLargeCollectionChecks.swift:147` | Validation keeps newer status and error set during its own publication. |
| `OrbitLargeCollectionChecks.swift:169` | Old-root completion publishes no collection, status or error into a new root. |
| `OrbitLargeCollectionChecks.swift:194` | Pausing mapping during completion keeps the collection and the newer scan status. |
| `OrbitLargeCollectionChecks.swift:217` | A reentrant scan pause keeps the active request and the next accepted addition. |
| `OrbitLargeCollectionChecks.swift:234` | Repeated cancellation inside its own notification does not recurse. |
| `OrbitLargeCollectionChecks.swift:250` | Older cancelled validation cannot clear a newer completed one (assert state). |
| `OrbitLargeCollectionChecks.swift:264` | Status set to identical text after the job began is not clobbered by job completion. |
| `OrbitLargeCollectionChecks.swift:272` | Completion preserves a newer status message with identical text. |
| `OrbitLargeCollectionChecks.swift:282` | Cancellation preserves a newer status message with identical text. |
| `OrbitLargeCollectionChecks.swift:305` | Failure cleanup does not publish stale status into a new root. |
| `OrbitLayoutChecks/LayoutChecks.swift:483` | Pending collection removal is visible in flight and applies exactly once (model, not pixels). |
| `OrbitMappingReuseChecks/main.swift:60` | A rejected unfinished-map switchback presentation still observes the mapping worker's completion. |
| `OrbitMappingReuseChecks/main.swift:77` | A rejected saved-map switchback presentation still observes completion of the reopen. |
| `OrbitMappingReuseChecks/main.swift:116` | A cached session restores selection; drag and delete work before saved-index loading finishes. |
| `OrbitMappingReuseChecks/main.swift:145` | Preview rejected by a lifecycle transition still observes the resumed parent through completion. |
| `OrbitPreviewBatchChecks/model.swift:45` | Provisional folders stay collectable; back navigation and held geometry survive the final handoff. |
| `OrbitPreviewBatchChecks/model.swift:66` | Gesture holds map revision and group membership across deferred publication and source completion. |
| `OrbitPreviewBatchChecks/model.swift:104` | The browse route stays stable during fallback; confirmed deletion versus presence is resolved. |
| `OrbitPreviewBatchChecks/model.swift:133` | Vanished provisional folder leaves the listing and the route returns to root. |
| `OrbitPreviewBatchChecks/model.swift:140` | Provisional files remain collectable before the scan completes. |
| `OrbitPreviewBatchChecks/model.swift:150` | Unfinished file entries remain collectable before mapping completes. |
| `OrbitPreviewSearchChecks/model.swift:35` | Deleting a matched file during partial search removes it from results; the query persists. |
| `OrbitPreviewSearchChecks/model.swift:57` | A same-query refresh keeps visible results; later batches add matches progressively. |
| `OrbitPreviewSearchChecks/model.swift:65` | A reentrant query change in a notification keeps prior rows until the new result lands. |
| `OrbitPreviewSearchChecks/model.swift:81` | A root switch inside a search notification shows only the new root's matches. |
| `OrbitPreviewSearchChecks/model.swift:93` | Preview tie order survives saved handoff and refresh; a later query uses indexed order. |
| `OrbitQueuedCollectionChecks/main.swift:74` | Navigation, typed drag and permanent deletion finish before the held inventory writer runs. |
| `OrbitQueuedCollectionChecks/main.swift:99` | Deletion with writer held completes first; later inventory reflects it. Drag uses typed token. |
| `OrbitQueuedCollectionChecks/main.swift:136` | Drops received during validation and deletion apply after the location switch returns. |
| `OrbitQueuedCollectionChecks/main.swift:157` | A large already-included group drop is accepted and deduplicated; status reports the skipped members. |
| `OrbitQueuedCollectionChecks/main.swift:164` | An accepted reservation survives a newer drag and root switch, and is consumed exactly once. |
| `OrbitQueuedCollectionChecks/main.swift:175` | Drop reserved in one root never enters another root collection; restored on return. |
| `OrbitQueuedCollectionChecks/main.swift:182` | Failed provider data releases its reservation; a vanished item is rejected after validation. |
| `OrbitQueuedCollectionChecks/main.swift:192` | Clear cancels delayed provider data and queued additions instead of refilling the collection. |
| `OrbitQueuedCollectionChecks/main.swift:207` | A reentrant root switch during a collect rejects the stale drop. |
| `OrbitQueuedCollectionChecks/main.swift:219` | A small request overtakes a group remainder; clear cancels every remaining group chunk. |
| `OrbitQueuedCollectionChecks/main.swift:240` | A small request is applied before the remaining chunks of a large group drop. |
| `OrbitQueuedCollectionChecks/main.swift:249` | A deleted buffered file never reappears on commit; a later small request overtakes the recheck. |
| `OrbitQueuedCollectionChecks/main.swift:278` | Small request accepted during buffered recheck commits before the buffered group remainder. |
| `OrbitQueuedCollectionChecks/main.swift:286` | Confirmed removal deletes only reviewed items; an unreviewed later addition survives. |
| `OrbitQueuedCollectionChecks/main.swift:317` | A cancelled confirmation during held publication resumes without losing its collection. |
| `OrbitQueuedCollectionChecks/main.swift:331` | A stale confirmation lease cannot release a newer confirmation after a session switch. |
| `OrbitQueuedCollectionChecks/main.swift:347` | Releasing an old confirmation lease does not release a newer confirmation on the same owner. |
| `OrbitQueuedCollectionChecks/main.swift:354` | Clear during a held confirmation empties the collection and dismisses the review. |
| `OrbitQueuedCollectionChecks/main.swift:364` | Clearing during an active confirmation empties the collection and closes the review. |
| `OrbitRemovalFinishChecks/main.swift:16` | Removal A completes after switching to B; B error and status survive; A not restored. |
| `OrbitRemovalFinishExtraChecks/main.swift:24` | A replacement during overlay rebuild preserves and finishes the removal. |
| `OrbitRemovalFinishExtraChecks/main.swift:38` | Invalidating the displayed root during completion preserves later feedback. |
| `OrbitRemovalFinishExtraChecks/main.swift:52` | A feedback notification switch keeps B's feedback and update pause state. |
| `OrbitRemovalFinishExtraChecks/main.swift:68` | A monitor failure during completion preserves B's feedback. |
| `OrbitRemovalFinishExtraChecks/main.swift:85` | A monitor failure respects root invalidation and a newer pause state. |
| `OrbitRemovalFinishExtraChecks/main.swift:102` | Newer feedback and pause state set during completion commit are preserved. |
| `OrbitRemovalFinishExtraChecks/main.swift:117` | A nested removal stays busy across a root switch and clears on completion. |
| `OrbitRemovalStartChecks/main.swift:31` | Root invalidation at removal start keeps the owning root's removal and the new feedback. |
| `OrbitRemovalStartChecks/main.swift:32` | A forced same-root activation during the published removal keeps owner and feedback. |
| `OrbitRemovalStartChecks/main.swift:35` | A forced activation followed by a switch during the published removal keeps owner and feedback. |
| `OrbitRemovalStartChecks/main.swift:40` | A switch inside the removal activity notification keeps the owner and deletes the right file. |
| `OrbitRemovalStartChecks/main.swift:42` | A switch from the published removal notification restores the correct owner and a single deletion. |
| `OrbitRemovalStartChecks/main.swift:44` | Same-root feedback during removal start is kept and no duplicate removal starts. |
| `OrbitResumableChecks/main.swift:66` | Process killed mid-stage with a saved map first resumes with collection and selection intact. |
| `OrbitResumableChecks/main.swift:70` | Process killed mid-stage with no saved map resumes the partial map with collection intact. |
| `OrbitResumableChecks/main.swift:92` | Published stage plus later refresh reopens the updated snapshot; explicit rescan keeps it. |
| `OrbitResumableChecks/main.swift:112` | Deleting the cache directory mid-stage still resumes the committed partial map to completion. |
| `OrbitResumableChecks/main.swift:282` | Mapping excludes its own index and durable stage without starting a refresh loop. |
| `OrbitSavedSearchChecks/model.swift:33` | Saved search hides deletions with an accurate count while the refresh is gated. |
| `OrbitSavedSearchChecks/model.swift:49` | A switch from a removal notification restores the removed session and reconciles it. |
| `OrbitSearchInterruptionChecks.swift:21` | A stopped preview search resumes after its pending query is replaced; the map is not outdated. |
| `OrbitSearchInterruptionChecks.swift:35` | A failed same-query search with empty prior results remains marked outdated. |
| `OrbitSearchLifetimeChecks.swift:13` | Gated search worker finishes after model deinit; no late publication occurs. |
| `OrbitSearchLifetimeChecks.swift:37` | A dropped model receives no late search publication when its worker finishes. |
| `OrbitSessionChecks/main.swift:40` | Map child ID index must match the current folder revision across navigation and filter changes. |
| `OrbitSessionChecks/main.swift:66` | Saved share never implies discovery finished; incompatible counts must not show 100 percent. |
| `OrbitSessionChecks/main.swift:145` | Reattached progress reads finished offscreen memory without waiting for a new discovery event. |
| `OrbitSessionChecks/main.swift:188` | Completed discovery stays visible while the cache writer is held. Hooks make the race reproducible. |
| `OrbitSessionChecks/main.swift:244` | Cancelled or failed mapping releases its model and durable stage lease so the stage can reopen. |
| `OrbitSessionChecks/main.swift:293` | Late progress delivery must not replace a newer stage revision. |
| `OrbitSessionChecks/main.swift:300` | A resumed stage accepts its own revision sequence after retaining legacy counters. |
| `OrbitSessionChecks/main.swift:321` | Navigating into a partial sub-folder keeps root-wide discovery and writes no sub-folder snapshot. |
| `OrbitSessionChecks/main.swift:329` | Child-ID membership arrays stay aligned with visible children across query and sort changes. |
| `OrbitSessionChecks/main.swift:337` | Reattaching a partial map restores its collection and retains its discovery progress. |
| `OrbitSessionChecks/main.swift:344` | Detached partial map finishes saving in background and its preview is released. |
| `OrbitSessionChecks/main.swift:354` | Retained old map is not new inventory; hidden discovery must not replace it before publication. |
| `OrbitSessionChecks/main.swift:358` | Forced refresh does not count the retained map as new inventory; discovery hidden until publication. |
| `OrbitSessionChecks/main.swift:366` | Switching away and back during refresh restores the still-refreshing map. |
| `OrbitSessionChecks/main.swift:375` | Refresh keeps completed memory, withholds the incompatible saved count, and needs Refresh when paused. |
| `OrbitSessionChecks/main.swift:378` | Leaving and returning to an updated map restores the same saved snapshot id and size. |
| `OrbitSessionChecks/main.swift:382` | Gated refresh withholds cached counts and shows completed memory without a fresh estimate. |
| `OrbitSessionChecks/main.swift:395` | Returning mid-refresh keeps the completed memory bar and the updating phase. |
| `OrbitSessionChecks/main.swift:409` | Replacement folder at a mapped path is scanned fresh (assert behavior, not caches). |
| `OrbitSessionChecks/main.swift:422` | Stopped mapping keeps its count, publishes nothing, and Resume continues its saved stage. |
| `OrbitSessionChecks/main.swift:429` | Stopped mapping keeps counts; Resume continues the saved stage without a new save. |
| `OrbitSessionChecks/main.swift:462` | Symlinked directory is not reachable from its host and scans independently. |
| `OrbitSessionChecks/main.swift:471` | A stale path resolution cannot switch the displayed root after cancellation. |
| `OrbitSessionChecks/main.swift:477` | Held deletion keeps updating phase; no premature empty completion before counts install. |
| `OrbitSessionChecks/main.swift:492` | Held deletion shows updating phase rather than the old cache as fully saved. |
| `OrbitSessionChecks/main.swift:495` | Switching location during a held deletion keeps the removal owned across sessions. |
| `OrbitSessionChecks/main.swift:498` | Returning to a held deletion restores its removing state and updating phase. |
| `OrbitSessionChecks/main.swift:504` | Detached removal finishes; refreshed empty map saves with zero counts and full share. |
| `OrbitSessionChecks/main.swift:511` | Removals from two sessions both complete; only the active root reports removing. |
| `OrbitSessionChecks/main.swift:522` | Removals started in two locations complete independently while both sessions stay busy. |
| `OrbitSessionChecks/main.swift:543` | Scan save completion keeps the Deleting status while a removal is held. |
| `OrbitSessionChecks/main.swift:555` | Pending children deleted under serial and async discovery; a drag from another root is rejected. |
| `OrbitSessionChecks/main.swift:568` | Async discovery completes a child folder while the cache writer remains held. |
| `OrbitSessionChecks/main.swift:576` | A discovered file accepts a collection drop while mapping is held. |
| `OrbitSessionChecks/main.swift:582` | Drag payload from a held partial map is rejected after switching to a different root. |
| `OrbitSessionChecks/main.swift:589` | A folder discovered before switching away accepts a drop after returning to the partial map. |
| `OrbitSessionChecks/main.swift:603` | A visible saved-map item stays collectable and deletable while its session opens. |
| `OrbitSessionChecks/main.swift:612` | Visible saved-map items stay collectable and draggable while the session is still opening. |
| `OrbitSessionChecks/main.swift:671` | Old stage row is not shown while held before dirty-parent replay; only fresh replacements appear. |
| `OrbitSessionChecks/main.swift:681` | Stage replay after restart publishes the deletion or the recreated file new identity. |
| `OrbitSessionChecks/main.swift:692` | Map revision stays unchanged during interaction; the overlay applies only after interaction ends. |
| `OrbitSessionChecks/main.swift:708` | Deleting a pending entry during map interaction keeps the held revision; overlay applies after release. |
| `OrbitSessionChecks/main.swift:739` | Deleting the open folder redirects to its surviving parent while the scan is still running. |
| `OrbitSessionChecks/main.swift:752` | Deleting the folder being browsed redirects to its parent while the scan continues. |
| `OrbitSessionChecks/main.swift:787` | Moved item survives a partial failure; error shown; collection keeps only the survivor. |
| `OrbitSessionChecks/main.swift:809` | Replacing the active root directory is reflected as a new map on rescan. |
| `OrbitSessionChecks/main.swift:829` | Saved removal projects immediately; reattaching keeps the overlay until the refresh completes. |
| `OrbitSessionChecks/main.swift:837` | Removal projected onto a saved map persists through a detached reattach until reconciled. |
| `OrbitSessionChecks/main.swift:863` | Detached cross-session removal completes; blocking work stays off the cooperative executor. |
| `OrbitSessionChecks/main.swift:885` | An item from a former root is rejected after a synchronous preview transition. |
| `OrbitSessionChecks/main.swift:898` | Item from a former root is rejected after a forced transition; collection holds only new root item. |
| `OrbitSessionChecks/main.swift:913` | Stopped map shows the removal overlay and restarts to an empty reconciled map. |
| `OrbitSessionChecks/main.swift:919` | Missing location reports an error and opens nothing. |
| `OrbitSessionChecks/main.swift:961` | Removals during a held full publication reconcile to an empty saved folder. |
| `OrbitSessionChecks/main.swift:984` | Collection validation belongs to its root while another root is visible; switching restores it. |
| `OrbitSessionChecks/main.swift:1000` | Pending collection validation stays with its own root and is not applied to another root. |
| `OrbitSessionChecks/main.swift:1007` | A refresh replacement revalidates the accepted collection before completing. |
| `OrbitSessionChecks/main.swift:1014` | Cancelling validation discards queued additions instead of applying them. |
| `OrbitSessionChecks/main.swift:1020` | Pausing the map does not cancel an already accepted collection operation. |
| `OrbitSessionChecks/main.swift:1025` | Subscribers observe the committed collection after the background identity check. |
| `OrbitSessionChecks/main.swift:1035` | Root moved during the pre-publication notification rejects the collection without publishing. |
| `OrbitSessionChecks/main.swift:1055` | A distinct background rejection is not hidden by a newer status message. |
| `OrbitSessionChecks/main.swift:1072` | Background rejection stays visible after a newer status and returns with the map. |
| `OrbitUpdaterChecks.swift:50` | Queued restart waits for removal, is cancelled by a new removal, and runs exactly once. |
| `OrbitUpdaterChecks.swift:66` | No postponement when no removal is active; aborted update clears the waiting state. |
| `OrbitUpdaterChecks.swift:75` | Removal started from inside the deferred-restart notification keeps restart held until it ends. |
| `OrbitVolumeChecks/main.swift:43` | The Mac fallback volume stays visible while discovery is blocked. |
| `OrbitVolumeChecks/main.swift:48` | Repeated refreshes coalesce to one discovery worker. Timing assertion dropped; call count kept. |
| `OrbitVolumeChecks/main.swift:62` | The actual root volume replaces the Mac fallback when discovery finishes. |
| `OrbitVolumeChecks/main.swift:67` | An empty discovery result falls back to the root volume. |
| `OrbitVolumeChecks/main.swift:73` | A refresh requested during finish restarts discovery exactly once. |
| `OrbitVolumeChecks/main.swift:81` | The model deallocates while blocked discovery is in flight; no retain cycle. |

## Keep → `orbit-automation`

| Source | Behavior to cover |
|---|---|
| `OrbitAutomationChecks/main.swift:71` | Timeout and disconnect report outcomeUnknown; peer mismatch does not; query schema limit enforced. |
| `OrbitAutomationChecks/main.swift:77` | Query of 4096 accented characters accepted; 4097 rejected by schema validation. |
| `OrbitAutomationChecks/main.swift:104` | Invalid arguments rejected; identity-checked selection; list cursors are immutable and go stale. |
| `OrbitAutomationChecks/main.swift:114` | List paging returns stable order; cursor goes stale once the search query changes. |
| `OrbitAutomationChecks/main.swift:120` | Selection with a wrong identity rejected as stale; current identity selects the item. |
| `OrbitAutomationChecks/main.swift:128` | Context is null while a map opens; stale context is rejected after switching maps. |
| `OrbitAutomationChecks/main.swift:136` | Preview of a non-file is rejected and leaves the current selection unchanged. |
| `OrbitAutomationChecks/main.swift:148` | Replaced file identity and changed collection or map invalidate prepared removal plans. |
| `OrbitAutomationChecks/main.swift:160` | Collection page omits search counts that belong to a different query. |
| `OrbitAutomationChecks/main.swift:167` | Removal plans invalidated by collection edits or map switches; no files removed. |
| `OrbitAutomationChecks/main.swift:181` | Confirmed plan removes only its temporary item and cannot be replayed. |
| `OrbitAutomationChecks/main.swift:262` | Failed search reports search_failed; the same query recovers after the index returns. |
| `OrbitAutomationTransportChecks/IntegrationInstallerChecks.swift:65` | Codex MCP add runs once; equivalent symlink accepted; conflicting server config preserved. |
| `OrbitAutomationTransportChecks/IntegrationInstallerChecks.swift:85` | Empty, invalid or oversized command output fails and never triggers a config write. |
| `OrbitAutomationTransportChecks/main.swift:75` | Request with no listening endpoint reports unavailable rather than hanging or succeeding. |
| `OrbitAutomationTransportChecks/main.swift:78` | Byte-exact roundtrip of JSON and Unicode paths, empty frame, 1 MiB frame, client size limit. |
| `OrbitAutomationTransportChecks/main.swift:90` | Second server reports alreadyRunning; live endpoint preserved; repeat start is idempotent. |
| `OrbitAutomationTransportChecks/main.swift:97` | Fragmented frames reassemble; oversized length prefix closes connection before allocation. |
| `OrbitAutomationTransportChecks/main.swift:118` | Stop removes owned socket; stale socket recovers; a live foreign listener is never unlinked. |
| `OrbitAutomationTransportChecks/main.swift:135` | Symlinked socket, lock or directory and loose directory mode are refused; targets untouched. |
| `OrbitAutomationTransportChecks/main.swift:159` | Stop does not wait for a pending handler and never responds; wall-clock bound, inject clock. |
| `OrbitAutomationTransportChecks/main.swift:180` | Ninth concurrent connection is rejected; sleeps used for accept timing, inject clock. |
| `OrbitAutomationTransportChecks/main.swift:191` | Absolute frame read deadline holds under trickled input; real sleeps, inject clock. |
| `test_mcp.py:32` | Tool names, schemas, destructive annotation and confirm enum checked without app connection; instruction copy ignored. |
| `test_mcp.py:47` | Requests before initialize are rejected; notifications get no reply; unknown method errors. |
| `test_mcp.py:54` | Invalid initialize params rejected; server recovers and accepts later initialize. |
| `test_mcp.py:64` | Supported protocol versions echo; unknown version falls back to the latest supported. |
| `test_mcp.py:69` | Non-JSON and non-object frames get parse or invalid-request errors; following ping still answers. |
| `test_mcp.py:75` | Fractional, boolean, nested and wrong-version ids and params are rejected with the right codes. |
| `test_mcp.py:81` | A frame one byte over 1 MiB is rejected and the next message still succeeds. |
| `test_mcp.py:87` | Schema violations return invalid_arguments before any app connection; structured and text payloads match. |
| `test_mcp.py:113` | Unknown tool name and malformed tools/list cursor return invalid params. |
| `test_mcp.py:118` | Task-augmented tool calls are rejected as unsupported without connecting to the app. |
| `test_mcp.py:123` | Setup commands with extra args and malformed JSON arguments exit 2 with invalid_arguments JSON. |
| `test_mcp.py:133` | Bundle version is reported by --version and initialize without launching the app. |
| `test_mcp.py:145` | Metadata lists protocol versions; mcp-config is correct; unknown command returns unknown_command. |

## Keep → `orbit-core`

| Source | Behavior to cover |
|---|---|
| `OrbitAppChecks.swift:233` | Refresh planning coalesces true descendants, never follows replaced ancestors, honors cancellation and stale snapshots. |
| `OrbitAppChecks.swift:258` | Only true descendant refresh paths coalesce; missing paths refresh nearest indexed folder. |
| `OrbitAppChecks.swift:263` | Ancestor replaced by symlink is refreshed as a folder and the link is never followed. |
| `OrbitAppChecks.swift:271` | Cancellation observed during refresh planning and the planner stops with CancellationError. |
| `OrbitAppChecks.swift:277` | Refresh planned against a superseded index snapshot revision is rejected as stale. |
| `OrbitAppChecks.swift:1319` | Cached map is usable before stage reopen completes; relaunch reuses the saved index. |
| `OrbitAppChecks.swift:1338` | Offline changes replay on relaunch; full refresh keeps the map and navigation made during scan. |
| `OrbitAsyncMappingChecks/BacklogDiscoveryChecks.swift:8` | Retiring a drained partial page does not change earlier reader snapshots or offsets. |
| `OrbitAsyncMappingChecks/BacklogDiscoveryChecks.swift:65` | Discovery completes and supports browse, search and removal while the writer is gated. |
| `OrbitAsyncMappingChecks/BacklogDiscoveryChecks.swift:97` | Discovery beyond former frontier ceiling; queue draining preserves snapshots and totals. |
| `OrbitAsyncMappingChecks/BacklogDiscoveryChecks.swift:144` | Cancellation clears a large pending backlog and rejects retained stale inventory snapshots. |
| `OrbitAsyncMappingChecks/BacklogDiscoveryChecks.swift:195` | Listing beyond former entry ceiling finishes discovery before any durable write. |
| `OrbitAsyncMappingChecks/BoundedChildrenChecks.swift:9` | Bounded child previews match full listing ordering, totals, hard-link owners and cancellation. |
| `OrbitAsyncMappingChecks/DiscoveryHotPathChecks.swift:61` | Legacy cache-key names are still found and removed correctly. |
| `OrbitAsyncMappingChecks/FreshLeafGroupChecks.swift:8` | Leaf-group peek respects parent, record and byte bounds and skips unsuitable entries. |
| `OrbitAsyncMappingChecks/FreshLeafGroupChecks.swift:42` | Grouped commit keeps watermark, live reads and hard-link settlement consistent with durable state. |
| `OrbitAsyncMappingChecks/FreshLeafGroupChecks.swift:96` | Cancelling a leaf group rolls back the whole transaction and resumes pending parents. |
| `OrbitAsyncMappingChecks/FreshLeafGroupChecks.swift:121` | Journaled removal during uncommitted group is not resurrected by the group commit. |
| `OrbitAsyncMappingChecks/LiveInventoryChecks.swift:8` | Live cache invalidates on partial append, restart, identity replacement, denial and removal. |
| `OrbitAsyncMappingChecks/LiveInventoryChecks.swift:128` | Live browsing, search and removal projection never open the database. |
| `OrbitAsyncMappingChecks/LiveInventoryChecks.swift:159` | Full refresh marking stays bounded and does not re-enumerate directories serially. |
| `OrbitAsyncMappingChecks/LiveInventoryChecks.swift:192` | Four workers reuse shared subtree; busy journal fence falls back without waiting. Barrier, not timer. |
| `OrbitAsyncMappingChecks/LiveInventoryChecks.swift:261` | Resume reuses completed listings, re-enumerates dirty directories, rejects foreign seed identity. |
| `OrbitAsyncMappingChecks/MappingProgressChecks.swift:19` | Exact durable cache counts, unique revisions, count-only preview under removals, publication phases. |
| `OrbitAsyncMappingChecks/MappingProgressChecks.swift:81` | Save-work accounting is idempotent on acknowledgement and withdrawn on cancellation. |
| `OrbitAsyncMappingChecks/MappingProgressChecks.swift:106` | Memory completion does not wait for journal catch-up; final actual counts replace estimate. Inject clock. |
| `OrbitAsyncMappingChecks/MappingProgressChecks.swift:149` | Shared-listing import is not reported as new enumeration; completion never regresses. |
| `OrbitAsyncMappingChecks/MappingProgressChecks.swift:183` | A finished pass with deferred retry files is not reported as complete until reconciled. |
| `OrbitAsyncMappingChecks/MetadataBookkeepingChecks.swift:13` | Fused metadata totals, hard-link owner lists, and backlog byte estimates per batch. |
| `OrbitAsyncMappingChecks/MetadataBookkeepingChecks.swift:56` | Exclusion ordering preserved; rejected duplicate batch rolls back queued and deferred directories. |
| `OrbitAsyncMappingChecks/ParallelDiscoveryChecks.swift:14` | Gated worker race: live siblings publish while one folder stalls; durable listings keep ticket order. |
| `OrbitAsyncMappingChecks/ParallelDiscoveryChecks.swift:124` | Cancelled workers publish no live changes or durable batches and claim no extra directories. |
| `OrbitAsyncMappingChecks/SecureCursorChecks.swift:96` | Scanner fallback fails identity validation when the root is swapped mid-traversal. |
| `OrbitAsyncMappingChecks/main.swift:60` | Bulk vs fallback discovery produce identical trees (assert trees, not mode). |
| `OrbitAsyncMappingChecks/main.swift:78` | Memory navigation, exact size-ranked search, identity-aware removal projection and boundary totals. |
| `OrbitAsyncMappingChecks/main.swift:127` | Journal replay removes queued deleted inode but keeps replacement inode after reconciliation. |
| `OrbitAsyncMappingChecks/main.swift:176` | Committed watermark never exposes totals before deferred hard-link accounting settles. |
| `OrbitAsyncMappingChecks/main.swift:198` | Stale queued children are rejected after their parent directory identity changes. |
| `OrbitAsyncMappingChecks/main.swift:214` | Writer revalidates cached ancestors before accepting descendant metadata during replacement. |
| `OrbitAsyncMappingChecks/main.swift:239` | Pinned snapshots keep retired pending rows; newer snapshots do not double-count; cancelled reads rejected. |
| `OrbitAsyncMappingChecks/main.swift:272` | Denied or missing first directory does not stop unrelated discovery or drop RAM progress. |
| `OrbitAsyncMappingChecks/main.swift:307` | Within-batch and cross-batch duplicate names rejected before exposure, then repaired serially. |
| `OrbitAsyncMappingChecks/main.swift:358` | SIGKILL at each async boundary resumes pending directory work without loss. |
| `OrbitAutomationChecks/main.swift:196` | Native identity validation refuses removal of a file replaced after plan preparation. |
| `OrbitBootVolumeChecks/main.swift:112` | Unique Data-volume entries counted once in live view, count-only inventory and durable totals. |
| `OrbitCacheResetChecks.swift:22` | An unrequested reset is a no-op and leaves caches, preferences and sibling files intact. |
| `OrbitCacheResetChecks.swift:31` | A requested reset removes both cache trees, preserves preferences, and does not repeat. |
| `OrbitCacheResetChecks.swift:42` | Missing cache folders are accepted without being created or touching unrelated folders. |
| `OrbitCacheResetChecks.swift:79` | A regular file at the mapping path is rejected and the first cache target stays untouched. |
| `OrbitCloudScanningChecks/main.swift:30` | Cloud roots and sibling prefixes excluded from bulk and fallback scans; local totals unchanged. |
| `OrbitCloudScanningChecks/main.swift:41` | Sync and async mapping persist no cloud entries and keep cloud contents out of search. |
| `OrbitCloudScanningChecks/main.swift:56` | Pre-exclusion schema saved map is rejected and replaced by a fresh stage. |
| `OrbitCoreDiskIndexStoreTests.swift:20` | Overlapping replacement paths rejected including NFC/NFD variants; cancellation honored; snapshot kept. |
| `OrbitCoreDiskIndexStoreTests.swift:47` | Saved index round-trips metadata; folder queries are lazy and bounded; cancellation honored. |
| `OrbitCoreDiskIndexStoreTests.swift:86` | Compact schema v5, identity bits, Unicode paths and exact 2^53+ sizes round-trip; update removes subtrees. |
| `OrbitCoreDiskIndexStoreTests.swift:146` | NFC and NFD filesystem names look up and refresh by identity without duplicates. |
| `OrbitCoreDiskIndexStoreTests.swift:200` | Mismatched parent or sibling-prefix children rejected atomically; NFD parents resolve. |
| `OrbitCoreDiskIndexStoreTests.swift:226` | Search treats %, _ literally with case and Unicode folding; ancestors ordered; cancellation honored. |
| `OrbitCoreDiskIndexStoreTests.swift:248` | Nested directories reopen with correct parents across many empty siblings. |
| `OrbitCoreDiskIndexStoreTests.swift:261` | Cancelled, invalid or partial saves keep prior snapshot; stale and old-schema caches rejected. Mid-save timer: inject cancel probe. |
| `OrbitCoreDiskIndexStoreTests.swift:314` | Incremental create, rename, delete and folder totals match expected; prefix-sibling untouched. |
| `OrbitCoreDiskIndexStoreTests.swift:343` | Hard link across subtrees moves allocation ownership correctly on add and remove. |
| `OrbitCoreDiskIndexStoreTests.swift:361` | Hard-link changes recalculate allocation across nested ancestors on add and remove. |
| `OrbitCoreDiskIndexStoreTests.swift:381` | Incremental mixed hard-link batch matches a full rescan on every node. |
| `OrbitCoreDiskIndexStoreTests.swift:440` | Cancelled incremental update keeps the prior snapshot intact. |
| `OrbitCoreDiskScanPreviewTests.swift:6` | Root and pending folders publish before descent; returned preview views remain immutable. |
| `OrbitCoreDiskScanPreviewTests.swift:47` | Preview folder totals, identities and hard-link owners match final bulk and fallback scans. |
| `OrbitCoreDiskScanPreviewTests.swift:97` | Hydration bounded to 2000 descendants while direct children and ancestors remain complete. |
| `OrbitCoreDiskScanPreviewTests.swift:124` | Unreadable folder reported as inaccessible, distinct from complete empty folder. |
| `OrbitCoreDiskScanPreviewTests.swift:144` | Size ordering and cached empty child listing invalidate when measured sizes change. |
| `OrbitCoreDiskScannerTests.swift:23` | Hierarchy sizes, hidden and empty folders; replacement folder gets a new identity. |
| `OrbitCoreDiskScannerTests.swift:50` | Scanner never follows symlink loops or broken links; links count as leaves. |
| `OrbitCoreDiskScannerTests.swift:65` | Hard links keep logical size per file but charge allocation to a single owner. |
| `OrbitCoreDiskScannerTests.swift:89` | Sparse file reports logical size separately from smaller allocated size. |
| `OrbitCoreDiskScannerTests.swift:151` | Scan honors cancellation both before start and during progress callbacks. |
| `OrbitCoreDiskScannerTests.swift:164` | Non-URL, missing, file and symlink scan roots are rejected. |
| `OrbitCoreDiskScannerTests.swift:173` | Unreadable child is reported as inaccessible; unreadable root throws. Skips under root. |
| `OrbitCoreDiskScannerTests.swift:187` | Final progress totals equal the scanned result; drop the throttle-count assertion. |
| `OrbitCoreTrashPolicyTests.swift:23` | Ordinary descendants removable; scan root, home and sibling-prefix paths protected. |
| `OrbitCoreTrashPolicyTests.swift:33` | Symlink entries and ancestor symlink escapes are rejected by trash policy. |
| `OrbitCoreTrashPolicyTests.swift:46` | Hidden sensitive folders, trash folders and Library protected; ordinary dotfiles allowed. |
| `OrbitCoreTrashPolicyTests.swift:61` | System paths, volume roots, missing items and non-file URLs rejected for removal. |
| `OrbitCoreTrashPolicyTests.swift:69` | Cloud roots, resolved aliases and case-variant paths are cloud-excluded from removal. |
| `OrbitCoreTrashPolicyTests.swift:95` | Cloud exclusion roots come from known folders; unreadable metadata adds no exclusion. Drop probe-order checks. |
| `OrbitCoreTrashPolicyTests.swift:116` | Dataless cloud errors excluded; ordinary access-denied errors not mislabeled as cloud. |
| `OrbitCoreTrashPolicyTests.swift:139` | Each item rechecked within a batch and new batches never inherit ancestor classification. Drop probe counts. |
| `OrbitDiscoveryCacheChecks/main.swift:84` | Idle cache readers do not pin WAL checkpoints; readers observe each new committed revision. |
| `OrbitDiscoveryCacheChecks/main.swift:106` | Hook-driven concurrent write cannot tear an in-flight listing; next lookup sees new snapshot. |
| `OrbitDiscoveryCacheChecks/main.swift:129` | Missing, replaced, unfinished, malformed or missing-table cache listings are rejected and release snapshots. |
| `OrbitDiscoveryCacheChecks/main.swift:159` | Mid-listing cancellation and wrong root identity release read transactions; cancelled reader stays cancelled. |
| `OrbitDurableRemovalChecks/main.swift:98` | A published cursor cannot retire a newer deletion; unaffected file sizes stay available. |
| `OrbitDurableRemovalChecks/main.swift:216` | A replaced removal journal does not expose a stale row after the map is rebuilt. |
| `OrbitDurableRemovalChecks/main.swift:235` | Cancel during copy or rename publication keeps the old snapshot; retry reconciles both removals. |
| `OrbitDurableRemovalChecks/main.swift:300` | Cumulative overflow across journal entries is withheld; the final saved snapshot is reconciled. |
| `OrbitDurableRemovalChecks/main.swift:424` | SIGKILL during staged removal reopens and reconciles without event history. |
| `OrbitGenerationChecks/main.swift:19` | Cache purge during completed scan republishes the retained stage with its children. |
| `OrbitGenerationChecks/main.swift:28` | Interrupted scan resumes to a complete published map after cache deletion. |
| `OrbitGenerationChecks/main.swift:38` | Invalidation makes an active stale publisher fail and leaves no saved snapshot. |
| `OrbitGenerationChecks/main.swift:47` | Invalidation blocks behind root lease during publication via gated hook, not a timer. |
| `OrbitGenerationChecks/main.swift:64` | Durable invalidation marker survives crash and prevents a stale stage from publishing. |
| `OrbitGenerationChecks/main.swift:73` | Published stage restores a purged saved index with the same snapshot id, no remap. |
| `OrbitGenerationChecks/main.swift:84` | Invalidated stage cannot resume after purge; retire then fresh mapping rebuilds correctly. |
| `OrbitGenerationChecks/main.swift:96` | Invalidation marker survives interruption and cache deletion. |
| `OrbitLockChecks/main.swift:11` | Symlink and firmlink aliases resolve to one lock before and after cache creation; symlink cache rejected. |
| `OrbitLockChecks/main.swift:32` | Cross-process lease blocks the stage until released; uses a 350ms sleep, replace with a deterministic gate. |
| `OrbitMappingCoverageChecks/main.swift:35` | Pending, stopped and exact-stage coverage is found without acquiring the writer lease. |
| `OrbitMappingCoverageChecks/main.swift:39` | Ancestor stage covers pending child; exact stage wins; invalidated exact stage falls back. |
| `OrbitMappingCoverageChecks/main.swift:88` | A published stage restores its known directory after cache purge and rejects unindexed directories. |
| `OrbitMappingCoverageChecks/main.swift:106` | Symlink inside the root pointing outside is not treated as reachable. |
| `OrbitMappingCoverageChecks/main.swift:110` | Mismatched root or target identity makes the target unreachable. |
| `OrbitMappingCoverageChecks/main.swift:114` | Coverage discovery with a cancelled token throws CancellationError. |
| `OrbitMappingCoverageChecks/main.swift:121` | A symlinked or empty stage database is rejected during discovery. |
| `OrbitMappingCoverageChecks/main.swift:128` | Empty placeholder stage database is not accepted as coverage. |
| `OrbitMappingReuseChecks/main.swift:175` | Relaunched child routes reuse the parent stage, then its saved map, without a child scan. |
| `OrbitMonitorChecks.swift:15` | Combined FSEvents overflow flags still invalidate the index. |
| `OrbitMonitorChecks.swift:16` | Excluded descendants match; siblings, dot-dot, NUL and device-root edge cases handled. |
| `OrbitMonitorChecks.swift:57` | Deleted descendants map across alias, canonical, device-relative and removed-ancestor paths; siblings rejected. |
| `OrbitMonitorChecks.swift:73` | Dot-dot traversal and symlink escapes are rejected in path and canonical normalization. |
| `OrbitMonitorChecks.swift:98` | Sibling events do not force full refresh; ambiguous symlink descendants refresh their ancestor. |
| `OrbitPortableCoreChecks/CompactNameChecks.swift:3` | Compact name index byte parity across blocks, candidate limits, byte budget and in-flight cancellation. |
| `OrbitPortableCoreChecks/MappingProgressChecks.swift:3` | Progress never claims completion when cancelled, uncertain or retrying; cache denominator stays honest. Drop exact fractions. |
| `OrbitPortableCoreChecks/main.swift:45` | Checkpoint JSON round-trips full-width event IDs; raw hard-link allocation preserved on node. |
| `OrbitPortableCoreChecks/main.swift:66` | Removal matches identity, subtracts ancestor totals once, ignores replacements and sibling prefixes. |
| `OrbitPortableCoreChecks/main.swift:101` | Byte-exact Unicode removal projection; inconsistent totals rejected; root removal covers descendants. |
| `OrbitPortableCoreChecks/main.swift:126` | Depth-first hard-link path ordering with byte tie-break is a strict order; path composition exact. |
| `OrbitPortableCoreChecks/main.swift:151` | Collection coverage respects component boundaries, Unicode spellings and optimized group coverage parity. |
| `OrbitPortableCoreChecks/main.swift:187` | Cancelled commit body never runs; commit lock recovers after throw; projections honor cancellation. |
| `OrbitPreparedSearchChecks/main.swift:30` | Prepared saved search matches reference across Unicode, literal queries, limits, deletion overlays and capacity cap. |
| `OrbitPreparedSearchChecks/main.swift:76` | Staged prepared search matches reference across removals, renames, monotonic tails and damaged rows; NFC/NFD literal lookup. |
| `OrbitPreviewBatchChecks/core.swift:25` | Unfinished listing is marked provisional, children are not collectable, and it cannot confirm missing folders. |
| `OrbitPreviewBatchChecks/core.swift:32` | Fallback keeps the provisional listing; a deleted staged file is removed on rollback. |
| `OrbitPreviewBatchChecks/core.swift:43` | Fallback totals, per-node identity, symlink flags, and hardlink ownership match the native scanner. |
| `OrbitPreviewBatchChecks/core.swift:62` | A failed nested fallback clears provisional children before completing an inaccessible folder. |
| `OrbitPreviewBatchChecks/core.swift:77` | Failed nested listing clears provisional children, marks folder inaccessible, confirms stale child missing. |
| `OrbitPreviewCancellationChecks/main.swift:19` | A cancelled preview read throws CancellationError instead of returning a view. |
| `OrbitPreviewCancellationChecks/main.swift:36` | Cancelling the detached reader task stops its in-flight preview read. |
| `OrbitPreviewCancellationChecks/main.swift:61` | A cancelled read releases the preview lock so the waiting scanner write proceeds. Inject clock: latency is printed only. |
| `OrbitPreviewCancellationChecks/main.swift:91` | Hydration lock holds the scanner writer; cancelling the read releases reader and writer. |
| `OrbitPreviewCancellationChecks/main.swift:93` | A cancelled first-read sort is not cached; the later read returns the complete order and revision. |
| `OrbitPreviewCancellationChecks/main.swift:98` | A cancelled hydration of a cached order preserves the complete cached order. |
| `OrbitPreviewSearchChecks/core.swift:21` | Preview search ranks matches by size during a scan and after completion. |
| `OrbitPreviewSearchChecks/core.swift:27` | Removed subtrees and projected folder counts are excluded from preview search results. |
| `OrbitPreviewSearchChecks/core.swift:32` | A search limit caps returned nodes while keeping the total match count. |
| `OrbitPreviewSearchChecks/core.swift:35` | Replacing a tombstoned path discards the projection and retries with the new identity. |
| `OrbitPreviewSearchChecks/core.swift:49` | Partial preview and saved index return identical matches for composed, decomposed and kana queries. |
| `OrbitPreviewSearchChecks/core.swift:60` | Partial and saved search share byte-identical tie ordering and the 300-result cutoff. |
| `OrbitPreviewSearchChecks/core.swift:75` | A cancelled preview search throws CancellationError instead of returning results. |
| `OrbitPreviewSearchChecks/core.swift:76` | Cancelled preview search throws CancellationError. |
| `OrbitPreviewSearchChecks/model.swift:105` | Partial results keep their tie order through saved handoff and a same-query refresh. |
| `OrbitPreviewSearchChecks/model.swift:110` | A new query after handoff uses saved indexed ordering rather than preview order. |
| `OrbitPublicationCrashChecks/main.swift:36` | Crash after clone keeps old snapshot; crash after rename leaves complete new snapshot with original checkpoint. |
| `OrbitPublicationCrashChecks/main.swift:70` | Cancelled or SIGKILLed hard-link refresh keeps published snapshot; then reallocates all owners exactly. |
| `OrbitQueueChecks/main.swift:12` | SIGKILL mid-migration rolls back; reader unaffected; cancel rolls back; retry keeps dirty priority. |
| `OrbitQueueChecks/main.swift:40` | Reader sees stable rows during migration; cancellation rolls back and a partial migration never commits. |
| `OrbitQueueChecks/main.swift:53` | Retry creates pending indexes; dirty parent stays first, and rolled-back dirty updates keep priority. |
| `OrbitRemovalJournalChecks.swift:54` | An uncommitted removal rolls back; no operations are visible after the child exits before commit. |
| `OrbitRemovalJournalChecks.swift:60` | A live operation's intersections, containment relation and liveness are reported to recovery. |
| `OrbitRemovalJournalChecks.swift:72` | Unrelated root gets no intersections; liveness probe reports the live child operation. |
| `OrbitRemovalJournalChecks.swift:76` | An abandoned operation is not live; a replaced file keeps its original removal identity. |
| `OrbitRemovalJournalChecks.swift:80` | A 50000-item operation pages through intersections with bounded page size and completes. |
| `OrbitRemovalJournalChecks.swift:90` | A folder removal covering the recovery root is reported with the coversRoot relation. |
| `OrbitRemovalJournalChecks.swift:100` | An unresolved parent never authorizes removal and is reported as unresolved. |
| `OrbitRemovalJournalChecks.swift:127` | The pending operations listing skips completed operations and pages in sequence order. |
| `OrbitRemovalJournalChecks.swift:141` | Journal schema migration preserves the journal identity and its pending operations. |
| `OrbitRemovalJournalChecks.swift:149` | An inventory format rebuild keeps the removal journal, its identity and pending operations, and bumps journal version. |
| `OrbitRemovalJournalChecks.swift:162` | Journal database stays at user_version 3 after the inventory rebuild. |
| `OrbitRemovalJournalChecks.swift:168` | A removal path outside the recovery root is rejected as invalid. |
| `OrbitRemovalJournalChecks.swift:169` | A fence waiter cancelled while another holder is active returns CancellationError. |
| `OrbitRemovalJournalChecks.swift:186` | A missing operation lock fails closed, and liveness probing does not recreate it. |
| `OrbitRemovalJournalChecks.swift:194` | A recreated journal database yields a new identity. |
| `OrbitRemovalJournalChecks.swift:217` | Recovery of an unfinished operation returns its removals and reports live work. |
| `OrbitRemovalJournalChecks.swift:219` | Recovery reports removals from live and completed operations beyond the 256-row history window. |
| `OrbitRemovalJournalChecks.swift:226` | Idle recovery from the current cursor does not probe any operation. |
| `OrbitRemovalJournalChecks.swift:228` | Root replaced by a symlink during recovery raises journalChanged. |
| `OrbitRemovalJournalChecks.swift:322` | Saved recovery rejects a missing captured tail rather than looping over newer headers. |
| `OrbitRemovalJournalChecks.swift:329` | Saved recovery keeps its captured highwater; the next recovery consumes the later append. |
| `OrbitResumableChecks/main.swift:45` | SIGKILL resume in saved-first and partial-first order keeps the collection, selection and updated snapshot. |
| `OrbitResumableChecks/main.swift:141` | Stop before publication preserves the old saved index; restart then completes. |
| `OrbitResumableChecks/main.swift:172` | A deletion between a committed bulk batch and the fallback removes the stale row. |
| `OrbitResumableChecks/main.swift:188` | Staged and incremental hard-link allocation match the scanner across nested, case and Unicode names. |
| `OrbitResumableChecks/main.swift:203` | Staged and incremental hard-link allocations match scanner across nested, case, Unicode names. |
| `OrbitResumableChecks/main.swift:241` | A second process browses the saved map while another holds the stage; retry completes after release. |
| `OrbitResumableChecks/main.swift:252` | Lease held by another process keeps saved map browseable; retry completes after release. |
| `OrbitResumableChecks/main.swift:288` | Own cache and staging folders are excluded and the settled map does not loop (sleeps: inject clock). |
| `OrbitResumableChecks/main.swift:313` | Cancelling while the root publication lease is held throws CancellationError promptly. |
| `OrbitSavedSearchChecks/core.swift:18` | Saved search hides a removed subtree and files; the projected ancestor reports zero size and count. |
| `OrbitSavedSearchChecks/core.swift:22` | Projected totals and the 300-result cutoff match a fresh index after deletion. |
| `OrbitSavedSearchChecks/core.swift:27` | Projected search with removals matches a freshly saved index of the same tree. |
| `OrbitSavedSearchChecks/core.swift:28` | A removal with a mismatched file identity does not filter saved results. |
| `OrbitSavedSearchChecks/core.swift:31` | Ancestor projection keys preserve the original UTF-8 bytes of composed names. |
| `OrbitSavedSearchChecks/core.swift:41` | Ancestor key derived from a composed folder name preserves its original UTF-8 bytes. |
| `OrbitSavedSearchChecks/core.swift:47` | A projected search never modifies the saved index file bytes. |
| `OrbitSessionChecks/main.swift:90` | Legacy boot scope gets a live preview; current maps keep their view and saved caches survive. |
| `OrbitSessionChecks/main.swift:774` | Totals after deleting one hard link match a fresh scan's size and allocated size. |
| `OrbitSessionChecks/main.swift:946` | Full scan and later refresh reconcile removal epochs after a held publish and a held refresh. |
| `OrbitStageChecks/FreshFileInsertChecks.swift:5` | Compact fresh inserts keep duplicate names, replacement identities, directory transitions and rollback. |
| `OrbitStageChecks/FreshFileInsertChecks.swift:36` | Duplicate names and replaced identities stored correctly with exact totals; rollback path included. |
| `OrbitStageChecks/FreshFileInsertChecks.swift:49` | Directory replacement blocks file insert until the subtree is pruned; the child path is removed from the registry. |
| `OrbitStageChecks/FreshFileInsertChecks.swift:66` | A failed insert aborts the whole batch, leaving no rows or owners from that batch. |
| `OrbitStageChecks/FreshLeafGroupChecks.swift:72` | Grouped commit preserves prefix watermark, hard-link totals and rolls back stale, failed or cancelled groups. |
| `OrbitStageChecks/FreshLeafGroupChecks.swift:90` | Timed prefix commit advances the watermark exactly; remaining parents commit later. |
| `OrbitStageChecks/FreshLeafGroupChecks.swift:101` | Dirty, generation, identity, epoch, prune, failure and cancel inputs each roll back the whole group. |
| `OrbitStageChecks/FreshLeafGroupChecks.swift:129` | Gaps, large leaves, non-leaf records, record caps and byte caps all reject the group. |
| `OrbitStageChecks/FreshLeafGroupChecks.swift:156` | Empty leaf listings commit and clear pending directories with a durable watermark. |
| `OrbitStageChecks/FreshLeafGroupChecks.swift:162` | Crash before commit leaves nothing; crash after commit keeps all files and the watermark. |
| `OrbitStageChecks/FullRefreshQueueChecks.swift:5` | Bounded refresh cursor keeps low-path additions, pruning, reopen resume and rollback correct. |
| `OrbitStageChecks/FullRefreshQueueChecks.swift:30` | Chunked refresh marks the generation; low-path additions and pruned parents keep pending work. |
| `OrbitStageChecks/FullRefreshQueueChecks.swift:47` | Reopened interrupted refresh resumes and marks every unmarked directory. |
| `OrbitStageChecks/FullRefreshQueueChecks.swift:57` | A newer generation starts from zero instead of inheriting the prior generation's cursor. |
| `OrbitStageChecks/FullRefreshQueueChecks.swift:65` | A failed refresh chunk rolls back, the cursor does not advance, and retry completes. |
| `OrbitStageChecks/ListingEpochChecks.swift:80` | SIGKILL at listing boundaries keeps epoch, first batch, trim and pending child work atomic. |
| `OrbitStageChecks/ListingEpochChecks.swift:117` | Failed initial and replacement batches roll back epoch and all rows. |
| `OrbitStageChecks/ListingEpochChecks.swift:140` | Empty relisting trims prior files and subfolders and reassigns surviving hard links. |
| `OrbitStageChecks/ListingEpochChecks.swift:160` | Stale revision and stale refresh generation reject empty completion without minting an epoch. |
| `OrbitStageChecks/ListingEpochChecks.swift:175` | Empty fallback after committed bulk batch removes provisional rows and does not reuse epoch. |
| `OrbitStageChecks/ListingEpochChecks.swift:202` | Native fallback discards uncommitted sub-128 tail and reflects concurrent replacement exactly once. |
| `OrbitStageChecks/ListingEpochChecks.swift:228` | Fallback after fresh commit requeues and replaces the completed listing without stale rows. |
| `OrbitStageChecks/ListingEpochChecks.swift:256` | Root, offscreen and preferred folders commit first 128 rows before EOF. |
| `OrbitStageChecks/ListingEpochChecks.swift:289` | Dirty, refresh, prior-epoch, completed, oversized and over-budget fresh completions reject without mutation. |
| `OrbitStageChecks/ListingEpochChecks.swift:336` | Fresh completion atomic after refresh marking; pending and stale generations rejected without mutation. |
| `OrbitStageChecks/ListingEpochChecks.swift:373` | Conflicting fresh completion rolls back; success publishes rows, issues and child work atomically. |
| `OrbitStageChecks/ListingEpochChecks.swift:416` | Active folder visible before first commit; wrong identity or path reads rejected; cancel clears it. |
| `OrbitStageChecks/ListingEpochChecks.swift:460` | First 128 rows durable before background batches; schema version and no leaf path persistence. |
| `OrbitStageChecks/ListingEpochChecks.swift:492` | Multi-batch prune resumes after reopen with parent paths, sibling boundaries and hard-link owners intact. |
| `OrbitStageChecks/NoStaleTrimChecks.swift:67` | Trim with no stale work skips removal scans; generation guards, rollback and crash recovery hold. |
| `OrbitStageChecks/NoStaleTrimChecks.swift:84` | A stale file or directory is removed by trim and prune; totals match after reconciliation. |
| `OrbitStageChecks/NoStaleTrimChecks.swift:93` | A dirty revision or full-refresh generation keeps directories pending despite trim. |
| `OrbitStageChecks/NoStaleTrimChecks.swift:103` | A failed trim acknowledgement rolls back trim, dirty and pending state; retry succeeds. |
| `OrbitStageChecks/NoStaleTrimChecks.swift:115` | A cancelled writer leaves trim work unconsumed. |
| `OrbitStageChecks/NoStaleTrimChecks.swift:121` | Crash after the trim-empty write or commit leaves exactly the expected durable state. |
| `OrbitStageChecks/WriterCheckpointChecks.swift:5` | Commits release readers before COMMIT and reopened totals match; WAL bound is a resource guard. |
| `OrbitStageChecks/main.swift:5` | Old or unversioned inventories are rejected byte-for-byte; empty databases initialize. |
| `OrbitStageChecks/main.swift:23` | Mismatched directory identity in a batch rolls back fully and leaves prior rows intact. |
| `OrbitStageChecks/main.swift:50` | Signed device, high-bit inode and case-distinct hard links charge one owner correctly. |
| `OrbitStageChecks/main.swift:74` | Mount issue clears on replacement; directory replacement requires prune before insert. |
| `OrbitStageChecks/main.swift:131` | Interrupted candidate resumes; additions, owner deletion, identity replacement and bulk deletion reconcile exactly. |
| `OrbitStageChecks/main.swift:173` | Bulk removal reconciles to full-scan totals; the elapsed-time print is not asserted. |
| `OrbitStageChecks/main.swift:177` | Stale stage publisher is rejected and does not overwrite a changed cache file. |
| `OrbitStageChecks/main.swift:191` | Replacing an excluded cache directory during mapping does not pull its files into the stage. |
| `OrbitStageChecks/main.swift:205` | Cancellation is honored during 100k-removal projection. Wall-clock 2s deadline: inject clock or probe. |
| `OrbitStageRemovalChecks/AnchorCrash.swift:6` | SIGKILL before fresh cursor commit falls back to complete replay without losing intent. |
| `OrbitStageRemovalChecks/FreshAnchor.swift:10` | Fresh cursor skips no live removal intent; live completion advances cursor on later refresh. Uses 1.1s sleep. |
| `OrbitStageRemovalChecks/FreshAnchor.swift:22` | Removal committed between fresh capture and scan start is replayed; published map has no stale row. |
| `OrbitStageRemovalChecks/FreshAnchor.swift:34` | Recreated removal journal blocks replay and forces full reconciliation before publication. |
| `OrbitStageRemovalChecks/Live.swift:6` | Live deletion overlays while unrelated mapping proceeds; completion requeues parent before cursor ack. Uses 1.1s sleep. |
| `OrbitStageRemovalChecks/Overflow.swift:7` | >10k removals in one parent stay bounded, repair that parent, leave unrelated scan pending. |
| `OrbitStageRemovalChecks/OverflowCrash.swift:7` | SIGKILL during overflow parent repair preserves pending scan; restart hides stale rows. |
| `OrbitStageRemovalChecks/PublishRace.swift:7` | Deletion injected after clone is replayed before fenced rename; cursor travels with snapshot. |
| `OrbitStageRemovalChecks/ReadRace.swift:7` | Gated staged view and search spanning a replay transition reject stale results; no timers. |
| `OrbitStageRemovalChecks/Replay.swift:7` | Staged removal overlay persists until reconciliation; stale final watermark rejected, retry succeeds. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:156` | Operation appended during preparation is not applied beyond captured highwater. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:177` | Completion after header probe settles at expected cursor without losing items. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:194` | Completing a later cached header during replay still settles the cursor at the expected sequence. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:213` | Live operation delays cursor but does not hide unrelated staged map. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:227` | Cancellation during journal header probe is honored and leaves durable cursor at zero. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:243` | Root directory replaced between journal probes yields journalChanged retry and cursor unchanged. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:258` | Canonical root swapped for symlink after last unrelated probe is detected as journal change. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:258` | Canonical root swapped before a relevant journal page is detected; root validation still holds. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:281` | Missing captured journal tail is rejected as invalid; newer-only headers cannot advance cursor. |
| `OrbitStageRemovalChecks/ReplayBatch.swift:311` | Empty and acknowledged replay ranges keep browsing and detect a late append. Drop probe-call-count hooks. |
| `OrbitStageSearchFastChecks/cancel.swift:10` | Task or token cancellation interrupts an executing SQLite search via progress handler gate. |
| `OrbitStageSearchFastChecks/concurrent.swift:10` | Paused search stays on its snapshot while mapping adds 2000 matching files; gated, not timed. |
| `OrbitStageSearchFastChecks/damage.swift:5` | Damaged size, allocated, file_count or raw_alloc columns raise invalid even at limit 0. |
| `OrbitStageSearchFastChecks/equal-cancel.swift:20` | Task or token cancellation after entering equal-size fast path yields CancellationError; later search works. Gate-driven. |
| `OrbitStageSearchFastChecks/normalized.swift:34` | Normalized-equal IDs with distinct identities and punctuation survive equal-size search; overlays unchanged. |
| `OrbitStageSearchFastChecks/parity.swift:15` | Staged and live search match independent traversal across limits, Unicode folding, ties, identities and removal overlays. |
| `OrbitSubtreeReuseChecks/main.swift:45` | Legacy root scope upgrades once, keeps preview rows, and rejects stale discovery listings. |
| `OrbitSubtreeReuseChecks/main.swift:81` | Normalized kana names fall back to native traversal; search and accounting match the saved index. |
| `OrbitSubtreeReuseChecks/main.swift:166` | Completed descendant is reused in both orders; event floor kept; cross-source hardlink owners preserved. |
| `OrbitSubtreeReuseChecks/main.swift:187` | Tiny listings traverse natively while larger retained descendants are still reused. |
| `OrbitSubtreeReuseChecks/main.swift:214` | Source cursor floor is not relaxed without explicit history verification. |
| `OrbitSubtreeReuseChecks/main.swift:227` | One history validator does not block an unrelated discovery worker. Semaphore-driven, not timing-based. |
| `OrbitSubtreeReuseChecks/main.swift:251` | Stop and reopen keep the copied directory and original event checkpoint without recopying. |
| `OrbitSubtreeReuseChecks/main.swift:269` | Cancellation after copied pages propagates and preserves durable reuse progress. |
| `OrbitSubtreeReuseChecks/main.swift:287` | SIGKILL after imported pages keeps committed progress and resumes without double accounting. |
| `OrbitSubtreeReuseChecks/main.swift:303` | A source without removal provenance falls back without lowering the checkpoint. |
| `OrbitSubtreeReuseChecks/main.swift:313` | Saved replacement rows are reused using normal metadata, without private staging columns. |
| `OrbitSubtreeReuseChecks/main.swift:327` | Pre-existing and between-page deletion intents cannot restore removed records. |
| `OrbitSubtreeReuseChecks/main.swift:348` | Replacing a saved source keeps a coherent version or reconciles fully, preserving the event floor. |
| `OrbitSubtreeReuseChecks/main.swift:369` | Replacing a saved source during reuse yields a coherent old version or full reconciliation. |
| `OrbitSubtreeReuseChecks/main.swift:386` | Requested descendant priority does not overtake removal reconciliation of dirty parents. |

## Keep → `orbit-platform-macos`

| Source | Behavior to cover |
|---|---|
| `OrbitAsyncMappingChecks/DiscoveryHotPathChecks.swift:11` | Reused native bulk buffer matches fallback metadata, interrupts cleanly, keeps cache-key compatibility. |
| `OrbitAsyncMappingChecks/SecureCursorChecks.swift:69` | Admits boot-volume firmlink aliases but rejects duplicate routes and wrong inodes. |
| `OrbitAsyncMappingChecks/SecureCursorChecks.swift:120` | Rejects replaced cached ancestor directories and evicts stale cached child descriptors. |
| `OrbitAsyncMappingChecks/SecureCursorChecks.swift:136` | Rejects an ancestor swapped for a symlink during secure directory traversal. |
| `OrbitAsyncMappingChecks/SecureCursorChecks.swift:151` | Rejects a replaced root inode when cached cursor descriptors are reused. |
| `OrbitAsyncMappingChecks/SecureCursorChecks.swift:165` | Independent enumeration offsets, descriptor cache bound and no descriptor leak on close. |
| `OrbitAutomationTransportChecks/IntegrationInstallerChecks.swift:49` | CLI symlink install is idempotent; a conflicting CLI file is preserved; spaced paths survive. |
| `OrbitAutomationTransportChecks/IntegrationInstallerChecks.swift:57` | Repeated CLI install is a no-op; a pre-existing user file at the link path is preserved. |
| `OrbitBootVolumeChecks/main.swift:39` | Ordinary traversal scope rejects other devices, nested mounts and sibling byte-prefix paths. |
| `OrbitBootVolumeChecks/main.swift:47` | Physical Data Users alias and descendants pruned; unverified routes and replaced endpoints rejected. |
| `OrbitBootVolumeChecks/main.swift:63` | Bulk, streaming and fallback firmlink identities equal secure opens; stale firmlink identity rejected. |
| `OrbitBootVolumeChecks/main.swift:82` | Mapping coverage reaches home and nested firmlink targets but not pruned aliases or unrelated mounts. |
| `OrbitCacheResetChecks.swift:57` | A symlinked cache path is rejected before removal; retry after repair completes. |
| `OrbitCoreDiskScannerTests.swift:100` | Bulk enumeration matches fallback across batches including Unicode names and symlinks; cancellation honored. |
| `OrbitCoreDiskScannerTests.swift:135` | Named resource fork allocation matches the fallback walker. |
| `OrbitCoreTrashPolicyTests.swift:166` | Dataless materialization policy restored after nested failure; removal rechecks changed cloud policy. |
| `OrbitMappingCoverageChecks/main.swift:59` | A stage does not claim a known replaced directory or a replacement root. |
| `OrbitMappingCoverageChecks/main.swift:73` | Saved coverage requires both root identity and requested directory identity. |
| `OrbitMappingCoverageChecks/main.swift:101` | Byte-prefix sibling, symlink boundary, root and target identity, and cancellation are all enforced. |
| `OrbitMappingCoverageChecks/main.swift:134` | A supplied mounted volume stays a separate mapping root; runs only with a mount argument. |
| `OrbitMonitorChecks.swift:18` | Traversal segments and embedded NUL bytes cannot match an exclusion prefix. |
| `OrbitMonitorChecks.swift:20` | Empty-string device-root exclusion matches relative device-level event paths. |
| `OrbitMonitorChecks.swift:22` | Checkpoint from an old root scope triggers automatic full reconciliation. |
| `OrbitMonitorChecks.swift:33` | Home cache and its System Volumes Data firmlink alias are excluded from change feedback. |
| `OrbitMonitorChecks.swift:41` | Firmlink alias under /System/Volumes/Data matches the same excluded cache inode. |
| `OrbitMonitorChecks.swift:65` | Event under a removed ancestor normalizes to the missing child path within the root. |
| `OrbitMonitorChecks.swift:67` | Sibling directory and dot-dot escapes rejected; host and device-relative forms both checked. |
| `OrbitMonitorChecks.swift:84` | Vanished root is not accepted; moving an ancestor to a symlink invalidates the cache. |
| `OrbitMonitorChecks.swift:90` | Hooked ancestor move mid-normalization invalidates the cached root checkpoint. |
| `OrbitMonitorChecks.swift:109` | Real FSEvents create, rename and delete reach both alias and canonical roots with history finished. |
| `OrbitMonitorChecks.swift:122` | Real FSEvents create, rename, delete delivered through both root spellings; uses real sleeps. |
| `OrbitMonitorChecks.swift:138` | Replacing the watched root with a symlink forces a full refresh and invalidates the checkpoint. |
| `OrbitMonitorChecks.swift:163` | Batches delivered in arrival order with their own event-id watermarks. |
| `OrbitMonitorChecks.swift:177` | Burst queue stays bounded; overflow keeps its watermark and history completion. |
| `OrbitMonitorChecks.swift:188` | A 10,000-event batch is delivered with its watermark; printed timing is not asserted. |
| `OrbitMonitorChecks.swift:196` | Events from a stopped monitor generation are never delivered after restart. |
| `OrbitMonitorChecks.swift:210` | Writes inside an excluded cache do not feed back into refresh or overflow loops. |
| `OrbitRemovalJournalChecks.swift:66` | A physical /System/Volumes/Data alias intersects the removal under its native root. |
| `OrbitRemovalJournalChecks.swift:92` | A replaced parent folder invalidates the authorization to remove its child. |
| `OrbitRemovalJournalChecks.swift:105` | A same-inode parent moved behind a symlink does not authorize removal. |
| `OrbitRemovalJournalChecks.swift:116` | An overlapping symlink alias resolves a moved file to its new path under the root. |
| `OrbitRemovalJournalChecks.swift:184` | A symlinked journal lock file is rejected as unsafe. |
| `OrbitResumableChecks/main.swift:156` | Descriptor-relative fallback maps nested files and leaves symlinks untraversed. |
| `OrbitResumableChecks/main.swift:273` | A symlink replacing the saved database is rejected and its target is left untouched. |
| `OrbitSessionChecks/main.swift:456` | Symlinks are not reachable mapping paths; nested mounts become independent roots. |
| `OrbitSessionChecks/main.swift:802` | A replaced active root or known child is reloaded by its new identity, not the stale path. |
| `OrbitSessionChecks/main.swift:818` | An undiscovered child whose identity changed is replaced with the fresh contents. |
| `OrbitSubtreeReuseChecks/main.swift:405` | Same-volume FSEvents history reuses; changed parent identity rejected; unavailable history declines. |
| `OrbitSubtreeReuseChecks/main.swift:425` | Future parent host checkpoints and unverified mixed-stream sources are not reused. |
| `OrbitSubtreeReuseChecks/main.swift:430` | Verified source reuse retains the parent host checkpoint; unreachable sources bypass validation. |
| `OrbitSubtreeReuseChecks/main.swift:438` | Incomplete, unflushed, expired or invalidating history is rejected; exhausted budget refuses reuse. |
| `OrbitSubtreeReuseChecks/main.swift:441` | Only quiet, flushed, unexpired, complete FSEvents history is accepted for reuse. |
| `OrbitSubtreeReuseChecks/main.swift:454` | Native verifier accepts a quiet flushed source and detects a journaled change. Inject clock: usleep polling. |
| `OrbitSubtreeReuseChecks/main.swift:465` | Real FSEvents verifier accepts quiet source and detects change; polls real latency (inject clock). |
| `OrbitSubtreeReuseChecks/main.swift:474` | Physical /System/Volumes/Data alias updates reach their native map location. |

## Keep → `orbit-shell-macos`

| Source | Behavior to cover |
|---|---|
| `OrbitUpdaterChecks.swift:20` | Removal in progress blocks update checks, installs and termination. |

## Keep → `orbit-sunburst`

| Source | Behavior to cover |
|---|---|
| `OrbitInteractionChecks/main.swift:5` | Every grouped sector member stays available for drag and row highlighting. |
| `OrbitInteractionChecks/main.swift:28` | Partial, full, and collected-ancestor marks differ; tiny sectors omit badges. |
| `OrbitInteractionChecks/main.swift:68` | Cache reuses exact groups across selection and collection changes; equal-size replacement refreshes. |
| `OrbitInteractionChecks/main.swift:112` | Prepared hover group and layout survive selection and collection changes unchanged. |
| `OrbitInteractionChecks/main.swift:133` | Replaced child, new map revision, or new root produces a fresh prepared layout. |
| `OrbitInteractionChecks/main.swift:150` | Same-size file replaced with a new file identity refreshes the group member identity. |
| `OrbitInteractionChecks/main.swift:163` | Prepared child IDs keep nested segments, colors, hover groups and member lookup. |
| `OrbitInteractionChecks/main.swift:188` | Pending folders get no invented arc and partial totals stay incomplete; copy strings ignored. |
| `OrbitInteractionChecks/main.swift:215` | Branch colors follow folder IDs across reordering; shared palette colors keep separate identities. |
| `OrbitInteractionChecks/main.swift:281` | Hit test boundaries: sector edges, radial and angular gaps (analytic vs reference). |
| `OrbitRenderChecks.swift:153` | Base layer reused across hover; invalidated by collection change, resize and map refresh. |
| `OrbitRenderChecks.swift:160` | Collection change invalidates base layer while reusing the prepared geometry object. |
| `OrbitRenderChecks.swift:163` | Canvas resize and map revision refresh invalidate the cached base layer. |
| `OrbitRootGroupChecks/main.swift:67` | Grouped and ungrouped sunburst layouts match bit-for-bit across random trees, with and without IDs. |
| `OrbitRootGroupChecks/main.swift:76` | Layout without supplied child IDs matches baseline geometry and membership. |
| `OrbitRootGroupChecks/main.swift:78` | Saturated, Unicode-collision and duplicate labels use the baseline path with identical geometry. |
| `OrbitRootGroupChecks/main.swift:84` | Case-fold name collision forces baseline layout; output stays identical. |
| `OrbitRootGroupChecks/main.swift:89` | Group threshold boundary sizes produce identical geometry and membership. |
| `OrbitRootGroupChecks/main.swift:94` | Grouped directories keep membership and geometry parity with the baseline. |
| `OrbitRootGroupChecks/main.swift:98` | A zero-size member forces baseline layout; no segment reuses root IDs. |
| `OrbitRootGroupChecks/main.swift:115` | Same-count reordering matches baseline geometry and membership. |
| `OrbitRootGroupChecks/main.swift:117` | A same-count refresh with changed identity and size does not reuse stale nodes. |
| `OrbitRootGroupChecks/main.swift:129` | Nested directory groups match the baseline layout. |
| `OrbitRootGroupChecks/main.swift:133` | Two-level nested directories group with geometry identical to baseline. |

## Keep → `orbit-ui`

| Source | Behavior to cover |
|---|---|
| `OrbitFolderTransactionChecks/Colors.swift:50` | Presentation-only clear then reinstall stops an older delayed result overwriting colors. |
| `OrbitInteractionChecks/main.swift:343` | Drag starts from the pressed sector including groups; rejected drag never opens a folder. |
| `OrbitInteractionChecks/main.swift:354` | Navigation mid-gesture cannot activate stale sectors; interaction begin and end stay balanced. |
| `OrbitInteractionChecks/main.swift:440` | Row drag keeps its pressed item; a reused row cannot drag or open a replacement. |
| `OrbitInteractionChecks/main.swift:454` | Row reordered between clicks is selected without opening the replacement item. |
| `OrbitInteractionChecks/main.swift:474` | Chart begin and end interaction callbacks stay balanced through clicks, rejected drags and removal. |
| `OrbitInteractionChecks/main.swift:493` | Row begin and end interaction callbacks stay balanced through double-clicks, reuse and removal. |
| `OrbitInteractionOwnershipChecks/HoverRefreshHostedChecks.swift:57` | Hover resyncs to the item under a stationary pointer after a layout revision. Hosted pointer injection. |
| `OrbitLayoutChecks/LayoutChecks.swift:654` | Visible row anchor and focus survive a large name sort. |
| `OrbitListHoverChecks.swift:74` | Scroll clears hover once and restores the pointer row after settling (injected clock). |
| `OrbitListHoverChecks.swift:81` | Scroll suppression clears hover once, ignores row churn, restores current pointer row. |
| `OrbitListHoverChecks.swift:94` | Finger-up and live-scroll end cannot restore hover while momentum continues. |
| `OrbitListHoverChecks.swift:112` | Scrollbar live scrolling and cancelled gestures complete suppression lifecycle. |
| `OrbitListHoverChecks.swift:235` | Hover restore blocked through momentum and restarted by each wheel event; inject clock for quiet period. |
| `OrbitListHoverChecks.swift:273` | Table reset keeps visible row offset, focus and pending restore across replacement and cancel. |
