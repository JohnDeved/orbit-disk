//! Event-path normalization and batch decisions, ported from `Tests/OrbitMonitorChecks.swift`.

use std::path::Path;

use super::exclusion_sets;
use super::plan::{
    EventPathCache, EventPlan, PreparedBatch, RawEvent, prepare_batch, raw_path_excluded, resolve_event_path,
};
use crate::paths::{self, realpath};

fn bytes(path: &Path) -> Vec<u8> {
    paths::bytes(path).to_vec()
}

fn join(base: &[u8], tail: &str) -> Vec<u8> {
    let mut out = base.to_vec();
    out.push(b'/');
    out.extend_from_slice(tail.as_bytes());
    out
}

fn normalize(plan: &EventPlan, raw: &[u8]) -> Option<Vec<u8>> {
    plan.normalized_event_path(raw, &mut EventPathCache::default())
}

/// The same root watched relative to a fake device mounted at `/` (as Swift's device fixtures).
fn device_plan(plan: &EventPlan) -> EventPlan {
    EventPlan {
        physical_root: plan.canonical_root.clone(),
        mount_path: b"/".to_vec(),
        relative_path: Some(b"fixture".to_vec()),
        untracked_mounts: Vec::new(),
        ..plan.clone()
    }
}

/// The root addressed only by its real path, watched host-wide.
fn canonical_plan(plan: &EventPlan, real_root: &[u8]) -> EventPlan {
    EventPlan {
        root_path: real_root.to_vec(),
        canonical_root: real_root.to_vec(),
        physical_root: real_root.to_vec(),
        relative_path: None,
        untracked_mounts: Vec::new(),
        ..plan.clone()
    }
}

/// How FSEvents spells a native path for this plan's stream.
fn raw_spelling(plan: &EventPlan, native: &[u8]) -> Vec<u8> {
    match &plan.relative_path {
        None => native.to_vec(),
        Some(_) if plan.mount_path != b"/" && paths::contains(native, &plan.mount_path) => {
            paths::trim_slashes(&native[plan.mount_path.len()..]).to_vec()
        }
        Some(_) => native[1..].to_vec(),
    }
}

fn batch(plan: &EventPlan, excluded: &[Vec<u8>], raw_excluded: &[Vec<u8>], events: &[(&[u8], u32)]) -> PreparedBatch {
    let events: Vec<RawEvent<'_>> = events.iter().map(|&(path, flags)| RawEvent { path, flags, id: 1 }).collect();
    prepare_batch(plan, excluded, raw_excluded, &events, || true).expect("current")
}

#[test]
fn event_paths_map_into_the_root_and_never_escape_it() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let sibling = base.path().join("root-sibling");
    let outside = base.path().join("outside");
    for dir in [&root, &sibling, &outside] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let root_path = paths::standardize(&bytes(&root));
    let real_root = realpath(&root_path).unwrap();
    let real_sibling = realpath(&bytes(&sibling)).unwrap();
    let plan = EventPlan::inspect(&root).unwrap();
    let canonical = EventPlan::inspect(&paths::to_path(&real_root)).unwrap();
    let device = device_plan(&plan);
    let strip = |path: &[u8]| path[1..].to_vec();

    // Deleted items and deleted ancestors still map (lexically) below the caller's root spelling.
    assert_eq!(normalize(&plan, &join(&real_root, "deleted")), Some(join(&root_path, "deleted")));
    assert_eq!(normalize(&canonical, &join(&real_root, "deleted")), Some(join(&real_root, "deleted")));
    assert_eq!(normalize(&plan, &join(&real_root, "missing/child")), Some(join(&root_path, "missing/child")));
    assert_eq!(normalize(&device, &strip(&join(&real_root, "deleted"))), Some(join(&root_path, "deleted")));

    // Sibling byte-prefixes, `..` and symlinks never escape the root.
    assert_eq!(normalize(&plan, &join(&real_sibling, "deleted")), None, "sibling escaped");
    assert_eq!(normalize(&device, &strip(&join(&real_sibling, "deleted"))), None, "device sibling escaped");
    assert_eq!(normalize(&plan, &join(&real_root, "../outside/missing")), None, "dot-dot escaped");
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    assert_eq!(normalize(&plan, &join(&real_root, "escape/missing")), None, "symlink escaped");
    let host = canonical_plan(&plan, &real_root);
    assert_eq!(normalize(&host, &join(&real_root, "escape/missing")), None, "canonical symlink escaped");
    assert_eq!(
        normalize(&host, &join(&real_root, "escape")),
        Some(join(&real_root, "escape")),
        "link itself is an item"
    );
    assert_eq!(normalize(&device, &strip(&join(&real_root, "escape/missing"))), None, "device symlink escaped");

    // A vanished root resolves to nothing rather than to whatever replaced it.
    let vanished = base.path().join("race-root");
    std::fs::create_dir(&vanished).unwrap();
    let observed = realpath(&bytes(&vanished)).unwrap();
    std::fs::remove_dir(&vanished).unwrap();
    assert_eq!(resolve_event_path(&join(&observed, "gone"), &observed), None);

    // Unmappable events outside the root are ignored; ambiguous ones below it force a refresh.
    assert!(!batch(&plan, &[], &[], &[(&join(&real_sibling, "missing"), 0)]).full_refresh, "host sibling refreshed");
    assert!(!batch(&device, &[], &[], &[(&strip(&join(&real_sibling, "missing")), 0)]).full_refresh);
    assert!(
        batch(&plan, &[], &[], &[(&join(&real_root, "escape/missing"), 0)]).full_refresh,
        "ambiguous not refreshed"
    );
}

#[test]
fn moved_ancestor_invalidates_the_parent_cache() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    let cache_parent = root.join("cache-parent");
    std::fs::create_dir_all(cache_parent.join("child")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let cache_root = realpath(&bytes(&cache_parent.join("child"))).unwrap();
    let plan = EventPlan::inspect(&root).unwrap();
    let mut cache = EventPathCache::default();
    assert!(plan.normalized_event_path(&join(&cache_root, "one"), &mut cache).is_some());
    std::fs::rename(&cache_parent, outside.join("moved")).unwrap();
    std::os::unix::fs::symlink(outside.join("moved"), &cache_parent).unwrap();
    let _ = plan.normalized_event_path(&join(&cache_root, "two"), &mut cache);
    assert!(!cache.revalidate(), "moved ancestor did not invalidate the cache");
}

#[test]
fn replaced_root_invalidates_the_checkpoint() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("volatile");
    std::fs::create_dir(&root).unwrap();
    let plan = EventPlan::inspect(&root).unwrap();
    std::fs::rename(&root, base.path().join("old-volatile")).unwrap();
    std::os::unix::fs::symlink(base.path().join("old-volatile"), &root).unwrap();
    let event = join(&paths::standardize(&bytes(&root)), "changed");
    let result = batch(&plan, &[], &[], &[(&event, 0)]);
    assert!(result.full_refresh && result.identity_invalidated, "{result:?}");
}

#[test]
fn exclusions_match_whole_components_in_every_spelling() {
    let cache = vec![b"/root/cache".to_vec()];
    assert!(raw_path_excluded(b"/root/cache/file", &cache));
    assert!(raw_path_excluded(b"/root/cache", &cache));
    assert!(!raw_path_excluded(b"/root/cache-sibling/file", &cache), "sibling prefix matched");
    assert!(!raw_path_excluded(b"/root/cache/../other", &cache), "dot-dot matched");
    assert!(raw_path_excluded(b"relative/file", &[Vec::new()]), "device-root exclusion missed");

    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let cache_dir = root.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let plan = EventPlan::inspect(&root).unwrap();
    let (excluded, raw_excluded) = exclusion_sets(&plan, std::slice::from_ref(&cache_dir));
    // Writes into the excluded cache arrive in the physical spelling FSEvents uses.
    let real_cache = realpath(&bytes(&cache_dir)).unwrap();
    let physical = raw_spelling(&plan, &real_cache);
    let event = join(&physical, "index.sqlite");
    assert!(raw_path_excluded(&event, &raw_excluded), "raw exclusion missed {:?}", paths::lossy(&event));
    let result = batch(&plan, &excluded, &raw_excluded, &[(&event, 0)]);
    assert!(result.directories.is_empty() && !result.full_refresh, "{result:?}");
    let normalized_only = batch(&plan, &excluded, &[], &[(&event, 0)]);
    assert!(normalized_only.directories.is_empty() && !normalized_only.full_refresh, "{normalized_only:?}");
    let outside_cache = raw_spelling(&plan, &join(&realpath(&bytes(&root)).unwrap(), "other"));
    let root_path = paths::standardize(&bytes(&root));
    assert_eq!(
        batch(&plan, &excluded, &raw_excluded, &[(&outside_cache, 0)]).directories.into_iter().collect::<Vec<_>>(),
        vec![root_path]
    );
}
