//! Journal, liveness, deletion and removal-run checks (Swift `OrbitRemovalJournalChecks`,
//! removal rows of `OrbitAppChecks`).

use super::*;
use std::io::{BufRead, BufReader, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, mpsc};

pub(crate) mod support {
    use super::super::*;
    use std::path::{Path, PathBuf};

    pub struct Fixture {
        _tmp: tempfile::TempDir,
        pub base: PathBuf,
    }

    /// A temporary directory with symlinks resolved, so raw and canonical spellings agree unless
    /// a test introduces aliases on purpose.
    pub fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        Fixture { _tmp: tmp, base }
    }

    impl Fixture {
        pub fn path(&self, rel: &str) -> PathBuf {
            self.base.join(rel)
        }

        pub fn journal(&self) -> Journal {
            Journal::open(&self.path("cache.staging"), JournalOptions::default()).unwrap()
        }

        pub fn db_path(&self) -> PathBuf {
            self.path("cache.staging").join(DIRECTORY_NAME).join("journal.sqlite")
        }

        pub fn raw_db(&self) -> rusqlite::Connection {
            rusqlite::Connection::open(self.db_path()).unwrap()
        }
    }

    pub fn bytes(path: &Path) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }

    pub fn id(path: &Path) -> FileIdentity {
        current_identity(&bytes(path)).expect("identity")
    }

    pub fn removal(path: &Path) -> ScanRemoval {
        ScanRemoval { path: bytes(path), identity: id(path) }
    }

    pub fn write(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    pub fn none() -> Cancellation {
        Cancellation::new()
    }

    /// Inserts `count` completed operations on an unrelated root directly into the database,
    /// as the Swift fixtures do.
    pub fn seed_unrelated(conn: &rusqlite::Connection, count: i64) {
        conn.execute_batch(&format!(
            "BEGIN IMMEDIATE;
             WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<{count})
             INSERT INTO operations(seq, root, root_dev, root_ino, lock_name, complete)
             SELECT i, CAST('/unrelated' AS BLOB), 1, 1,
                    printf('%08X-0000-0000-0000-000000000000.operation', i), 1 FROM n;
             INSERT INTO items(op, path, dev, ino, canonical, parent_dev, parent_ino)
             SELECT seq, CAST('/unrelated/file' AS BLOB), 1, 2, CAST('/unrelated/file' AS BLOB), 1, 1
             FROM operations;
             COMMIT;"
        ))
        .unwrap();
    }
}

use support::*;

// --- Child processes ------------------------------------------------------------------------

const CHILD_ENV: &str = "ORBIT_JOURNAL_CHILD";
const BASE_ENV: &str = "ORBIT_JOURNAL_BASE";

/// Entry point for child processes. A no-op unless the parent test set [`CHILD_ENV`]. The child
/// talks to the parent with lines on stdout/stdin; libtest's own lines are skipped.
#[test]
fn child_process_entry() {
    let Ok(mode) = std::env::var(CHILD_ENV) else { return };
    let base = PathBuf::from(std::env::var(BASE_ENV).unwrap());
    let journal = Journal::open(&base.join("cache.staging"), JournalOptions::default()).unwrap();
    let root = bytes(&base.join("root"));
    let stdin = std::io::stdin();
    let wait_for_parent = || {
        let mut line = String::new();
        stdin.lock().read_line(&mut line).unwrap();
    };
    let say = |what: &str| {
        println!("@@{what}");
        std::io::stdout().flush().unwrap();
    };
    match mode.as_str() {
        "uncommitted" => {
            Hooks::set(&journal.hooks.before_pending_commit, || std::process::exit(47));
            let item = removal(&base.join("root/nested/target"));
            let _ = journal.begin(&root, id(&base.join("root")), &[item], &none());
            unreachable!("the commit hook exits");
        }
        "live" => {
            let items = [removal(&base.join("root/nested/target")), removal(&base.join("root/nested/other"))];
            let op = journal.begin(&root, id(&base.join("root")), &items, &none()).unwrap();
            say("ready");
            wait_for_parent();
            std::fs::remove_file(base.join("root/nested/target")).unwrap();
            say("removed");
            wait_for_parent();
            drop(op);
            unreachable!("the parent kills this process");
        }
        "run-before" | "run-after" => {
            let item = removal(&base.join("root/dir/victim"));
            struct Gate<'a>(&'a dyn Fn(), bool);
            impl RemovalExecutor for Gate<'_> {
                fn remove(&mut self, item: &ScanRemoval, parent: FileIdentity) -> Result<(), PlatformError> {
                    if self.1 {
                        remove_no_follow(&item.path, item.identity, Some(parent))?;
                    }
                    (self.0)();
                    Ok(())
                }
            }
            let block = || {
                say("ready");
                wait_for_parent();
            };
            let run = RemovalRun::new(&journal, root.clone(), id(&base.join("root")), vec![item]);
            run.execute(&mut Gate(&block, mode == "run-after"), |_| None, &none());
            unreachable!("the parent kills this process");
        }
        other => panic!("unknown child mode {other}"),
    }
}

struct ChildProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ChildProcess {
    fn spawn(mode: &str, base: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "journal::tests::child_process_entry", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, mode)
            .env(BASE_ENV, base)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, stdout }
    }

    /// Blocks until the child announces `what` (a pipe handshake, no timers).
    fn expect(&mut self, what: &str) {
        let marker = format!("@@{what}");
        let mut line = String::new();
        loop {
            line.clear();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "child exited before {what}");
            // libtest may print `test <name> ... ` on the same line first.
            if line.trim_end().ends_with(&marker) {
                return;
            }
        }
    }

    fn proceed(&mut self) {
        writeln!(self.stdin, "go").unwrap();
        self.stdin.flush().unwrap();
    }

    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    fn wait_status(mut self) -> Option<i32> {
        self.child.wait().unwrap().code()
    }
}

fn tree(fx: &Fixture) {
    write(&fx.path("root/nested/target"), b"1");
    write(&fx.path("root/nested/other"), b"2");
    std::fs::create_dir_all(fx.path("root-other")).unwrap();
}

// --- Crash safety and liveness -------------------------------------------------------------

#[test]
fn process_exit_before_commit_leaves_no_operation() {
    let fx = fixture();
    tree(&fx);
    let journal = fx.journal();
    let child = ChildProcess::spawn("uncommitted", &fx.base);
    assert_eq!(child.wait_status(), Some(47));
    journal
        .with_fence(&none(), |fence| {
            assert_eq!(fence.high_water(), 0);
            assert!(fence.operations(0, 5)?.is_empty());
            Ok(())
        })
        .unwrap();
}

#[test]
fn killed_owner_turns_live_operation_into_abandoned_intent() {
    let fx = fixture();
    tree(&fx);
    let journal = fx.journal();
    let initial = journal.with_fence(&none(), |f| Ok(f.identity().to_string())).unwrap();
    let target_identity = id(&fx.path("root/nested/target"));
    let mut child = ChildProcess::spawn("live", &fx.base);
    child.expect("ready");
    let nested = bytes(&fx.path("root/nested"));
    journal
        .with_fence(&none(), |fence| {
            let entries = fence.operations(0, 5)?;
            assert_eq!(entries.len(), 1);
            assert!(!entries[0].completed);
            assert_eq!((fence.high_water(), fence.identity()), (1, initial.as_str()));
            let found = fence.intersections(1, &nested, None, 5)?;
            assert_eq!(found.len(), 2);
            assert!(found.iter().all(|i| i.relation == Relation::Inside && i.original_relation.is_none()));
            assert!(fence.intersections(1, &bytes(&fx.path("root-other")), None, 5)?.is_empty());
            assert!(fence.is_live(1)?, "a running owner must be reported live");
            Ok(())
        })
        .unwrap();
    child.proceed();
    child.expect("removed");
    // Still live after the partial mutation; never treated as abandoned while the owner runs.
    assert!(journal.with_fence(&none(), |f| f.is_live(1)).unwrap());
    child.kill();
    assert!(!journal.with_fence(&none(), |f| f.is_live(1)).unwrap(), "a killed owner is abandoned");
    // A file recreated at the removed path keeps the journal's original identity.
    write(&fx.path("root/nested/target"), b"3");
    assert_ne!(id(&fx.path("root/nested/target")), target_identity);
    let recorded = journal
        .with_fence(&none(), |f| f.intersections(1, &bytes(&fx.path("root")), None, 5))
        .unwrap()
        .into_iter()
        .find(|i| i.original_path == bytes(&fx.path("root/nested/target")))
        .unwrap();
    assert_eq!(recorded.removal.identity, target_identity);
}

#[test]
fn missing_operation_lock_fails_closed_and_is_not_recreated() {
    let fx = fixture();
    tree(&fx);
    let journal = fx.journal();
    let op = journal
        .begin(&bytes(&fx.path("root")), id(&fx.path("root")), &[removal(&fx.path("root/nested/target"))], &none())
        .unwrap();
    let dir = journal.directory().to_path_buf();
    let lock = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "operation"))
        .unwrap();
    std::fs::remove_file(&lock).unwrap();
    let error = journal.with_fence(&none(), |f| f.is_live(op.sequence())).unwrap_err();
    assert!(matches!(error, JournalError::Unsafe(_)), "{error:?}");
    assert!(!lock.exists(), "liveness probing must not recreate a lost lock");
}

#[test]
fn recreated_database_gets_a_new_identity() {
    let fx = fixture();
    let journal = fx.journal();
    let first = journal.with_fence(&none(), |f| Ok(f.identity().to_string())).unwrap();
    std::fs::remove_file(fx.db_path()).unwrap();
    let second = journal.with_fence(&none(), |f| Ok(f.identity().to_string())).unwrap();
    assert_ne!(first, second);
}

#[test]
fn symlinked_fence_lock_is_rejected() {
    let fx = fixture();
    tree(&fx);
    let journal = fx.journal();
    std::os::unix::fs::symlink(fx.path("root/nested/target"), journal.directory().join("journal.lock")).unwrap();
    let error = journal.with_fence(&none(), |_| Ok(())).unwrap_err();
    assert!(matches!(error, JournalError::Unsafe(_)), "{error:?}");
    assert_eq!(std::fs::read(fx.path("root/nested/target")).unwrap(), b"1");
}

#[test]
fn replaced_journal_directory_is_rejected() {
    let fx = fixture();
    let journal = fx.journal();
    journal.with_fence(&none(), |_| Ok(())).unwrap();
    std::fs::rename(journal.directory(), fx.path("old-journal")).unwrap();
    std::fs::create_dir(journal.directory()).unwrap();
    std::fs::set_permissions(journal.directory(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    assert!(matches!(journal.with_fence(&none(), |_| Ok(())), Err(JournalError::Unsafe(_))));
}

// --- Fence ---------------------------------------------------------------------------------

#[test]
fn fence_wait_honours_cancellation() {
    let fx = fixture();
    let journal = Arc::new(fx.journal());
    let cancelled = Cancellation::new();
    cancelled.cancel();
    assert!(matches!(journal.with_fence(&cancelled, |_| Ok(())), Err(JournalError::Cancelled)));

    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = {
        let journal = Arc::clone(&journal);
        std::thread::spawn(move || {
            journal
                .with_fence(&none(), |_| {
                    held_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .unwrap();
        })
    };
    held_rx.recv().unwrap();
    let token = Cancellation::new();
    let (busy_tx, busy_rx) = mpsc::sync_channel(1);
    Hooks::set(&journal.hooks.fence_busy, move || {
        let _ = busy_tx.try_send(());
    });
    let waiter = {
        let journal = Arc::clone(&journal);
        let token = token.clone();
        std::thread::spawn(move || journal.with_fence(&token, |_| Ok(())))
    };
    busy_rx.recv().unwrap(); // the waiter observed the held fence
    token.cancel();
    assert!(matches!(waiter.join().unwrap(), Err(JournalError::Cancelled)));
    release_tx.send(()).unwrap();
    holder.join().unwrap();
}

#[test]
fn read_only_access_creates_nothing_and_does_not_wait() {
    let fx = fixture();
    let parent = fx.path("cache.staging");
    let read_only = JournalOptions { access: Access::ReadOnly, ..JournalOptions::default() };
    assert!(Journal::open(&parent, read_only.clone()).is_err());
    assert!(!parent.exists());
    let journal = fx.journal();
    let reader = Journal::open(&parent, read_only).unwrap();
    assert!(reader.with_fence(&none(), |_| Ok(())).is_err(), "no database yet");
    let identity = journal.with_fence(&none(), |f| Ok(f.identity().to_string())).unwrap();
    assert_eq!(reader.with_fence(&none(), |f| Ok(f.identity().to_string())).unwrap(), identity);
    let busy = journal.with_fence(&none(), |_| Ok(reader.with_fence(&none(), |_| Ok(()))));
    assert!(matches!(busy.unwrap(), Err(JournalError::Busy)));
}

// --- Intersections and authorization -------------------------------------------------------

#[test]
fn operation_pages_bounded_items() {
    let fx = fixture();
    let journal = fx.journal();
    std::fs::create_dir_all(fx.path("root")).unwrap();
    let root = bytes(&fx.path("root"));
    let items: Vec<_> = (0..50_000u64)
        .map(|i| ScanRemoval {
            path: bytes(&fx.path(&format!("root/file-{i}"))),
            identity: FileIdentity { dev: 1, ino: i + 1 },
        })
        .collect();
    let op = journal.begin(&root, id(&fx.path("root")), &items, &none()).unwrap();
    let rows = journal
        .with_fence(&none(), |fence| {
            let mut after: Option<Vec<u8>> = None;
            let mut count = 0;
            loop {
                let page = fence.intersections(op.sequence(), &root, after.as_deref(), MAX_PAGE)?;
                count += page.len();
                if page.len() < MAX_PAGE {
                    return Ok(count);
                }
                after = Some(page.last().unwrap().original_path.clone());
            }
        })
        .unwrap();
    assert_eq!(rows, 50_000);
    assert!(journal.with_fence(&none(), |f| f.intersections(1, &root, None, MAX_PAGE + 1)).is_err());
    op.complete(&none()).unwrap();
    assert!(journal.with_fence(&none(), |f| f.operations(0, 1)).unwrap()[0].completed);
    assert!(matches!(op.complete(&none()), Err(JournalError::Invalid(_))), "completion is once only");
}

#[test]
fn folder_removal_covers_recovery_root_and_outside_paths_are_refused() {
    let fx = fixture();
    tree(&fx);
    let journal = fx.journal();
    let root = bytes(&fx.path("root"));
    let op = journal.begin(&root, id(&fx.path("root")), &[removal(&fx.path("root/nested"))], &none()).unwrap();
    let found = journal
        .with_fence(&none(), |f| f.intersections(op.sequence(), &bytes(&fx.path("root/nested")), None, 5))
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].relation, Relation::CoversRoot);
    for bad in [bytes(&fx.path("escape")), root.clone(), bytes(&fx.path("root/../escape"))] {
        let item = ScanRemoval { path: bad, identity: FileIdentity { dev: 1, ino: 1 } };
        assert!(matches!(journal.begin(&root, id(&fx.path("root")), &[item], &none()), Err(JournalError::Invalid(_))));
    }
}

#[test]
fn changed_or_unresolved_parents_never_authorize_removal() {
    let fx = fixture();
    let journal = fx.journal();
    let root_path = fx.path("root");
    std::fs::create_dir_all(fx.path("root/parent")).unwrap();
    std::fs::create_dir_all(fx.path("root/movable")).unwrap();
    let root = bytes(&root_path);
    let root_id = id(&root_path);
    let item = |rel: &str| ScanRemoval { path: bytes(&fx.path(rel)), identity: FileIdentity { dev: 1, ino: 9 } };

    // Replaced parent folder.
    let replaced = journal.begin(&root, root_id, &[item("root/parent/child")], &none()).unwrap();
    let child = bytes(&fx.path("root/parent/child"));
    assert!(replaced.parent_is_unchanged(&child));
    std::fs::rename(fx.path("root/parent"), fx.path("root/old-parent")).unwrap();
    std::fs::create_dir(fx.path("root/parent")).unwrap();
    assert!(!replaced.parent_is_unchanged(&child));

    // Unresolved parent: never authorizes and intersects every root as unresolved.
    let missing = journal.begin(&root, root_id, &[item("root/missing/child")], &none()).unwrap();
    assert!(!missing.parent_is_unchanged(&bytes(&fx.path("root/missing/child"))));
    let found = journal
        .with_fence(&none(), |f| f.intersections(missing.sequence(), &bytes(&fx.path("unrelated")), None, 5))
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].relation, Relation::Unresolved);

    // Same inode, but now reached through a symlink: the canonical spelling changed.
    let moved = journal.begin(&root, root_id, &[item("root/movable/child")], &none()).unwrap();
    let moved_child = bytes(&fx.path("root/movable/child"));
    assert!(moved.parent_is_unchanged(&moved_child));
    let before = canonical::directory_identity(&bytes(&fx.path("root/movable")));
    std::fs::rename(fx.path("root/movable"), fx.path("root/moved-parent")).unwrap();
    std::os::unix::fs::symlink(fx.path("root/moved-parent"), fx.path("root/movable")).unwrap();
    assert_eq!(canonical::directory_identity(&bytes(&fx.path("root/movable"))), before);
    assert!(!moved.parent_is_unchanged(&moved_child));
}

#[test]
fn symlink_alias_projects_moved_item_under_root() {
    let fx = fixture();
    let journal = fx.journal();
    write(&fx.path("root/new-location/file"), b"4");
    std::os::unix::fs::symlink(fx.path("root/new-location"), fx.path("root/old-location")).unwrap();
    let root = bytes(&fx.path("root"));
    let old_file = bytes(&fx.path("root/old-location/file"));
    let op = journal
        .begin(
            &root,
            id(&fx.path("root")),
            &[ScanRemoval { path: old_file.clone(), identity: id(&fx.path("root/new-location/file")) }],
            &none(),
        )
        .unwrap();
    let found = journal.with_fence(&none(), |f| f.intersections(op.sequence(), &root, None, 5)).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].removal.path, bytes(&fx.path("root/new-location/file")));
    assert_eq!(found[0].original_path, old_file);
    assert_eq!(found[0].original_relation, Some(Relation::Inside));
    // Recovering the alias itself matches lexically only.
    let lexical = journal
        .with_fence(&none(), |f| f.intersections(op.sequence(), &bytes(&fx.path("root/old-location")), None, 5))
        .unwrap();
    assert_eq!(lexical.len(), 1);
    assert_eq!(lexical[0].removal.path, old_file);
    // The same folder reached through a symlinked root still intersects.
    std::os::unix::fs::symlink(fx.path("root"), fx.path("root-alias")).unwrap();
    let alias = journal
        .with_fence(&none(), |f| f.intersections(op.sequence(), &bytes(&fx.path("root-alias")), None, 5))
        .unwrap();
    assert_eq!(alias.len(), 1);
    assert_eq!(alias[0].removal.path, bytes(&fx.path("root-alias/new-location/file")));
}

#[test]
fn pending_listing_skips_completed_operations_in_order() {
    let fx = fixture();
    tree(&fx);
    let journal = fx.journal();
    let root = bytes(&fx.path("root"));
    let rid = id(&fx.path("root"));
    let first = journal.begin(&root, rid, &[removal(&fx.path("root/nested/target"))], &none()).unwrap();
    let second = journal.begin(&root, rid, &[removal(&fx.path("root/nested/other"))], &none()).unwrap();
    let third = journal.begin(&root, rid, &[removal(&fx.path("root/nested"))], &none()).unwrap();
    second.complete(&none()).unwrap();
    let mut seen = Vec::new();
    journal
        .with_fence(&none(), |fence| {
            let mut after = 0;
            while let Some(next) = fence.pending_operations(after, 1)?.into_iter().next() {
                assert!(!next.completed && next.sequence > after);
                seen.push(next.sequence);
                after = next.sequence;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(seen, [first.sequence(), third.sequence()]);
}

// --- Format compatibility ------------------------------------------------------------------

const SWIFT_V3: &str = "
PRAGMA application_id=1330793038; PRAGMA user_version=3;
CREATE TABLE metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),identity TEXT NOT NULL);
CREATE TABLE operations(seq INTEGER PRIMARY KEY AUTOINCREMENT,root BLOB NOT NULL,root_identity TEXT NOT NULL,lock_name TEXT NOT NULL UNIQUE,complete INTEGER NOT NULL DEFAULT 0);
CREATE INDEX pending_operations ON operations(complete,seq);
CREATE TABLE items(op INTEGER NOT NULL,path BLOB NOT NULL,identity TEXT NOT NULL,canonical BLOB,parent_identity TEXT,PRIMARY KEY(op,path)) WITHOUT ROWID;
";

#[test]
fn swift_journal_is_migrated_with_identity_cursor_and_live_operations() {
    let fx = fixture();
    tree(&fx);
    let journal_dir = fx.path("cache.staging").join(DIRECTORY_NAME);
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&journal_dir).unwrap();
        for name in ["journal.sqlite", "journal.lock"] {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(0o600)
                .open(journal_dir.join(name))
                .unwrap();
        }
    }
    let swift_identity = "0F2C8C1E-6E7A-4F7B-9E39-6B1D2C3A4B5C";
    let live_lock = "1A2B3C4D-0000-4000-8000-00000000000A.operation";
    let target = fx.path("root/nested/target");
    let target_id = id(&target);
    let nested = fx.path("root/nested");
    let nested_id = id(&nested);
    {
        let conn = fx.raw_db();
        conn.execute_batch(SWIFT_V3).unwrap();
        conn.execute("INSERT INTO metadata VALUES(1, ?1)", [swift_identity]).unwrap();
        let root = bytes(&fx.path("root"));
        let ops = [
            (1, "00000001-0000-0000-0000-000000000000.operation", 1),
            (2, live_lock, 0),
            (3, "00000003-0000-0000-0000-000000000000.operation", 0),
        ];
        for (seq, lock, complete) in ops {
            conn.execute(
                "INSERT INTO operations VALUES(?1, ?2, '-2:5', ?3, ?4)",
                rusqlite::params![seq, root, lock, complete],
            )
            .unwrap();
        }
        // Darwin prints st_dev signed; Swift wrote it with `"\(st_dev):\(st_ino)"`.
        let swift_id = |id: FileIdentity| format!("{}:{}", id.dev as u32 as i32, id.ino);
        conn.execute(
            "INSERT INTO items VALUES(2, ?1, ?2, ?3, ?4)",
            rusqlite::params![bytes(&target), swift_id(target_id), bytes(&target), swift_id(nested_id)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO items VALUES(3, ?1, 'old-id', ?1, NULL)",
            rusqlite::params![bytes(&fx.path("root/nested/other"))],
        )
        .unwrap();
        // Simulate history that had a later operation rolled back: AUTOINCREMENT already handed out 7.
        conn.execute("UPDATE sqlite_sequence SET seq=7 WHERE name='operations'", []).unwrap();
    }
    // The Swift process that owns operation 2 is still running.
    let held =
        std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(journal_dir.join(live_lock)).unwrap();
    std::fs::set_permissions(journal_dir.join(live_lock), std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    assert!(
        orbit_platform::posix::try_flock(held.as_fd(), orbit_platform::posix::LockMode::Exclusive, &journal_dir)
            .unwrap()
    );

    let journal = fx.journal();
    let root = bytes(&fx.path("root"));
    journal
        .with_fence(&none(), |fence| {
            assert_eq!(fence.identity(), swift_identity, "saved Swift cursors must stay valid");
            assert_eq!(fence.high_water(), 3);
            let pending: Vec<_> = fence.pending_operations(0, 10)?.iter().map(|e| e.sequence).collect();
            assert_eq!(pending, [2, 3]);
            assert_eq!(fence.operations(0, 1)?[0].root_identity, FileIdentity { dev: u64::from(u32::MAX - 1), ino: 5 });
            assert!(fence.is_live(2)?, "a running Swift owner stays live across the migration");
            assert!(!fence.is_live(1).is_ok_and(|live| live), "missing lock files fail closed");
            let item = &fence.intersections(2, &root, None, 5)?[0];
            assert_eq!((item.relation, item.removal.identity), (Relation::Inside, target_id));
            let unknown = &fence.intersections(3, &root, None, 5)?[0];
            assert_eq!(unknown.relation, Relation::Unresolved, "unparseable identities force a full refresh");
            Ok(())
        })
        .unwrap();
    let conn = fx.raw_db();
    assert_eq!(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), schema::VERSION);
    drop(conn);
    let next = journal.begin(&root, id(&fx.path("root")), &[removal(&fx.path("root/nested/other"))], &none()).unwrap();
    assert_eq!(next.sequence(), 8, "sequences reserved by the Swift journal are never reissued");
    drop(held);
}

#[test]
fn unknown_journal_format_is_refused_untouched() {
    let fx = fixture();
    let journal = fx.journal();
    journal.with_fence(&none(), |_| Ok(())).unwrap();
    fx.raw_db().execute_batch("PRAGMA user_version=9").unwrap();
    assert!(matches!(journal.with_fence(&none(), |_| Ok(())), Err(JournalError::Incompatible(_))));
    let conn = fx.raw_db();
    assert_eq!(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 9);
}

// --- Deletion and removal runs --------------------------------------------------------------

#[test]
fn directory_deletion_unlinks_nested_symlinks_without_following_them() {
    let fx = fixture();
    write(&fx.path("outside/keep.txt"), b"keep");
    write(&fx.path("root/dir/sub/deep/file"), b"x");
    std::os::unix::fs::symlink(fx.path("outside"), fx.path("root/dir/sub/dir-link")).unwrap();
    std::os::unix::fs::symlink(fx.path("outside/keep.txt"), fx.path("root/dir/file-link")).unwrap();
    std::os::unix::fs::symlink(fx.path("missing"), fx.path("root/dir/dangling")).unwrap();
    let journal = fx.journal();
    let outcome =
        RemovalRun::new(&journal, bytes(&fx.path("root")), id(&fx.path("root")), vec![removal(&fx.path("root/dir"))])
            .execute(&mut NoFollowDelete, |_| None, &none());
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert!(!fx.path("root/dir").exists());
    assert_eq!(std::fs::read(fx.path("outside/keep.txt")).unwrap(), b"keep");
    // A top-level symlink item is itself unlinked, never its target.
    std::os::unix::fs::symlink(fx.path("outside"), fx.path("root/link")).unwrap();
    remove_no_follow(&bytes(&fx.path("root/link")), id(&fx.path("root/link")), None).unwrap();
    assert!(fx.path("outside/keep.txt").exists() && fx.path("root").exists());
}

#[test]
fn replaced_file_and_folder_are_refused_without_touching_either_version() {
    let fx = fixture();
    write(&fx.path("root/file"), b"old");
    write(&fx.path("root/folder/inner"), b"old");
    let journal = fx.journal();
    let collected = vec![removal(&fx.path("root/file")), removal(&fx.path("root/folder"))];
    std::fs::rename(fx.path("root/file"), fx.path("root/file-original")).unwrap();
    std::fs::rename(fx.path("root/folder"), fx.path("root/folder-original")).unwrap();
    write(&fx.path("root/file"), b"new");
    write(&fx.path("root/folder/inner"), b"new");
    let outcome = RemovalRun::new(&journal, bytes(&fx.path("root")), id(&fx.path("root")), collected).execute(
        &mut NoFollowDelete,
        |_| None,
        &none(),
    );
    assert!(outcome.removed.is_empty());
    assert_eq!(outcome.failures.len(), 2);
    assert!(outcome.failures.iter().all(|f| matches!(f.reason, FailureReason::ItemChanged)));
    assert!(outcome.changed_parents.contains(&bytes(&fx.path("root"))), "rejected paths are refreshed");
    for rel in ["root/file", "root/folder/inner", "root/file-original", "root/folder-original/inner"] {
        assert!(fx.path(rel).exists(), "{rel} was touched");
    }
    assert!(outcome.cursor.is_some());
}

#[test]
fn replaced_root_blocks_removal_even_when_item_identity_matches() {
    let fx = fixture();
    write(&fx.path("root/a"), b"a");
    write(&fx.path("root/b"), b"b");
    let journal = fx.journal();
    let root_id = id(&fx.path("root"));
    let items = vec![removal(&fx.path("root/a")), removal(&fx.path("root/b"))];
    std::fs::rename(fx.path("root"), fx.path("old-root")).unwrap();
    std::fs::create_dir(fx.path("root")).unwrap();
    std::fs::rename(fx.path("old-root/a"), fx.path("root/a")).unwrap();
    std::fs::rename(fx.path("old-root/b"), fx.path("root/b")).unwrap();
    let outcome = RemovalRun::new(&journal, bytes(&fx.path("root")), root_id, items).execute(
        &mut NoFollowDelete,
        |_| None,
        &none(),
    );
    assert!(outcome.root_changed);
    assert!(outcome.removed.is_empty());
    assert_eq!(outcome.failures.len(), 1, "the run stops at the first root check");
    assert!(fx.path("root/a").exists() && fx.path("root/b").exists());
}

#[test]
fn policy_rejection_and_parent_limit_are_reported() {
    let fx = fixture();
    for dir in ["a", "b", "c"] {
        write(&fx.path(&format!("root/{dir}/file")), b"x");
    }
    let journal = fx.journal();
    let items: Vec<_> = ["a", "b", "c"].iter().map(|d| removal(&fx.path(&format!("root/{d}/file")))).collect();
    let protected = bytes(&fx.path("root/b/file"));
    let mut trashed = Vec::new();
    let outcome =
        RemovalRun::new(&journal, bytes(&fx.path("root")), id(&fx.path("root")), items).with_parent_limit(1).execute(
            &mut TrashExecutor(|p: &Path| {
                trashed.push(p.to_path_buf());
                Ok(None)
            }),
            |path| (path == protected.as_slice()).then(|| "protected".to_string()),
            &none(),
        );
    assert_eq!(trashed.len(), 2);
    assert!(matches!(&outcome.failures[..], [RemovalFailure { reason: FailureReason::Policy(_), .. }]));
    assert_eq!(outcome.changed_parents.len(), 1);
    assert!(outcome.parents_overflow);
    assert_eq!(outcome.overlay.len(), 2);
    let completed = journal.with_fence(&none(), |f| f.operations(0, 1)).unwrap();
    assert!(completed[0].completed, "the operation completes after the last attempt");
}

/// A killed run is replayed: before the mutation the file survives the repair, after it the
/// indexed row is dropped. Either way the operation is abandoned, not live.
#[test]
fn killed_removal_run_is_reconciled_at_each_step_boundary() {
    use crate::recovery::fake::{FakeClock, FakeMap};
    use crate::recovery::{SavedRecovery, reconcile_saved_map};
    for (mode, survives) in [("run-before", true), ("run-after", false)] {
        let fx = fixture();
        write(&fx.path("root/dir/victim"), b"v");
        write(&fx.path("root/dir/keep"), b"k");
        let journal = fx.journal();
        let mut map = FakeMap::scan(&fx.path("root"));
        map.cursor = Some(
            journal
                .with_fence(&none(), |f| {
                    Ok(RemovalCursor { identity: f.identity().to_string(), sequence: f.high_water() })
                })
                .unwrap(),
        );
        let mut child = ChildProcess::spawn(mode, &fx.base);
        child.expect("ready");
        assert!(journal.with_fence(&none(), |f| f.is_live(1)).unwrap());
        child.kill();
        assert_eq!(fx.path("root/dir/victim").exists(), survives, "{mode}");
        let result = reconcile_saved_map(&journal, &mut map, &none(), &FakeClock::default()).unwrap();
        assert!(matches!(result, SavedRecovery::Published(ref c) if c.sequence == 1), "{mode}: {result:?}");
        assert_eq!(map.published.contains_key(&bytes(&fx.path("root/dir/victim"))), survives, "{mode}");
        assert!(map.published.contains_key(&bytes(&fx.path("root/dir/keep"))));
    }
}
