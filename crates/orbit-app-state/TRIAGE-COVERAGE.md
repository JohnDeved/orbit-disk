# Triage coverage: orbit-app-state

Rows of `docs/rust-port/test-triage.json` with `target: orbit-app-state` (321 behaviors). Test paths are
relative to the crate (`tests::…` under `src/tests/`; unit tests sit in their module).

| Coverage | Rows |
|---|---:|
| test: asserted by the named test(s) | 225 |
| design: cannot occur by construction (reason given) | 48 |
| backend: owned by the `Backend` implementation / orbit-core | 25 |
| shell: host concern; the state exposes the input | 3 |
| gap: implemented, not yet asserted | 20 |

| Swift check | Case | Coverage | Where / why |
|---|---|---|---|
| OrbitAppChecks.swift:70 | collected-ancestor-parity | test | `paths::tests::collected_ancestor_respects_components_normalization_and_unicode_bytes` |
| OrbitAppChecks.swift:99 | collection-coverage-boundaries | test | `coverage::tests::coverage_respects_component_boundaries_unicode_spellings_and_root` |
| OrbitAppChecks.swift:113 | equivalent unicode spellings keep separate coverage chains | test | `coverage::tests::coverage_respects_component_boundaries_unicode_spellings_and_root` |
| OrbitAppChecks.swift:138 | grouped-hover-caching | test | `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change`; `hover::stale_layout_hover_is_ignored_and_navigation_clears_hover`; `hover::search_rows_relate_to_exact_matches_and_to_group_ancestors` |
| OrbitAppChecks.swift:185 | hover relations follow navigation and same-path metadata | test | `hover::map_hover_relates_nodes_deep_segments_and_groups_to_rows`; `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change` |
| OrbitAppChecks.swift:198 | search results replace cached rows and group relations | test | `hover::search_rows_relate_to_exact_matches_and_to_group_ancestors` |
| OrbitAppChecks.swift:212 | name sort reorders cached rows and hover relations | test | `hover::map_hover_relates_nodes_deep_segments_and_groups_to_rows` |
| OrbitAppChecks.swift:218 | same-id folder and search refresh replace row metadata | design | design: rows read the installed tree / search hits directly; there is no row cache to invalidate |
| OrbitAppChecks.swift:298 | large-list hover row lookup follows sort, refresh, search | test | `hover::map_hover_relates_nodes_deep_segments_and_groups_to_rows`; design: rows read the installed tree / search hits directly; there is no row cache to invalidate |
| OrbitAppChecks.swift:338 | prepared group hover matches member-walk behavior | test | `hover::map_hover_relates_nodes_deep_segments_and_groups_to_rows` |
| OrbitAppChecks.swift:353 | search-filtered group hover includes ancestors only | test | `hover::search_rows_relate_to_exact_matches_and_to_group_ancestors` |
| OrbitAppChecks.swift:358 | fresh search results and same-id metadata update prepared hover | test | `hover::search_rows_relate_to_exact_matches_and_to_group_ancestors` |
| OrbitAppChecks.swift:363 | stale graph revision or different root cannot replace hover | test | `hover::stale_layout_hover_is_ignored_and_navigation_clears_hover` |
| OrbitAppChecks.swift:371 | parent validation respects path boundaries | design | design: groups are a layout segment plus node indices, never path prefixes |
| OrbitAppChecks.swift:377 | direct hover writes discard prepared membership | test | `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change` |
| OrbitAppChecks.swift:408 | first browsable partial map before scan completion | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection` |
| OrbitAppChecks.swift:421 | pending folders collectable and navigable on partial map | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection`; `search::preview_search_reruns_after_preview_changes_then_uses_the_saved_index`; `misc::automation_collects_and_removes_on_a_partial_map_with_its_own_receipt` |
| OrbitAppChecks.swift:433 | held map gesture defers progress and final handoff | test | `mapping::gesture_holds_updates_and_applies_them_in_order_on_release` |
| OrbitAppChecks.swift:451 | cancelled scan keeps partial map browsable | test | `mapping::pause_keeps_the_partial_map_and_resume_continues_in_a_new_session` |
| OrbitAppChecks.swift:462 | restart rediscovers parent without discarding route | test | `mapping::pause_keeps_the_partial_map_and_resume_continues_in_a_new_session` |
| OrbitAppChecks.swift:479 | restart retains partial preview and selection | test | `mapping::pause_keeps_the_partial_map_and_resume_continues_in_a_new_session`; `mapping::switching_roots_keeps_the_background_session_and_restores_its_view` |
| OrbitAppChecks.swift:502 | cancelled preview work cannot replace new root | test | `mapping::stale_resolution_and_preview_work_cannot_replace_a_newer_root` |
| OrbitAppChecks.swift:519 | completed scan browsable and removable when persistence fails | gap | a failed publication is a backend outcome; the failure path (M8) keeps the map, removal on it is not exercised |
| OrbitAppChecks.swift:564 | large saved folder rows and color assignments | test | `mapping::gesture_holds_a_refreshed_folder_and_a_background_name_sort` |
| OrbitAppChecks.swift:572 | filtered subfolder refresh keeps search results and selection | test | `search::preview_search_reruns_after_preview_changes_then_uses_the_saved_index` |
| OrbitAppChecks.swift:592 | cached folder prepares off main and stale preparation rejected | test | `mapping::superseded_folder_reads_never_replace_newer_navigation` |
| OrbitAppChecks.swift:605 | name sort background, cancellable, deferred during gesture | test | `mapping::gesture_holds_a_refreshed_folder_and_a_background_name_sort` |
| OrbitAppChecks.swift:638 | same-count folder refresh replaces row metadata | design | design: rows read the installed tree / search hits directly; there is no row cache to invalidate |
| OrbitAppChecks.swift:653 | large preview deferred during held graph gesture | test | `mapping::gesture_holds_updates_and_applies_them_in_order_on_release` |
| OrbitAppChecks.swift:703 | previous search rows stay usable during replacement search | test | `search::search_debounces_keeps_previous_rows_and_drops_superseded_results` |
| OrbitAppChecks.swift:718 | unreferenced search results released | design | design: rows read the installed tree / search hits directly; there is no row cache to invalidate |
| OrbitAppChecks.swift:744 | cached map displays before journal validation | test | `mapping::switching_roots_keeps_the_background_session_and_restores_its_view` |
| OrbitAppChecks.swift:761 | incompatible journal refresh publishes new revision | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection`; `mapping::colors_keep_their_branch_ordinals_and_stale_preparations_append` |
| OrbitAppChecks.swift:778 | direct navigation replacement invalidates graph child IDs | design | design: the layout and child list are keyed on the map revision bumped by every install |
| OrbitAppChecks.swift:786 | replaced cached root is rescanned not shown from old saved map | test | `mapping::replaced_item_at_a_mapped_path_is_mapped_fresh` |
| OrbitAppChecks.swift:826 | deep map segment highlights visible ancestor row | test | `hover::map_hover_relates_nodes_deep_segments_and_groups_to_rows` |
| OrbitAppChecks.swift:833 | query change clears hover and search rows relate to exact match | test | `hover::search_rows_relate_to_exact_matches_and_to_group_ancestors` |
| OrbitAppChecks.swift:845 | updated search results invalidate rows and hover with same query | test | `hover::search_rows_relate_to_exact_matches_and_to_group_ancestors` |
| OrbitAppChecks.swift:853 | sort change invalidates cached order and list hover keeps it | test | `hover::map_hover_relates_nodes_deep_segments_and_groups_to_rows` |
| OrbitAppChecks.swift:861 | search row drag collects exact source independent of selection | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitAppChecks.swift:888 | grouped collect collapses descendants and rejects protected items | test | `collection::drops_collapse_descendants_report_included_and_reject_ineligible_items` |
| OrbitAppChecks.swift:900 | collection total and coverage update on removal and clear | test | `collection::collection_changes_during_validation_retry_against_the_new_revision`; `collection::clearing_cancels_queued_additions`; `collection::nested_items_mark_ancestors_and_totals_saturate` |
| OrbitAppChecks.swift:911 | nested file collection marks ancestors as containing items | test | `collection::nested_items_mark_ancestors_and_totals_saturate` |
| OrbitAppChecks.swift:919 | drag payloads private, one-use, never collect on start | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitAppChecks.swift:936 | replacement directory not collected under old identity | test | `collection::replaced_items_are_rejected_instead_of_collected_under_their_old_identity` |
| OrbitAppChecks.swift:945 | replacement file not collected under old identity | test | `collection::replaced_items_are_rejected_instead_of_collected_under_their_old_identity` |
| OrbitAppChecks.swift:957 | large grouped drop validates off main and applies one batch | test | `collection::large_drops_buffer_slices_recheck_before_commit_and_small_requests_overtake`; `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitAppChecks.swift:971 | mixed grouped drop reports duplicates and skipped items | test | `collection::drops_collapse_descendants_report_included_and_reject_ineligible_items` |
| OrbitAppChecks.swift:979 | already-included group drop does not duplicate | test | `collection::drops_collapse_descendants_report_included_and_reject_ineligible_items` |
| OrbitAppChecks.swift:985 | reentrant direct collection assignment wins | test | `collection::collection_changes_during_validation_retry_against_the_new_revision`; design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitAppChecks.swift:998 | queued collection retries against reentrant changes | test | `collection::collection_changes_during_validation_retry_against_the_new_revision` |
| OrbitAppChecks.swift:1015 | collection change during validation triggers retry | test | `collection::collection_changes_during_validation_retry_against_the_new_revision` |
| OrbitAppChecks.swift:1032 | accepted collection resumes after same root restored | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitAppChecks.swift:1043 | reentrant revision change during validation retries | test | `collection::collection_changes_during_validation_retry_against_the_new_revision` |
| OrbitAppChecks.swift:1062 | reentrant root detachment preserves accepted collection | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitAppChecks.swift:1079 | paused scan keeps accepted collection work | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitAppChecks.swift:1094 | new drag invalidates previous token | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitAppChecks.swift:1097 | scan root change invalidates drag payloads | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitAppChecks.swift:1136 | mapped roots and protected folders never removable | test | `collection::drops_collapse_descendants_report_included_and_reject_ineligible_items` |
| OrbitAppChecks.swift:1155 | confirmed permanent removal runs in background | test | `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch` |
| OrbitAppChecks.swift:1161 | nested symlink target untouched by directory removal | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitAppChecks.swift:1164 | changed identities refused and kept reviewable | test | `removal::partial_failures_keep_survivors_reviewable_and_refresh_their_parents` |
| OrbitAppChecks.swift:1171 | removal outcome survives ensuing map update | test | `removal::partial_failures_keep_survivors_reviewable_and_refresh_their_parents` |
| OrbitAppChecks.swift:1181 | index refreshes removed and rejected replacement paths | test | `removal::partial_failures_keep_survivors_reviewable_and_refresh_their_parents` |
| OrbitAppChecks.swift:1193 | replaced root blocks removal despite identity match | test | `removal::a_replaced_root_stops_removal_and_pauses_updates` |
| OrbitAppChecks.swift:1214 | collection-totals-overflow-membership | test | `collection::nested_items_mark_ancestors_and_totals_saturate`; `coverage::tests::coverage_respects_component_boundaries_unicode_spellings_and_root` |
| OrbitAppChecks.swift:1229 | collection totals do not overflow | test | `collection::nested_items_mark_ancestors_and_totals_saturate` |
| OrbitAppChecks.swift:1252 | first-map-reuse-and-indexed-search | test | `mapping::nested_location_reuses_the_covering_map`; `search::search_debounces_keeps_previous_rows_and_drops_superseded_results` |
| OrbitAppChecks.swift:1262 | indexed search finds saved name | test | `search::search_debounces_keeps_previous_rows_and_drops_superseded_results` |
| OrbitAppChecks.swift:1266 | nested location reuses parent index | test | `mapping::nested_location_reuses_the_covering_map` |
| OrbitAppChecks.swift:1271 | selected deep item validates and collects | test | `collection::nested_items_mark_ancestors_and_totals_saturate` |
| OrbitAppChecks.swift:1280 | automatic refresh deferred during held drag | test | `mapping::gesture_holds_a_refreshed_folder_and_a_background_name_sort` |
| OrbitAppChecks.swift:1289 | published refresh keeps navigation, collection, hover, rows | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection`; `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change` |
| OrbitAppChecks.swift:1299 | superseded-refresh-and-pause-search | test | `mapping::superseded_folder_reads_never_replace_newer_navigation` |
| OrbitAppChecks.swift:1314 | pause finishes search without restarting mapping | test | `search::pausing_completes_a_preview_search_without_restarting_mapping` |
| OrbitAppChecks.swift:1344 | full refresh retains map and navigation made during scan | test | `misc::hidden_refresh_keeps_the_map_and_reads_counts_at_most_every_half_second` |
| OrbitAutomationChecks/main.swift:216 | native-removal-cannot-overwrite-receipt | test | `misc::automation_collects_and_removes_on_a_partial_map_with_its_own_receipt` |
| OrbitAutomationChecks/main.swift:235 | partial-map-collect-delete-without-discovery | test | `misc::automation_collects_and_removes_on_a_partial_map_with_its_own_receipt` |
| OrbitCollectionRemovalChecks.swift:41 | destructive-delete-refused-while-edit-pending | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race |
| OrbitCollectionRemovalChecks.swift:44 | drop queues while collection edit runs | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race |
| OrbitCollectionRemovalChecks.swift:47 | queued-drag-during-edits | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race |
| OrbitCollectionRemovalChecks.swift:54 | rapid removals survive closing the collection sheet | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race |
| OrbitCollectionRemovalChecks.swift:62 | rapid-edits-survive-sheet-close | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race |
| OrbitCollectionRemovalChecks.swift:72 | removal-survives-location-switch | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race |
| OrbitCollectionRemovalChecks.swift:88 | reentrant-launch-switch-keeps-removal | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race; design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitCollectionRemovalChecks.swift:97 | removal-survives-same-root-refresh | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race |
| OrbitCollectionRemovalChecks.swift:105 | older-edit-cannot-restore-after-clear | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race |
| OrbitCollectionSessionChecks/main.swift:62 | restored-map-attaches-own-validation | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitCollectionSessionChecks/main.swift:72 | restored B exposes only committed B collection | test | `mapping::switching_roots_keeps_the_background_session_and_restores_its_view`; `collection::validation_belongs_to_its_root_across_switches` |
| OrbitCollectionSessionChecks/main.swift:78 | B result and busy state kept after root publication | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitCollectionSessionChecks/main.swift:80 | undismissed-error-kept-per-root | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitCollectionSessionChecks/main.swift:88 | deferred-rejection-keeps-newer-error | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitCollectionSessionChecks/main.swift:95 | intervening-error-preserved | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitCollectionSessionChecks/main.swift:106 | return-rejection-visible-after-switch-back | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitCollectionSessionChecks/main.swift:114 | reentrant-row-edit-keeps-queued-addition | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitCollectionSessionChecks/main.swift:119 | reentrant row edit keeps accepted addition queued | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitCollectionSessionChecks/main.swift:126 | same-text-status-not-overwritten | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error`; design: feedback guards compare revisions, never text |
| OrbitCollectionSessionChecks/main.swift:134 | inherited-feedback-survives-forced-refresh | gap | collection status carried into a forced same-root session is implemented, not asserted |
| OrbitCollectionSessionChecks/main.swift:151 | offscreen-completion-no-cross-root-publish | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitCollectionSessionChecks/main.swift:169 | review-close-after-root-invalidation | gap | closing a review after root invalidation |
| OrbitCollectionSessionChecks/main.swift:173 | review closes after root invalidation without clearing unowned feedback | gap | closing a review after root invalidation |
| OrbitCollectionSessionChecks/main.swift:192 | stale review actions do not change another session | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitCollectionSessionChecks/main.swift:198 | stale-confirmation-cannot-act-on-changed-collection | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitCollectionSessionChecks/main.swift:201 | stale confirmation does not act on changed collection | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitCollectionSessionChecks/main.swift:211 | reopened-review-is-distinct-instance | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitCollectionSessionChecks/main.swift:226 | row action from dismissed review leaves reopened review alone | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitCollectionSessionChecks/main.swift:248 | rejected-outer-open-no-claim | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitCollectionSessionChecks/main.swift:257 | rejected outer open does not claim reentrant review | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitDurableRemovalChecks/main.swift:129 | replaced journal forces reconciliation before retiring state | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:131 | published cursor cannot retire newer deletion | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:175 | overflowing parent recovery falls back to full refresh | test | `removal::too_many_changed_parents_fall_back_to_a_full_refresh` |
| OrbitDurableRemovalChecks/main.swift:260 | cancelled refresh keeps published snapshot | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:263 | cancel during recovery publication keeps snapshot | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:326 | cumulative overflow across refreshes reconciled | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:352 | SIGKILL bulk removal reconciles without full stage scan | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:386 | unchanged live polls keep map epoch | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core); RemovalReplay is only sent on change |
| OrbitDurableRemovalChecks/main.swift:390 | overlapping live operation barrier | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:400 | refresh retry publishes both removals | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:409 | failed removal restored after pending intent | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:453 | stage removal reconciled on reopen after SIGKILL | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:460 | stage-recreated identity shown, stale row not | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:461 | saved map reconciled after SIGKILL without event history | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitDurableRemovalChecks/main.swift:461 | overlapping nested saved maps reconcile after kill | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitFailureRecoveryChecks/main.swift:38 | saved-refresh-failure-resumes-stage | test | `misc::monitor_changes_debounce_into_one_refresh_and_a_failed_update_pauses`; journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitFailureRecoveryChecks/main.swift:45 | offscreen-failure-resumes-stage | test | `mapping::mapping_failure_keeps_the_partial_map_and_offscreen_failures_stay_offscreen` |
| OrbitFailureRecoveryChecks/main.swift:49 | onscreen-failure-resumes-stage | test | `mapping::mapping_failure_keeps_the_partial_map_and_offscreen_failures_stay_offscreen` |
| OrbitFailureRecoveryChecks/main.swift:52 | inflight-failure-keeps-newer-opening | test | `mapping::failure_during_a_newer_opening_keeps_the_newer_request` |
| OrbitFailureRecoveryChecks/main.swift:61 | reentrant-switch-no-old-feedback | test | `mapping::mapping_failure_keeps_the_partial_map_and_offscreen_failures_stay_offscreen` |
| OrbitFailureRecoveryChecks/main.swift:75 | rescan-after-failure-reopens-stage | test | `mapping::mapping_failure_keeps_the_partial_map_and_offscreen_failures_stay_offscreen`; journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitFolderPlacesChecks.swift:21 | selected-path-boundary | test | `places::tests::selected_place_is_the_deepest_component_wise_container` |
| OrbitFolderPlacesChecks.swift:26 | unique-folder-places-persist | test | `places::tests::places_are_unique_absolute_and_exclude_builtins`; `misc::places_accept_only_new_directories_and_persist_changes` |
| OrbitFolderPlacesChecks.swift:40 | remove-place-persists-folder-intact | test | `misc::places_accept_only_new_directories_and_persist_changes` |
| OrbitFolderTransactionChecks/Colors.swift:12 | stale-preparation-keeps-newer-colors | test | `mapping::colors_keep_their_branch_ordinals_and_stale_preparations_append` |
| OrbitFolderTransactionChecks/Colors.swift:17 | noop-publication-revisions | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection`; `mapping::colors_keep_their_branch_ordinals_and_stale_preparations_append` |
| OrbitFolderTransactionChecks/Colors.swift:34 | delayed-result-after-clear-and-reinstall | test | `mapping::colors_keep_their_branch_ordinals_and_stale_preparations_append` |
| OrbitFolderTransactionChecks/Colors.swift:43 | delayed-result-after-presentation-clear | test | `mapping::colors_keep_their_branch_ordinals_and_stale_preparations_append` |
| OrbitFolderTransactionChecks/Notifications.swift:47 | root install synchronous switch completes | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitFolderTransactionChecks/Notifications.swift:47 | beginPreview switch completes synchronously | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitFolderTransactionChecks/Notifications.swift:47 | finishInstall switch completes synchronously | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitFolderTransactionChecks/Notifications.swift:53 | sync-root-switch-during-install | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitFolderTransactionChecks/Notifications.swift:55 | cancel-initial-install-no-old-folder | test | `mapping::stale_resolution_and_preview_work_cannot_replace_a_newer_root` |
| OrbitFolderTransactionChecks/Notifications.swift:59 | cancel-final-handoff-no-stale-folder | test | `mapping::pause_keeps_the_partial_map_and_resume_continues_in_a_new_session` |
| OrbitFolderTransactionChecks/Notifications.swift:66 | selection-change-retries-handoff | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitFolderTransactionChecks/Route.swift:42 | removal-during-saved-folder-preparation | gap | removal during saved-folder preparation: the epoch check reloads, not asserted |
| OrbitFolderTransactionChecks/Route.swift:57 | route-changed-during-preparation | test | `mapping::superseded_folder_reads_never_replace_newer_navigation` |
| OrbitFolderTransactionChecks/Synthetic.swift:18 | same-folder-install-retains-hover-selection | test | `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change`; `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection` |
| OrbitFolderTransactionChecks/Synthetic.swift:23 | reentrant list reset during install is rejected | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitFolderTransactionChecks/Synthetic.swift:39 | reentrant root clear rejected during switch | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitInteractionChecks/main.swift:49 | grouped-coverage-matches-members-unicode | test | `coverage::tests::grouped_coverage_matches_per_member_coverage_exhaustively` |
| OrbitInteractionChecks/main.swift:91 | grouped-coverage-exhaustive-parity | test | `coverage::tests::grouped_coverage_matches_per_member_coverage_exhaustively` |
| OrbitInteractionChecks/main.swift:333 | click-with-minor-movement-navigates | test | `gesture::tests::small_movements_still_click_and_threshold_starts_drag` |
| OrbitInteractionChecks/main.swift:429 | row-double-click-opens-once | test | `gesture::tests::double_click_opens_once_and_dragged_press_never_opens` |
| OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:7 | HoverRefreshModelChecks.run | test | `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change` |
| OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:72 | grouped hover survives same-membership refresh | test | `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change` |
| OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:84 | stale prepared hover cannot reappear after pointer exit | test | `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change` |
| OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:87 | colliding sibling identities keep identity rejection | gap | colliding sibling identities |
| OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:89 | identity, shrink, or root change clears stale hover | test | `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change`; `hover::stale_layout_hover_is_ignored_and_navigation_clears_hover` |
| OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:91 | pointer transition uses refreshed member data | design | design: hover holds node indices of the current tree, re-resolved by path and identity on install |
| OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:94 | unsafe refresh falls back to exact hovered members | test | `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change` |
| OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:97 | canonically equal ids use new bytes | design | design: hover holds node indices of the current tree, re-resolved by path and identity on install |
| OrbitInteractionOwnershipChecks/HoverRefreshModelChecks.swift:101 | old map callback cannot replace fresh hover | test | `hover::stale_layout_hover_is_ignored_and_navigation_clears_hover` |
| OrbitInteractionOwnershipChecks/main.swift:63 | chart-completion-keeps-new-row-press | test | `mapping::gesture_holds_updates_and_applies_them_in_order_on_release` |
| OrbitInteractionOwnershipChecks/main.swift:81 | chart-activation-reentrant-press | test | `mapping::gesture_holds_updates_and_applies_them_in_order_on_release`; design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitInteractionOwnershipChecks/main.swift:97 | reentrant-chart-drag-setup-no-stale-session | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitInteractionOwnershipChecks/main.swift:125 | reentrant-drag-keeps-newer-owner | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitInteractionOwnershipChecks/main.swift:142 | old-drag-completion-keeps-newer-drag | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitInteractionOwnershipChecks/main.swift:153 | chart-drag-session-end-stale | test | `mapping::gesture_holds_updates_and_applies_them_in_order_on_release` |
| OrbitInteractionOwnershipChecks/main.swift:193 | delayed-drop-after-source-completion | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes`; design: typed in-app drops are synchronous; no reservation or provider data exists |
| OrbitInteractionOwnershipChecks/main.swift:204 | root-change-drag-ownership | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitInteractionOwnershipChecks/main.swift:218 | stopped-session-completion-ignored | test | `mapping::pause_keeps_the_partial_map_and_resume_continues_in_a_new_session` |
| OrbitInteractionOwnershipChecks/main.swift:225 | reentrant-publication-holds-later-stages | test | `mapping::gesture_holds_updates_and_applies_them_in_order_on_release` |
| OrbitLargeCollectionChecks.swift:41 | small-drop-large-collection-totals | test | `collection::large_drops_buffer_slices_recheck_before_commit_and_small_requests_overtake` |
| OrbitLargeCollectionChecks.swift:46 | addition retried against newer collection revision | test | `collection::collection_changes_during_validation_retry_against_the_new_revision` |
| OrbitLargeCollectionChecks.swift:51 | held-validation-merges-replacement | test | `collection::collection_changes_during_validation_retry_against_the_new_revision` |
| OrbitLargeCollectionChecks.swift:59 | pause-keeps-accepted-collection-job | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitLargeCollectionChecks.swift:74 | reentrant-pause-during-launch-keeps-addition | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error`; design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitLargeCollectionChecks.swift:99 | validation preserves newer launch status | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitLargeCollectionChecks.swift:119 | older rejection does not overwrite newer error | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitLargeCollectionChecks.swift:128 | mapped-root-not-collectable | test | `collection::drops_collapse_descendants_report_included_and_reject_ineligible_items` |
| OrbitLargeCollectionChecks.swift:147 | completion-keeps-newer-feedback | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitLargeCollectionChecks.swift:169 | old-root-completion-publishes-nothing | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitLargeCollectionChecks.swift:194 | pause-during-completion-keeps-collection | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitLargeCollectionChecks.swift:217 | reentrant-pause-keeps-active-request-and-next-addition | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitLargeCollectionChecks.swift:234 | repeated-cancel-no-recursion | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitLargeCollectionChecks.swift:250 | older-cancelled-worker-keeps-newer-validation | test | `collection::clearing_cancels_queued_additions` |
| OrbitLargeCollectionChecks.swift:264 | completion preserves newer status with identical text | design | design: feedback guards compare revisions, never text |
| OrbitLargeCollectionChecks.swift:272 | completion-keeps-identical-status | design | design: feedback guards compare revisions, never text |
| OrbitLargeCollectionChecks.swift:282 | cancellation-keeps-identical-status | design | design: feedback guards compare revisions, never text |
| OrbitLargeCollectionChecks.swift:305 | failure-cleanup-no-stale-status-in-new-root | gap | job failure while another root is shown |
| OrbitLayoutChecks/LayoutChecks.swift:483 | pending-removal-button-visible-pixels | design | design: collection edits (row removal, clear) are synchronous; there is no pending edit queue to race |
| OrbitMappingReuseChecks/main.swift:60 | unfinished-switchback-observes-completion | test | `mapping::unfinished_session_continues_offscreen_and_its_completion_applies_on_return` |
| OrbitMappingReuseChecks/main.swift:77 | saved-switchback-observes-completion | test | `mapping::switching_roots_keeps_the_background_session_and_restores_its_view` |
| OrbitMappingReuseChecks/main.swift:116 | cached-session-drag-delete-before-index-load | test | `mapping::switching_roots_keeps_the_background_session_and_restores_its_view` |
| OrbitMappingReuseChecks/main.swift:145 | rejected initial preview observes resumed parent to completion | test | `mapping::unfinished_session_continues_offscreen_and_its_completion_applies_on_return` |
| OrbitPreviewBatchChecks/model.swift:45 | provisional-navigation-and-held-geometry | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection`; `mapping::gesture_holds_updates_and_applies_them_in_order_on_release` |
| OrbitPreviewBatchChecks/model.swift:66 | map interaction holds revision and group membership | test | `mapping::gesture_holds_updates_and_applies_them_in_order_on_release`; `hover::group_hover_survives_same_membership_updates_and_clears_when_members_change` |
| OrbitPreviewBatchChecks/model.swift:104 | provisional-route-stable-during-fallback | test | `mapping::deleting_the_open_folder_during_mapping_returns_to_its_parent` |
| OrbitPreviewBatchChecks/model.swift:133 | fallback removes vanished provisional folder and returns to root | test | `mapping::deleting_the_open_folder_during_mapping_returns_to_its_parent` |
| OrbitPreviewBatchChecks/model.swift:140 | provisional-files-collectable | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection`; `misc::automation_collects_and_removes_on_a_partial_map_with_its_own_receipt` |
| OrbitPreviewBatchChecks/model.swift:150 | provisional files collectable before mapping completes | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection`; `misc::automation_collects_and_removes_on_a_partial_map_with_its_own_receipt` |
| OrbitPreviewSearchChecks/model.swift:35 | partial search drops deleted file and keeps query | test | `search::removing_a_matched_item_during_a_partial_search_drops_it_and_keeps_the_query` |
| OrbitPreviewSearchChecks/model.swift:57 | same-query-refresh-keeps-results | test | `search::preview_search_reruns_after_preview_changes_then_uses_the_saved_index` |
| OrbitPreviewSearchChecks/model.swift:65 | reentrant-query-change-keeps-prior-rows | test | `search::search_debounces_keeps_previous_rows_and_drops_superseded_results` |
| OrbitPreviewSearchChecks/model.swift:81 | search-root-switch-in-notification | test | `search::search_results_of_a_previous_root_are_dropped` |
| OrbitPreviewSearchChecks/model.swift:93 | saved-handoff-tie-order | gap | stable tie order across the saved hand-off: the flag is passed to the backend; ordering itself is backend-owned |
| OrbitQueuedCollectionChecks/main.swift:74 | live-drag-before-inventory-save | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection` |
| OrbitQueuedCollectionChecks/main.swift:99 | navigation, drag, and deletion finish before inventory writer | test | `misc::automation_collects_and_removes_on_a_partial_map_with_its_own_receipt` |
| OrbitQueuedCollectionChecks/main.swift:136 | drops queue during validation and deletion; apply after switch | test | `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch` |
| OrbitQueuedCollectionChecks/main.swift:157 | large-group-already-included | gap | large already-included group drop |
| OrbitQueuedCollectionChecks/main.swift:164 | reservation-consumed-once-across-drag | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes`; design: typed in-app drops are synchronous; no reservation or provider data exists |
| OrbitQueuedCollectionChecks/main.swift:175 | reserved drop from other root kept out of visible collection | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitQueuedCollectionChecks/main.swift:182 | failed-provider-releases-reservation | design | design: typed in-app drops are synchronous; no reservation or provider data exists |
| OrbitQueuedCollectionChecks/main.swift:192 | clear-cancels-delayed-additions | test | `collection::clearing_cancels_queued_additions` |
| OrbitQueuedCollectionChecks/main.swift:207 | reentrant-root-switch-rejects-stale-drop | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitQueuedCollectionChecks/main.swift:219 | small-request-overtakes-group-remainder | test | `collection::large_drops_buffer_slices_recheck_before_commit_and_small_requests_overtake`; `collection::clearing_cancels_queued_additions` |
| OrbitQueuedCollectionChecks/main.swift:240 | small item overtakes group remainder | test | `collection::large_drops_buffer_slices_recheck_before_commit_and_small_requests_overtake` |
| OrbitQueuedCollectionChecks/main.swift:249 | buffered-delete-no-reappearance | test | `collection::large_drops_buffer_slices_recheck_before_commit_and_small_requests_overtake` |
| OrbitQueuedCollectionChecks/main.swift:278 | small request overtakes buffered group recheck | test | `collection::large_drops_buffer_slices_recheck_before_commit_and_small_requests_overtake` |
| OrbitQueuedCollectionChecks/main.swift:286 | confirmed-removal-reviewed-items-only | test | `removal::confirmed_removal_deletes_only_the_reviewed_items` |
| OrbitQueuedCollectionChecks/main.swift:317 | dismiss-during-held-publication | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitQueuedCollectionChecks/main.swift:331 | stale-lease-cannot-release-new-confirmation | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitQueuedCollectionChecks/main.swift:347 | stale confirmation lease cannot release newer one | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitQueuedCollectionChecks/main.swift:354 | clear-during-confirmation | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitQueuedCollectionChecks/main.swift:364 | clear during confirmation empties collection and closes review | test | `collection::review_tokens_reject_stale_actions_and_confirmation_holds_publication` |
| OrbitRemovalFinishChecks/main.swift:16 | completion of A preserves B feedback and completes A | test | `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch` |
| OrbitRemovalFinishExtraChecks/main.swift:24 | replacement-during-overlay-rebuild | gap | replacement during overlay rebuild |
| OrbitRemovalFinishExtraChecks/main.swift:38 | invalidate-root-preserves-later-feedback | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitRemovalFinishExtraChecks/main.swift:52 | feedback-switch-preserves-b-pause-state | test | `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch` |
| OrbitRemovalFinishExtraChecks/main.swift:68 | monitor-failure-preserves-b-feedback | gap | monitor failure during removal completion |
| OrbitRemovalFinishExtraChecks/main.swift:85 | monitor-failure-respects-invalidation | gap | monitor failure during removal completion |
| OrbitRemovalFinishExtraChecks/main.swift:102 | newer-feedback-during-commit-preserved | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitRemovalFinishExtraChecks/main.swift:117 | nested-removal-busy-across-switch | test | `removal::removals_in_two_sessions_complete_independently` |
| OrbitRemovalStartChecks/main.swift:31 | invalidate-root-keeps-owner-and-feedback | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitRemovalStartChecks/main.swift:32 | published-force-same-root-keeps-owner | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitRemovalStartChecks/main.swift:35 | published-force-then-switch-keeps-owner | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitRemovalStartChecks/main.swift:40 | activity-switch-keeps-owner | test | `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch`; design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitRemovalStartChecks/main.swift:42 | published-switch-keeps-owner | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitRemovalStartChecks/main.swift:44 | same-root-feedback-no-duplicate-removal | test | `misc::automation_collects_and_removes_on_a_partial_map_with_its_own_receipt` |
| OrbitResumableChecks/main.swift:66 | saved-map-first SIGKILL resume keeps collection and selection | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitResumableChecks/main.swift:70 | partial-map-first SIGKILL resume keeps collection | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitResumableChecks/main.swift:92 | reopen after stage publication reflects updated snapshot | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitResumableChecks/main.swift:112 | staged partial map survives cache purge | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitResumableChecks/main.swift:282 | own-index-and-stage-excluded | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitSavedSearchChecks/model.swift:33 | saved-search-hides-deletions-while-refresh-gated | gap | saved search with a removal overlay while refresh is gated |
| OrbitSavedSearchChecks/model.swift:49 | switch-from-removal-notification-restores-session | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitSearchInterruptionChecks.swift:21 | stopped-preview-search-resumes | test | `search::pausing_completes_a_preview_search_without_restarting_mapping` |
| OrbitSearchInterruptionChecks.swift:35 | failed-same-query-stays-incomplete | test | `search::failed_search_stays_outdated_until_retried` |
| OrbitSearchLifetimeChecks.swift:13 | no search publication after model deallocated mid-search | design | design: the state owns no tasks or callbacks; a dropped state cannot be published into |
| OrbitSearchLifetimeChecks.swift:37 | no-publication-after-model-drop | design | design: the state owns no tasks or callbacks; a dropped state cannot be published into |
| OrbitSessionChecks/main.swift:40 | child-id-index-tracks-folder-revision | design | design: rows read the installed tree / search hits directly; there is no row cache to invalidate |
| OrbitSessionChecks/main.swift:66 | known-file-share-progress | test | `footer::tests::indexed_share_never_implies_discovery_finished_or_exceeds_found` |
| OrbitSessionChecks/main.swift:145 | offscreen-progress-reads-finished-memory | test | `mapping::unfinished_session_continues_offscreen_and_its_completion_applies_on_return` |
| OrbitSessionChecks/main.swift:188 | discovery-completion-during-cache-backlog | test | `footer::tests::indexed_share_never_implies_discovery_finished_or_exceeds_found` |
| OrbitSessionChecks/main.swift:244 | progress-lease-teardown | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitSessionChecks/main.swift:293 | late-progress-cannot-replace-newer-revision | test | `mapping::older_progress_and_superseded_preview_reads_never_replace_newer_state` |
| OrbitSessionChecks/main.swift:300 | resumed-stage-revision-sequence | design | design: the layout and child list are keyed on the map revision bumped by every install; carried progress restarts at revision 0 |
| OrbitSessionChecks/main.swift:321 | navigating partial map retains root-wide discovery | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection` |
| OrbitSessionChecks/main.swift:329 | child-ID membership stays aligned across query and sort | test | `hover::map_hover_relates_nodes_deep_segments_and_groups_to_rows`; `hover::search_rows_relate_to_exact_matches_and_to_group_ancestors` |
| OrbitSessionChecks/main.swift:337 | reattach restores own collection and discovery progress | test | `mapping::switching_roots_keeps_the_background_session_and_restores_its_view` |
| OrbitSessionChecks/main.swift:344 | detached partial map saves and releases preview | test | `mapping::unfinished_session_continues_offscreen_and_its_completion_applies_on_return` |
| OrbitSessionChecks/main.swift:354 | retained-map-discovery-before-publication | test | `misc::hidden_refresh_keeps_the_map_and_reads_counts_at_most_every_half_second` |
| OrbitSessionChecks/main.swift:358 | retained map not newly discovered on forced refresh | test | `misc::hidden_refresh_keeps_the_map_and_reads_counts_at_most_every_half_second` |
| OrbitSessionChecks/main.swift:366 | switch during refresh restores refreshing map | test | `mapping::switching_away_during_a_refresh_restores_the_refreshing_map` |
| OrbitSessionChecks/main.swift:375 | incremental-refresh-withholds-saved-count | test | `footer::tests::pending_updates_withhold_counts_and_paused_updates_offer_refresh` |
| OrbitSessionChecks/main.swift:378 | restored updated snapshot keeps identity and size | test | `mapping::switching_roots_keeps_the_background_session_and_restores_its_view` |
| OrbitSessionChecks/main.swift:382 | incremental refresh withholds incompatible counts | test | `footer::tests::pending_updates_withhold_counts_and_paused_updates_offer_refresh` |
| OrbitSessionChecks/main.swift:395 | reattach during refresh keeps completed memory and updating phase | gap | reattach during refresh: phase not asserted |
| OrbitSessionChecks/main.swift:409 | replacement-at-same-path-scans-fresh | test | `mapping::replaced_item_at_a_mapped_path_is_mapped_fresh` |
| OrbitSessionChecks/main.swift:422 | stopped-mapping-resume | test | `mapping::pause_keeps_the_partial_map_and_resume_continues_in_a_new_session` |
| OrbitSessionChecks/main.swift:429 | stopped mapping retains counts; resume continues stage | test | `mapping::pause_keeps_the_partial_map_and_resume_continues_in_a_new_session` |
| OrbitSessionChecks/main.swift:462 | symlinked directory not reachable from its host | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitSessionChecks/main.swift:471 | stale-resolution-cannot-switch-root | test | `mapping::stale_resolution_and_preview_work_cannot_replace_a_newer_root` |
| OrbitSessionChecks/main.swift:477 | removal-during-held-deletion | test | `removal::publication_during_a_held_removal_keeps_the_removal_status`; `footer::tests::pending_updates_withhold_counts_and_paused_updates_offer_refresh` |
| OrbitSessionChecks/main.swift:492 | held deletion not displayed as fully saved | test | `footer::tests::pending_updates_withhold_counts_and_paused_updates_offer_refresh` |
| OrbitSessionChecks/main.swift:495 | switching during removal keeps removal across sessions | test | `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch` |
| OrbitSessionChecks/main.swift:498 | returning to held deletion preserves updating phase | gap | returning to a held deletion |
| OrbitSessionChecks/main.swift:504 | detached removal completes and empty map saves | test | `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch` |
| OrbitSessionChecks/main.swift:511 | multi-session-removals-both-complete | test | `removal::removals_in_two_sessions_complete_independently` |
| OrbitSessionChecks/main.swift:522 | removals in two locations complete independently | test | `removal::removals_in_two_sessions_complete_independently` |
| OrbitSessionChecks/main.swift:543 | scan-save-status-during-held-removal | test | `removal::publication_during_a_held_removal_keeps_the_removal_status` |
| OrbitSessionChecks/main.swift:555 | partial-deletion-held-map | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitSessionChecks/main.swift:568 | async discovery completes folder while writer held | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitSessionChecks/main.swift:576 | file drop accepted while mapping held | test | `mapping::partial_map_is_browsable_and_publication_keeps_route_selection_and_collection` |
| OrbitSessionChecks/main.swift:582 | drag does not cross mapped roots | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitSessionChecks/main.swift:589 | discovered folder drop accepted after switching back | gap | discovered folder drop after switching back |
| OrbitSessionChecks/main.swift:603 | retained-saved-item-collectable | gap | saved item collectable while its session reopens |
| OrbitSessionChecks/main.swift:612 | saved-map item collectable while session opens | gap | saved item collectable while its session reopens |
| OrbitSessionChecks/main.swift:671 | stopped removal row not visible before durable replay | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitSessionChecks/main.swift:681 | stage replay publishes deletion or recreated identity | backend | journal / stage / scanner behavior of the `Backend` implementation (orbit-core) |
| OrbitSessionChecks/main.swift:692 | pending-file-deletion-during-interaction | test | `mapping::gesture_holds_updates_and_applies_them_in_order_on_release` |
| OrbitSessionChecks/main.swift:708 | pending deletion keeps map revision during interaction | test | `mapping::gesture_holds_updates_and_applies_them_in_order_on_release` |
| OrbitSessionChecks/main.swift:739 | delete-open-scanning-folder-redirects | test | `mapping::deleting_the_open_folder_during_mapping_returns_to_its_parent` |
| OrbitSessionChecks/main.swift:752 | deleted open folder redirects to parent | test | `mapping::deleting_the_open_folder_during_mapping_returns_to_its_parent` |
| OrbitSessionChecks/main.swift:787 | mixed-removal-partial-failure | test | `removal::partial_failures_keep_survivors_reviewable_and_refresh_their_parents` |
| OrbitSessionChecks/main.swift:809 | replacing active root shows new contents | test | `mapping::replaced_item_at_a_mapped_path_is_mapped_fresh` |
| OrbitSessionChecks/main.swift:829 | saved-overlay-removal-projection | test | `removal::partial_failures_keep_survivors_reviewable_and_refresh_their_parents`; `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch` |
| OrbitSessionChecks/main.swift:837 | saved overlay projected removal survives reattach | test | `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch` |
| OrbitSessionChecks/main.swift:863 | cross-session-detached-removal | test | `removal::removal_locks_the_collection_and_merges_into_its_owner_after_a_switch` |
| OrbitSessionChecks/main.swift:885 | former-root-item-rejected-after-preview | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitSessionChecks/main.swift:898 | former root item rejected after root transition | test | `collection::drag_payloads_are_single_use_and_invalidated_by_new_drags_and_root_changes` |
| OrbitSessionChecks/main.swift:913 | stopped removal overlay and restart reconcile | gap | stopped removal overlay then restart |
| OrbitSessionChecks/main.swift:919 | missing-location-error-only | test | `mapping::missing_location_reports_an_error_and_opens_nothing` |
| OrbitSessionChecks/main.swift:961 | full-scan epochs reconcile after publication | gap | epoch change during a held full publication: the completion re-checks the epoch, not asserted |
| OrbitSessionChecks/main.swift:984 | collection-validation-belongs-to-root | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitSessionChecks/main.swift:1000 | collection validation belongs to its root across switches | test | `collection::validation_belongs_to_its_root_across_switches` |
| OrbitSessionChecks/main.swift:1007 | refresh-replacement-revalidates-collection | test | `collection::collection_changes_during_validation_retry_against_the_new_revision`; validation retries on removal-epoch changes exactly like revision changes |
| OrbitSessionChecks/main.swift:1014 | cancel-validation-discards-additions | test | `collection::clearing_cancels_queued_additions` |
| OrbitSessionChecks/main.swift:1020 | pause-keeps-accepted-collection-job | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitSessionChecks/main.swift:1025 | subscribers-see-committed-snapshot | design | design: the layout and child list are keyed on the map revision bumped by every install |
| OrbitSessionChecks/main.swift:1035 | observer-moves-root-during-notification | design | design: events are applied one at a time and no observer runs inside a transition, so reentrant notifications cannot occur |
| OrbitSessionChecks/main.swift:1055 | newer-status-not-hidden-by-rejection | test | `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitSessionChecks/main.swift:1072 | distinct rejection not hidden by newer status | test | `collection::validation_belongs_to_its_root_across_switches`; `collection::completion_preserves_newer_status_and_queues_its_rejection_behind_a_newer_error` |
| OrbitUpdaterChecks.swift:50 | postponed-restart-cancelled-by-new-removal | shell | `removal_active()` is exposed; restart postponement lives in the shell |
| OrbitUpdaterChecks.swift:66 | no-postpone-and-aborted-update-cleanup | shell | `removal_active()` is exposed; restart postponement lives in the shell |
| OrbitUpdaterChecks.swift:75 | nested-removal-during-deferred-restart | shell | `removal_active()` is exposed; restart postponement lives in the shell |
| OrbitVolumeChecks/main.swift:43 | fallback-volume-visible-while-blocked | test | `misc::volume_discovery_coalesces_and_keeps_a_root_fallback` |
| OrbitVolumeChecks/main.swift:48 | refresh-coalesces-to-one-worker | test | `misc::volume_discovery_coalesces_and_keeps_a_root_fallback` |
| OrbitVolumeChecks/main.swift:62 | actual-root-replaces-fallback | test | `misc::volume_discovery_coalesces_and_keeps_a_root_fallback` |
| OrbitVolumeChecks/main.swift:67 | empty-discovery-keeps-root-fallback | test | `misc::volume_discovery_coalesces_and_keeps_a_root_fallback` |
| OrbitVolumeChecks/main.swift:73 | refresh-during-finish-restarts-once | test | `misc::volume_discovery_coalesces_and_keeps_a_root_fallback` |
| OrbitVolumeChecks/main.swift:81 | blocked-discovery-does-not-retain-model | design | design: the state owns no tasks or callbacks; a dropped state cannot be published into |
