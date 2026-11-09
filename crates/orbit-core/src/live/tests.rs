use super::*;
use crate::mailbox::{DirComponent, ListingCompletion, ListingFailure};
use crate::records::RecordBatch;
use crate::view::{VIEW_DEPTH, VIEW_DESCENDANT_BUDGET};
use orbit_platform::{EntryKind, EntryRecord};
use unicode_normalization::UnicodeNormalization;

const DEV: u64 = 7;

fn id(ino: u64) -> FileIdentity {
    FileIdentity { dev: DEV, ino }
}

#[derive(Clone, Copy)]
struct R<'a> {
    name: &'a str,
    kind: EntryKind,
    ino: u64,
    nlink: u32,
    size: u64,
    alloc: u64,
}

fn file<'a>(name: &'a str, ino: u64, size: u64) -> R<'a> {
    R { name, kind: EntryKind::File, ino, nlink: 1, size, alloc: size.div_ceil(4096) * 4096 }
}

fn link<'a>(name: &'a str, ino: u64, size: u64, alloc: u64) -> R<'a> {
    R { name, kind: EntryKind::File, ino, nlink: 2, size, alloc }
}

fn dir(name: &str, ino: u64) -> R<'_> {
    R { name, kind: EntryKind::Directory, ino, nlink: 1, size: 0, alloc: 0 }
}

/// A batch for the directory reached through `chain` (names with identities, root excluded).
fn batch(chain: &[(&str, u64)], records: &[R<'_>], complete: bool) -> ListingBatch {
    let mut path = b"/r".to_vec();
    let mut components = Vec::new();
    for (name, ino) in chain {
        path = crate::paths::join(&path, name.as_bytes());
        components.push(DirComponent { name: name.as_bytes().into(), identity: id(*ino) });
    }
    let parent_identity = chain.last().map_or(id(1), |(_, ino)| id(*ino));
    let mut batch = RecordBatch::default();
    for r in records {
        batch.push(&EntryRecord {
            name: r.name.as_bytes(),
            kind: r.kind,
            id: id(r.ino),
            nlink: r.nlink,
            size: r.size,
            allocated: r.alloc,
            is_mount_point: false,
        });
    }
    ListingBatch::new(
        Arc::from(path),
        parent_identity,
        Arc::from(components),
        batch,
        complete.then(ListingCompletion::default),
        None,
    )
}

fn inventory() -> LiveInventory {
    LiveInventory::new(TraversalScope::single_volume(b"/r", id(1)))
}

fn child<'t>(tree: &'t FolderTree, parent: u32, name: &str) -> Option<&'t crate::view::TreeNode> {
    tree.children(parent).iter().find(|&&c| tree.name(c) == name.as_bytes()).map(|&c| tree.node(c))
}

#[test]
fn hard_link_allocation_follows_path_order_through_arrival_and_removal() {
    let live = inventory();
    live.append(&batch(&[], &[dir("a", 10), dir("b", 11)], true));
    // The later path arrives first and owns the allocation for now.
    live.append(&batch(&[("b", 11)], &[link("x", 99, 100, 4096)], true));
    assert_eq!(live.node(b"/r/b").unwrap().allocated, 4096);
    // The depth-first-earlier path takes over; logical sizes stay per path.
    live.append(&batch(&[("a", 10)], &[link("y", 99, 100, 4096), file("z", 20, 10)], true));
    let a = live.node(b"/r/a").unwrap();
    let b = live.node(b"/r/b").unwrap();
    assert_eq!((a.allocated, b.allocated), (8192, 0));
    assert_eq!((a.size, b.size), (110, 100));
    let root = live.node(b"/r").unwrap();
    assert_eq!((root.allocated, root.size, root.file_count), (8192, 210, 3));
    let x = live.node(b"/r/b/x").unwrap();
    assert_eq!((x.allocated, x.raw_allocated), (0, 4096));
    // Removing the owner's folder hands ownership back to the remaining link.
    live.unavailable(b"/r/a", id(10), true);
    assert_eq!(live.node(b"/r/b").unwrap().allocated, 4096);
    let root = live.node(b"/r").unwrap();
    assert_eq!((root.allocated, root.size, root.file_count), (4096, 100, 1));
}

#[test]
fn replaced_directories_never_contribute_stale_descendants() {
    let live = inventory();
    live.append(&batch(&[], &[dir("a", 10)], true));
    live.append(&batch(&[("a", 10)], &[dir("old", 20)], true));
    // The parent is relisted and now holds a different directory under the same name.
    assert!(live.begin(b"/r/a", id(10)));
    live.append(&batch(&[("a", 10)], &[dir("old", 21)], true));
    // A listing of the replaced identity (queued before the swap) is rejected.
    assert!(!live.append(&batch(&[("a", 10), ("old", 20)], &[file("stale", 30, 500)], true)));
    assert!(live.append(&batch(&[("a", 10), ("old", 21)], &[file("fresh", 31, 7)], true)));
    assert_eq!(live.node(b"/r").unwrap().size, 7);
    assert_eq!(live.search("stale", 10, &[]).1, 0);
    let tree = live.view(b"/r/a", &[]).unwrap();
    assert_eq!(tree.children(FolderTree::FOLDER).len(), 1);
    // A restarted listing drops what was listed before, including whole subtrees.
    assert!(live.begin(b"/r/a", id(10)));
    assert_eq!(live.node(b"/r").unwrap().file_count, 0);
    assert!(live.view(b"/r/a/old", &[]).is_none());
}

#[test]
fn states_and_listing_flags_follow_discovery() {
    let live = inventory();
    assert_eq!(live.state(b"/r"), Some(ScanState::Pending));
    assert!(live.claim_root());
    live.append(&batch(&[], &[dir("a", 10), dir("denied", 12), file("f", 11, 5)], false));
    let tree = live.view(b"/r", &[]).unwrap();
    assert!(tree.is_listing);
    assert_eq!(tree.node(FolderTree::FOLDER).meta.state, ScanState::Scanning);
    assert_eq!(child(&tree, 0, "f").unwrap().meta.state, ScanState::Pending);
    assert_eq!(child(&tree, 0, "a").unwrap().meta.state, ScanState::Pending);
    live.append(&batch(&[], &[], true));
    live.unavailable(b"/r/denied", id(12), false);
    let tree = live.view(b"/r", &[]).unwrap();
    assert_eq!(tree.node(FolderTree::FOLDER).meta.state, ScanState::Complete);
    assert!(tree.is_listing, "a pending subfolder keeps the subtree listing");
    let denied = child(&tree, 0, "denied").unwrap();
    assert!(!denied.meta.flags.accessible);
    assert_eq!(denied.meta.state, ScanState::Complete);
    live.append(&batch(&[("a", 10)], &[], true));
    let tree = live.view(b"/r", &[]).unwrap();
    assert!(!tree.is_listing);
    // An empty readable folder differs from an unreadable one.
    let a = child(&tree, 0, "a").unwrap();
    assert!(a.meta.flags.accessible && a.meta.state == ScanState::Complete);
    // A failed listing keeps its partial contents but is no longer awaited.
    live.append(&batch(&[], &[dir("late", 13)], false));
    let mut failure = batch(&[("late", 13)], &[], false);
    failure.failure = Some(ListingFailure { message: "boom".into(), code: Some(5) });
    live.append(&batch(&[("late", 13)], &[file("partial", 40, 3)], false));
    live.append(&failure);
    assert_eq!(live.node(b"/r/late").unwrap().size, 3);
    assert_eq!(live.node(b"/r/late").unwrap().state, ScanState::Scanning);
    assert!(!live.view(b"/r", &[]).unwrap().is_listing);
}

#[test]
fn views_load_all_direct_children_and_bounded_descendants() {
    let live = inventory();
    // 300 direct children (more than one batch) and 40 folders of 100 files each.
    let mut names: Vec<String> = (0..260).map(|i| format!("f{i:03}")).collect();
    names.extend((0..40).map(|i| format!("d{i:02}")));
    let records: Vec<R<'_>> = names
        .iter()
        .enumerate()
        .map(|(i, n)| if n.starts_with('d') { dir(n, 1000 + i as u64) } else { file(n, 1000 + i as u64, i as u64) })
        .collect();
    live.append(&batch(&[], &records[..128], false));
    live.append(&batch(&[], &records[128..], true));
    let file_names: Vec<String> = (0..100).map(|i| format!("g{i:03}")).collect();
    for (i, n) in names.iter().enumerate().filter(|(_, n)| n.starts_with('d')) {
        let ino = 1000 + i as u64;
        let files: Vec<R<'_>> =
            file_names.iter().enumerate().map(|(j, f)| file(f, ino * 1000 + j as u64, 1 + j as u64)).collect();
        live.append(&batch(&[(n.as_str(), ino)], &files, true));
    }
    let tree = live.view(b"/r", &[]).unwrap();
    assert_eq!(tree.children(FolderTree::FOLDER).len(), 300);
    let descendants = tree.len() - 1 - 300;
    assert!(descendants <= VIEW_DESCENDANT_BUDGET);
    // Each hydrated folder loads its 96 largest children until the budget runs out.
    let first_dir = tree.children(FolderTree::FOLDER)[0];
    assert!(tree.node(first_dir).meta.flags.is_dir, "folders of 5050 bytes outrank files");
    let loaded = tree.children(first_dir);
    assert_eq!(loaded.len(), 96);
    assert_eq!(tree.node(first_dir).total_children, 100);
    assert_eq!(tree.node(loaded[0]).meta.size, 100);
    assert_eq!(descendants, 2000);
    // Totals of the folder equal the sum of its (complete) direct children.
    let sum: u64 = tree.children(FolderTree::FOLDER).iter().map(|&c| tree.node(c).meta.size).sum();
    assert_eq!(tree.node(FolderTree::FOLDER).meta.size, sum);
    // A nested view lists the root as its ancestor.
    let nested = live.view(b"/r/d00", &[]).unwrap();
    assert_eq!(nested.ancestors.len(), 1);
    assert_eq!(nested.path(nested.ancestors[0]), b"/r");
    assert_eq!(nested.children(FolderTree::FOLDER).len(), 100);
}

#[test]
fn removals_hide_matching_identities_and_adjust_ancestors_once() {
    let live = inventory();
    live.append(&batch(&[], &[dir("a", 10), file("keep", 11, 1)], true));
    live.append(&batch(&[("a", 10)], &[dir("b", 20), file("gone", 21, 100)], true));
    live.append(&batch(&[("a", 10), ("b", 20)], &[file("deep", 30, 1000)], true));
    let removal = |path: &str, ino: u64| ScanRemoval { path: path.as_bytes().to_vec(), identity: id(ino) };
    // A replacement with a different identity is not hidden.
    let tree = live.view(b"/r", &[removal("/r/a/gone", 999)]).unwrap();
    assert_eq!(tree.node(FolderTree::FOLDER).meta.size, 1101);
    // Removing a folder and something inside it subtracts once.
    let removals = [removal("/r/a/b", 20), removal("/r/a/b/deep", 30), removal("/r/a/gone", 21)];
    let tree = live.view(b"/r", &removals).unwrap();
    assert_eq!(tree.node(FolderTree::FOLDER).meta.size, 1);
    assert_eq!(tree.node(FolderTree::FOLDER).meta.file_count, 1);
    let a = tree.children(FolderTree::FOLDER).iter().copied().find(|&c| tree.name(c) == b"a").unwrap();
    assert_eq!(tree.node(a).meta.size, 0);
    assert!(tree.children(a).is_empty());
    assert!(live.view(b"/r/a/b", &removals).is_none());
    assert_eq!(live.file_count(&removals), Some(1));
    assert_eq!(live.file_count(&[removal("/r", 1)]), None);
    assert_eq!(live.search("deep", 10, &removals).1, 0);
    // A sibling with a name prefix is unaffected.
    assert!(live.view(b"/r", &[removal("/r/a/go", 21)]).is_some());
}

#[test]
fn search_ranks_by_size_then_path_and_folds_unicode() {
    let live = inventory();
    let nfc = "Résumé.txt";
    let nfd: String = nfc.nfd().collect();
    live.append(&batch(
        &[],
        &[
            file(nfc, 10, 5),
            file(&nfd, 11, 5),
            file("RESUME-big.txt", 12, 50),
            file("other", 13, 500),
            dir("resume-dir", 14),
        ],
        true,
    ));
    live.append(&batch(&[("resume-dir", 14)], &[file("inner", 20, 7)], true));
    let query = crate::fold::fold_query(" résume ");
    let (hits, total) = live.search(&query, 3, &[]);
    assert_eq!(total, 4);
    let paths: Vec<String> = hits.iter().map(|h| String::from_utf8(h.path.clone()).unwrap()).collect();
    // Largest first (the folder's subtree size counts), then path bytes: NFD sorts before NFC.
    assert_eq!(paths, ["/r/RESUME-big.txt", "/r/resume-dir", &format!("/r/{nfd}")]);
    assert_eq!(hits[1].meta.size, 7);
    assert_eq!(live.search(&query, 0, &[]), (Vec::new(), 4));
    assert_eq!(live.search("", 10, &[]).1, 0);
}

#[test]
fn nfc_and_nfd_siblings_stay_distinct() {
    let live = inventory();
    let nfc = "café";
    let nfd: String = nfc.nfd().collect();
    live.append(&batch(&[], &[dir(nfc, 10), dir(&nfd, 11)], true));
    live.append(&batch(&[(nfc, 10)], &[file("a", 20, 1)], true));
    live.append(&batch(&[(nfd.as_str(), 11)], &[file("b", 21, 2)], true));
    assert_eq!(live.node(format!("/r/{nfc}").as_bytes()).unwrap().size, 1);
    assert_eq!(live.node(format!("/r/{nfd}").as_bytes()).unwrap().size, 2);
    assert_eq!(live.view(b"/r", &[]).unwrap().children(FolderTree::FOLDER).len(), 2);
}

#[test]
fn concurrent_readers_see_consistent_totals() {
    let live = Arc::new(inventory());
    let dirs = 200u64;
    let names: Vec<String> = (0..dirs).map(|i| format!("d{i}")).collect();
    let records: Vec<R<'_>> = names.iter().enumerate().map(|(i, n)| dir(n, 100 + i as u64)).collect();
    live.append(&batch(&[], &records, true));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (live, done) = (live.clone(), done.clone());
        std::thread::spawn(move || {
            let mut views = 0;
            let mut last_revision = 0;
            while !done.load(std::sync::atomic::Ordering::Acquire) || views == 0 {
                let tree = live.view(b"/r", &[]).unwrap();
                assert!(tree.revision >= last_revision);
                last_revision = tree.revision;
                let kids = tree.children(FolderTree::FOLDER);
                let size: u64 = kids.iter().map(|&c| tree.node(c).meta.size).sum();
                let files: u64 = kids.iter().map(|&c| tree.node(c).meta.file_count).sum();
                let folder = tree.node(FolderTree::FOLDER).meta;
                assert_eq!((folder.size, folder.file_count), (size, files));
                views += 1;
            }
            views
        })
    };
    let file_names: Vec<String> = (0..50).map(|j| format!("f{j}")).collect();
    std::thread::scope(|s| {
        for w in 0..4u64 {
            let live = &live;
            let names = &names;
            let file_names = &file_names;
            s.spawn(move || {
                for i in (w..dirs).step_by(4) {
                    let ino = 100 + i;
                    let files: Vec<R<'_>> =
                        file_names.iter().enumerate().map(|(j, f)| file(f, ino * 100 + j as u64, j as u64)).collect();
                    live.append(&batch(&[(names[i as usize].as_str(), ino)], &files, true));
                }
            });
        }
    });
    done.store(true, std::sync::atomic::Ordering::Release);
    assert!(reader.join().unwrap() > 0);
    assert_eq!(live.discovered_file_count(), dirs * 50);
}

#[test]
fn views_stop_at_view_depth() {
    let live = inventory();
    let names = ["l1", "l2", "l3", "l4", "l5", "l6"];
    let mut chain: Vec<(&str, u64)> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let ino = 10 + i as u64;
        live.append(&batch(&chain, &[dir(name, ino), file("f", 100 + i as u64, 1)], true));
        chain.push((name, ino));
    }
    let tree = live.view(b"/r", &[]).unwrap();
    let depth = |mut ix: u32| {
        let mut d = 0;
        while ix != FolderTree::FOLDER {
            ix = tree.node(ix).parent;
            d += 1;
        }
        d
    };
    let deepest = (0..tree.len() as u32).map(depth).max().unwrap();
    assert_eq!(deepest, VIEW_DEPTH);
    assert_eq!(tree.node(FolderTree::FOLDER).meta.file_count, 6);
}

#[test]
fn relisting_a_folder_keeps_unchanged_subtrees_and_replaces_the_rest() {
    let live = inventory();
    live.claim_root();
    live.append(&batch(&[], &[dir("keep", 10), dir("swap", 11), file("gone", 12, 100), file("stay", 13, 50)], true));
    live.append(&batch(&[("keep", 10)], &[file("deep", 20, 1000)], true));
    live.append(&batch(&[("swap", 11)], &[file("old", 21, 500)], true));
    // The writer re-listed /r: `gone` vanished, `swap` is another directory now, `new` appeared.
    let relisted = batch(&[], &[dir("keep", 10), dir("swap", 31), file("stay", 13, 60), file("new", 32, 7)], true);
    assert!(live.relist(b"/r", id(1), &relisted.records));
    let tree = live.view(b"/r", &[]).unwrap();
    let root = tree.node(FolderTree::FOLDER).meta;
    // keep/deep survives; swap's old contents and `gone` do not.
    assert_eq!((root.file_count, root.size), (3, 1000 + 60 + 7));
    assert_eq!(child(&tree, FolderTree::FOLDER, "keep").unwrap().meta.size, 1000);
    assert!(child(&tree, FolderTree::FOLDER, "gone").is_none());
    assert_eq!(live.state(b"/r/swap"), Some(ScanState::Pending));
    assert_eq!(live.state(b"/r"), Some(ScanState::Complete));
    // A stale identity is rejected.
    assert!(!live.relist(b"/r/keep", id(99), &relisted.records));
}
