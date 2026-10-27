import CSQLite
import Darwin
import Foundation

private final class Database {
    private var handle: OpaquePointer?
    init(_ path: String) {
        precondition(sqlite3_open_v2(path, &handle, SQLITE_OPEN_READONLY, nil) == SQLITE_OK)
    }
    deinit { sqlite3_close(handle) }
    func scalar(_ sql: String) -> Int64 {
        var statement: OpaquePointer?
        precondition(sqlite3_prepare_v2(handle, sql, -1, &statement, nil) == SQLITE_OK)
        defer { sqlite3_finalize(statement) }
        precondition(sqlite3_step(statement) == SQLITE_ROW)
        return sqlite3_column_int64(statement, 0)
    }
}

private final class ListingOutcome: @unchecked Sendable {
    private let lock = NSLock()
    private var error: Error?
    func set(_ value: Error) { lock.lock(); error = value; lock.unlock() }
    func get() -> Error? { lock.lock(); defer { lock.unlock() }; return error }
}

enum ListingEpochChecks {
    static let fm = FileManager.default
    static func makeStage(_ base: URL, cancellation: ScanCancellation = ScanCancellation()) throws -> ResumableStage {
        let root = base.appendingPathComponent("root", isDirectory: true)
        try fm.createDirectory(at: root, withIntermediateDirectories: true)
        return try ResumableStage(cache: base.appendingPathComponent("cache"), root: root,
                                  current: StageCheckpoint(rootPath: root.path, identity: "fixture", eventID: 0),
                                  cancellation: cancellation)
    }
    static func finish(_ stage: ResumableStage) throws {
        while try stage.advance(progress: { _ in }) {}
    }
    static func crashChild(_ base: URL, boundary: String) throws {
        let stage = try makeStage(base)
        ListingStage.testListingCommit = { observed in
            if observed == boundary { kill(getpid(), SIGKILL) }
        }
        _ = try stage.advance(progress: { _ in })
        fatalError("missed crash boundary")
    }
    static func run(_ base: URL) throws {
        try checkCrashes(base.appendingPathComponent("crashes"))
        try checkRollback(base.appendingPathComponent("rollback"))
        try checkEmptyRelisting(base.appendingPathComponent("relist"))
        try checkStaleFinish(base.appendingPathComponent("stale"))
        try checkFallback(base.appendingPathComponent("fallback"))
        try checkActiveListing(base.appendingPathComponent("active"))
        try checkBoundedBatches(base.appendingPathComponent("batches"))
        try checkInterruptedCompactPrune(base.appendingPathComponent("compact-prune"))
    }
    static func checkCrashes(_ base: URL) throws {
        for boundary in ["batch-epoch", "batch-committed", "empty-epoch", "empty-committed"] {
            let fixture = base.appendingPathComponent(boundary)
            let root = fixture.appendingPathComponent("root", isDirectory: true)
            try fm.createDirectory(at: root, withIntermediateDirectories: true)
            let nonempty = boundary.hasPrefix("batch")
            if nonempty { try Data([1, 2, 3]).write(to: root.appendingPathComponent("file")) }
            let child = Process()
            child.executableURL = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
            child.arguments = ["listing-epoch-child", fixture.path, boundary]
            try child.run(); child.waitUntilExit()
            precondition(child.terminationReason == .uncaughtSignal && child.terminationStatus == SIGKILL, "crash child missed boundary")
            let stage = try makeStage(fixture)
            let database = Database(stage.crawler.store.databasePath)
            let committed = boundary.hasSuffix("committed")
            precondition(database.scalar("SELECT COUNT(*) FROM epochs") == (committed ? 1 : 0), "epoch committed without matching listing work")
            precondition(database.scalar("SELECT COUNT(*) FROM trim") == (committed && !nonempty ? 1 : 0), "empty listing trim was not atomic")
            precondition(database.scalar("SELECT COUNT(*) FROM nodes") == (committed && nonempty ? 2 : 1), "first listing batch was not atomic")
            precondition(database.scalar("SELECT COUNT(*) FROM directories WHERE pending=1") == 1, "crash lost pending work")
            try finish(stage)
            let view = try stage.view(path: root.path, removals: [], cancellation: ScanCancellation())!
            precondition(view.folder.scanState == .complete && view.folder.fileCount == (nonempty ? 1 : 0))
            precondition(view.folder.size == (nonempty ? 3 : 0))
        }
        print("PASS: SIGKILL before and after first-batch and empty-listing commits preserves queue and resumes")
    }
    static func checkRollback(_ base: URL) throws {
        let stage = try makeStage(base), store = stage.crawler.store
        let parent = stage.crawler.rootPath, device = stage.crawler.root.identity.device
        let database = Database(store.databasePath)
        func directory(_ name: String, _ inode: UInt64) -> FileMetadata {
            FileMetadata(name: name, device: device, inode: inode, isDirectory: true, isSymbolicLink: false,
                         isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0)
        }
        for (name, expectedEpoch) in [("first", Int64(0)), ("second", Int64(1))] {
            do {
                try store.append(parent: parent, epoch: nil, records: [directory(name, 1), directory(name, 2)], rootDevice: device)
                fatalError("conflicting directory identities accepted")
            } catch ListingStage.Failure.sqlite {}
            precondition(database.scalar("SELECT COALESCE(MAX(value),0) FROM epochs") == expectedEpoch, "failed batch advanced epoch")
            let registered = try store.directory(path: Data((stage.crawler.rootURL.path + "/" + name).utf8))
            precondition(registered == nil, "failed batch retained directory")
            if expectedEpoch == 0 {
                let epoch = try store.append(parent: parent, epoch: nil, records: [directory("kept", 3)], rootDevice: device)
                precondition(epoch == 1)
            }
        }
        print("PASS: failed initial and replacement batches roll back epoch and all rows")
    }
    static func checkEmptyRelisting(_ base: URL) throws {
        let root = base.appendingPathComponent("root", isDirectory: true)
        let child = root.appendingPathComponent("child", isDirectory: true)
        try fm.createDirectory(at: child.appendingPathComponent("nested"), withIntermediateDirectories: true)
        let owner = child.appendingPathComponent("owner")
        try Data(repeating: 7, count: 8192).write(to: owner)
        try fm.linkItem(at: owner, to: root.appendingPathComponent("surviving-link"))
        try Data([1, 2, 3]).write(to: child.appendingPathComponent("nested/file"))
        let stage = try makeStage(base)
        try finish(stage)
        for url in try fm.contentsOfDirectory(at: child, includingPropertiesForKeys: nil) { try fm.removeItem(at: url) }
        try stage.crawler.markChanged(directory: child.path)
        try finish(stage)
        let actual = try stage.view(path: root.path, removals: [], cancellation: ScanCancellation())!.folder
        let expected = try DiskScanner().scan(root).root
        precondition(actual.size == expected.size && actual.allocatedSize == expected.allocatedSize && actual.fileCount == expected.fileCount)
        let empty = try stage.view(path: child.path, removals: [], cancellation: ScanCancellation())!.folder
        precondition(empty.children.isEmpty && empty.scanState == .complete && empty.size == 0 && empty.allocatedSize == 0)
        print("PASS: empty relisting trims prior files and subfolders and reassigns surviving hard links")
    }
    static func checkStaleFinish(_ base: URL) throws {
        let stage = try makeStage(base), store = stage.crawler.store, parent = stage.crawler.rootPath
        let database = Database(store.databasePath)
        try store.markDirty(parent: parent)
        let stale = try store.finish(parent: parent, epoch: nil, expectedRevision: 0, expectedFullGeneration: 0)
        precondition(!stale && database.scalar("SELECT COUNT(*) FROM epochs") == 0 && database.scalar("SELECT COUNT(*) FROM trim") == 0)
        let revision = try store.dirtyRevision(parent: parent)
        try store.markFullRefresh()
        let staleGeneration = try store.finish(parent: parent, epoch: nil, expectedRevision: revision, expectedFullGeneration: 0)
        precondition(!staleGeneration && database.scalar("SELECT COUNT(*) FROM epochs") == 0 && database.scalar("SELECT COUNT(*) FROM trim") == 0)
        try finish(stage)
        let complete = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())!
        precondition(complete.folder.scanState == .complete)
        print("PASS: stale revision and full-refresh generation reject empty completion without minting an epoch")
    }
    static func checkFallback(_ base: URL) throws {
        let stage = try makeStage(base), root = stage.crawler.rootURL
        for index in 0..<256 { try Data([1]).write(to: root.appendingPathComponent("file-\(index)")) }
        var committed = false
        stage.crawler.testFallbackAfterFirstBuffer = {
            let db = Database(stage.crawler.store.databasePath)
            committed = db.scalar("SELECT COUNT(*) FROM nodes WHERE parent IS NOT NULL") >= 128
            for file in try! fm.contentsOfDirectory(at: root, includingPropertiesForKeys: nil) { try! fm.removeItem(at: file) }
        }
        try finish(stage)
        precondition(committed, "fallback test did not observe first durable batch")
        let view = try stage.view(path: root.path, removals: [], cancellation: ScanCancellation())!.folder
        precondition(view.scanState == .complete && view.children.isEmpty && view.size == 0 && view.allocatedSize == 0)
        let database = Database(stage.crawler.store.databasePath)
        precondition(database.scalar("SELECT MAX(value) FROM epochs") == 2, "empty fallback reused provisional epoch")
        print("PASS: empty fallback after a committed bulk batch removes all provisional rows")
    }
    static func checkActiveListing(_ base: URL) throws {
        let root = base.appendingPathComponent("root", isDirectory: true)
        for name in ["waiting-a", "waiting-b"] { try fm.createDirectory(at: root.appendingPathComponent(name), withIntermediateDirectories: true) }
        let cancellation = ScanCancellation(), stage = try makeStage(base, cancellation: cancellation)
        while true {
            let advanced = try stage.advance(progress: { _ in })
            precondition(advanced)
            let view = try stage.view(path: root.path, removals: [], cancellation: ScanCancellation())!
            if view.folder.scanState == .complete { break }
        }
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0), done = DispatchSemaphore(value: 0)
        let outcome = ListingOutcome(), before = stage.revision
        stage.crawler.testBeforeListing = { entered.signal(); release.wait() }
        DispatchQueue.global().async {
            defer { done.signal() }
            do { _ = try stage.advance(progress: { _ in }) } catch { outcome.set(error) }
        }
        precondition(entered.wait(timeout: .now() + 5) == .success)
        let active = try stage.view(path: root.path, removals: [], cancellation: ScanCancellation())!
        let scanning = active.folder.children.filter { $0.scanState == .scanning }
        precondition(scanning.count == 1 && active.folder.children.filter { $0.scanState == .pending }.count == 1)
        precondition(stage.revision > before)
        let focused = try stage.view(path: scanning[0].id, removals: [], cancellation: ScanCancellation())!
        precondition(focused.folder.scanState == .scanning && focused.isListing)
        let search = try stage.search(query: "waiting", cancellation: ScanCancellation(), removals: [], limit: 10)
        precondition(search.nodes.filter { $0.scanState == .scanning }.count == 1)
        let reader = DiskIndexStore(directory: base.appendingPathComponent("cache"))
        let wrongIdentity = try reader.stagedView(path: root.path, snapshot: stage.testPreviewSnapshot, revision: 0,
                                                  activeDirectory: (Data(scanning[0].id.utf8), "replaced:identity"))!
        precondition(wrongIdentity.folder.children.allSatisfy { $0.scanState == .pending })
        let wrongPath = try reader.stagedSearch(query: "waiting", snapshot: stage.testPreviewSnapshot, revision: 0,
                                                activeDirectory: (Data((scanning[0].id + "-other").utf8), scanning[0].fileIdentity!),
                                                removals: [], cancellation: ScanCancellation())
        precondition(wrongPath.nodes.allSatisfy { $0.scanState == .pending })
        let completedRoot = try reader.stagedView(path: root.path, snapshot: stage.testPreviewSnapshot, revision: 0,
                                                  activeDirectory: (Data(root.path.utf8), active.folder.fileIdentity!))!
        precondition(completedRoot.folder.scanState == .complete)
        cancellation.cancel(); release.signal()
        precondition(done.wait(timeout: .now() + 5) == .success && outcome.get() is CancellationError)
        let stopped = try stage.view(path: root.path, removals: [], cancellation: ScanCancellation())!
        precondition(stopped.folder.children.allSatisfy { $0.scanState == .pending })
        print("PASS: active folder is visible in folder and search views before the first commit; cancellation clears it")
    }

    static func checkBoundedBatches(_ base: URL) throws {
        let root = base.appendingPathComponent("root", isDirectory: true)
        try fm.createDirectory(at: root, withIntermediateDirectories: true)
        for index in 0..<4097 { try Data([1]).write(to: root.appendingPathComponent("file-\(index)")) }
        var savedCount: Int64 = 0
        do {
            let stage = try makeStage(base), database = Database(stage.crawler.store.databasePath)
            var counts: [Int64] = []
            do {
                _ = try stage.crawler.step(maxBatches: 2, onBatch: { _, _ in
                    counts.append(database.scalar("SELECT COUNT(*) FROM nodes WHERE parent IS NOT NULL"))
                })
                fatalError("missing batch interruption")
            } catch DurableCrawler.Interrupt.afterBatch {}
            precondition(counts.count == 2 && counts[0] == 128)
            precondition(counts[1] > counts[0] && counts[1] - counts[0] <= 1024)
            precondition(database.scalar("SELECT COUNT(*) FROM directories WHERE pending=1") == 1)
            precondition(database.scalar("SELECT COUNT(*) FROM pragma_table_info('nodes') WHERE name IN ('path','st_path','st_parent','st_name','st_size','st_allocated','st_is_link','folded','identity','file_identity','directory','charged')") == 0)
            precondition(database.scalar("PRAGMA user_version") == 4)
            precondition(database.scalar("SELECT COUNT(*) FROM directory_paths") == 1, "leaf paths must not be persisted")
            precondition(database.scalar("SELECT COUNT(*) FROM directory_paths p JOIN nodes n ON n.id=p.id WHERE n.is_dir=0") == 0)
            savedCount = counts[1]
        }
        let stage = try makeStage(base), database = Database(stage.crawler.store.databasePath)
        precondition(database.scalar("SELECT COUNT(*) FROM nodes WHERE parent IS NOT NULL") == savedCount)
        try finish(stage)
        let view = try stage.view(path: root.path, removals: [], cancellation: ScanCancellation())!.folder
        precondition(view.scanState == .complete && view.fileCount == 4097 && view.size == 4097)
        precondition(view.children.count == 4097)
        print("PASS: first 128 rows are durable immediately; bounded background batches survive reopen without losing rows")
    }

    static func checkInterruptedCompactPrune(_ base: URL) throws {
        let root = base.appendingPathComponent("root", isDirectory: true)
        let removed = root.appendingPathComponent("a-remove", isDirectory: true)
        let deepest = removed.appendingPathComponent("nested/さらに", isDirectory: true)
        let sibling = root.appendingPathComponent("a-remove-other", isDirectory: true)
        try fm.createDirectory(at: deepest, withIntermediateDirectories: true)
        try fm.createDirectory(at: sibling, withIntermediateDirectories: true)
        try Data([4, 5]).write(to: sibling.appendingPathComponent("kept"))
        for index in 0..<320 {
            try Data(repeating: 1, count: index == 0 ? 4096 : 3)
                .write(to: deepest.appendingPathComponent(String(format: "file-%03d", index)))
        }
        let survivor = root.appendingPathComponent("z-surviving-link")
        try fm.linkItem(at: deepest.appendingPathComponent("file-000"), to: survivor)
        var remaining: Int64 = 0
        do {
            let stage = try makeStage(base)
            try finish(stage)
            let database = Database(stage.crawler.store.databasePath)
            let before = database.scalar("SELECT COUNT(*) FROM nodes")
            try fm.removeItem(at: removed)
            try stage.crawler.markChanged(directory: root.path)
            try stage.crawler.store.startPrune(path: Data(removed.path.utf8), parent: Data(root.path.utf8), name: Data(removed.lastPathComponent.utf8))
            let advanced = try stage.crawler.store.pruneOneChunk()
            precondition(advanced)
            remaining = database.scalar("SELECT COUNT(*) FROM nodes")
            precondition(remaining < before && remaining > 128, "fixture must interrupt a multi-batch prune")
            precondition(database.scalar("SELECT COUNT(*) FROM prune") == 1)
            precondition(database.scalar("SELECT COUNT(*) FROM nodes n LEFT JOIN nodes p ON p.id=n.parent WHERE n.parent IS NOT NULL AND p.id IS NULL") == 0, "pruning orphaned live descendants")
            precondition(database.scalar("SELECT COUNT(*) FROM nodes n LEFT JOIN directory_paths p ON p.id=n.parent WHERE n.parent IS NOT NULL AND p.id IS NULL") == 0, "pruning removed a parent path before its children")
        }
        let stage = try makeStage(base), database = Database(stage.crawler.store.databasePath)
        precondition(database.scalar("SELECT COUNT(*) FROM nodes") == remaining, "reopen lost committed pruning progress")
        precondition(database.scalar("SELECT COUNT(*) FROM prune") == 1, "reopen lost the prune continuation")
        try finish(stage)
        let actual = try stage.view(path: root.path, removals: [], cancellation: ScanCancellation())!.folder
        let expected = try DiskScanner().scan(root).root
        precondition(actual.size == expected.size && actual.allocatedSize == expected.allocatedSize && actual.fileCount == expected.fileCount)
        precondition(actual.children.map(\.id) == expected.children.map(\.id))
        precondition(actual.children.first { $0.id == survivor.path }?.allocatedSize == expected.children.first { $0.id == survivor.path }?.allocatedSize)
        let siblingView = try stage.view(path: sibling.path, removals: [], cancellation: ScanCancellation())
        precondition(siblingView!.folder.children.count == 1)
        precondition(database.scalar("SELECT COUNT(*) FROM prune") == 0)
        precondition(database.scalar("SELECT COUNT(*) FROM directory_paths") == database.scalar("SELECT COUNT(*) FROM nodes WHERE is_dir=1"))
        precondition(database.scalar("SELECT COUNT(*) FROM directory_paths p LEFT JOIN nodes n ON n.id=p.id WHERE n.id IS NULL OR n.is_dir=0") == 0)
        precondition(database.scalar("SELECT COUNT(*) FROM owners o LEFT JOIN nodes n ON n.id=o.node_id WHERE n.id IS NULL") == 0)
        print("PASS: compact multi-batch pruning resumes with parent paths, sibling boundaries and surviving hard-link ownership intact")
    }
}
