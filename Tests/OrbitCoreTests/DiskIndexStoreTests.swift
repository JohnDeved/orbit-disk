import CSQLite
import Foundation
import OrbitCore

final class DiskIndexStoreTests: CheckCase {
    private var fixture: URL!
    private var root: URL!
    private var store: DiskIndexStore!

    override func setUpWithError() throws {
        fixture = FileManager.default.temporaryDirectory.appendingPathComponent("OrbitIndexChecks-\(UUID().uuidString)")
        root = fixture.appendingPathComponent("files")
        store = DiskIndexStore(directory: fixture.appendingPathComponent("cache"))
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws { try FileManager.default.removeItem(at: fixture) }

    func testRoundTripAndLazyFolderQueries() throws {
        let wideURL = root.appendingPathComponent("wide")
        let wide = folder(wideURL, children: (0..<2_500).map { leaf(wideURL.appendingPathComponent("file-\($0)"), size: Int64($0)) })
        let empty = folder(root.appendingPathComponent("empty"), children: [])
        let result = scan(folder(root, children: [empty, wide]), skipped: 2, issues: ["one inaccessible item", "another"])
        let saved = try store.save(result, eventID: UInt64.max, eventIdentity: "volume:日本語")
        let loaded = try checkUnwrap(store.savedSnapshot(for: root))
        checkEqual(loaded.id, saved.id)
        checkEqual(loaded.eventID, UInt64.max)
        checkEqual(loaded.eventIdentity, "volume:日本語")
        checkEqual(loaded.skippedCount, 2)
        checkEqual(loaded.issues, result.issues)
        let overview = try store.loadFolder(path: root.path, snapshot: loaded)
        checkEqual(overview.size, result.root.size)
        checkEqual(overview.fileIdentity, result.root.fileIdentity)
        checkEqual(overview.rawAllocatedSize, 0)
        checkEqual(overview.fileCount, 2_500)
        checkEqual(overview.children.map(\.name), ["wide", "empty"])
        checkLessThanOrEqual(overview.children[0].children.count, 96)
        let wideView = try store.loadFolder(path: wideURL.path, snapshot: loaded)
        checkEqual(wideView.children.count, 2_500)
        checkEqual(wideView.fileIdentity, wide.fileIdentity)
        checkEqual(wideView.children.first?.size, 2_499)
        checkEqual(try store.node(path: empty.id, snapshot: loaded)?.size, 0)

        let slash = URL(fileURLWithPath: "/")
        let rootSnapshot = try store.save(scan(folder(slash, children: [folder(URL(fileURLWithPath: "/Users"), children: [])])), eventID: 0, eventIdentity: "root")
        checkEqual(try store.loadFolder(path: "/", snapshot: rootSnapshot).children.first?.name, "Users")
        checkTrue(rootSnapshot.databaseURL != saved.databaseURL)
    }

    func testLiteralSearchAndAncestors() throws {
        let directory = root.appendingPathComponent("sub%_folder")
        let unicode = leaf(directory.appendingPathComponent("Résumé_100%.txt"), size: 80)
        let unrelated = leaf(root.appendingPathComponent("resumeX100anything.txt"), size: 100)
        let snapshot = try store.save(scan(folder(root, children: [folder(directory, children: [unicode]), unrelated])), eventID: 5, eventIdentity: "v")
        checkEqual(try store.search(query: "RÉSUMÉ_100%", snapshot: snapshot).nodes.map(\.id), [unicode.id])
        checkEqual(try store.search(query: "%", snapshot: snapshot).totalCount, 2)
        checkEqual(try store.search(query: "resume", snapshot: snapshot, limit: 1).nodes.map(\.id), [unrelated.id])
        checkEqual(try store.search(query: "resume", snapshot: snapshot, limit: 1).totalCount, 2)
        checkEqual(try store.ancestors(path: unicode.id, snapshot: snapshot).map(\.id), [root.path, directory.path, unicode.id])
        let cancellation = ScanCancellation(); cancellation.cancel()
        checkThrowsError(try store.search(query: "resume", snapshot: snapshot, cancellation: cancellation)) { checkTrue($0 is CancellationError) }
    }

    func testCancellationFailureAndInvalidCachesPreserveSnapshot() throws {
        let file = leaf(root.appendingPathComponent("item"), size: 11)
        let initial = try store.save(scan(folder(root, children: [file])), eventID: 40, eventIdentity: "original")
        let cancelled = ScanCancellation(); cancelled.cancel()
        checkThrowsError(try store.save(scan(folder(root, children: [])), eventID: 41, eventIdentity: "cancelled", cancellation: cancelled))
        checkEqual(store.savedSnapshot(for: root)?.id, initial.id)
        checkThrowsError(try store.save(scan(folder(root, children: [file, file])), eventID: 42, eventIdentity: "invalid"))
        checkEqual(store.savedSnapshot(for: root)?.id, initial.id)
        let many = (0..<20_000).map { leaf(root.appendingPathComponent("cancel-\($0)"), size: 1) }
        let interrupted = ScanCancellation()
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.01) { interrupted.cancel() }
        checkThrowsError(try store.save(scan(folder(root, children: many)), eventID: 42, eventIdentity: "interrupted", cancellation: interrupted)) {
            checkTrue($0 is CancellationError)
        }
        checkEqual(store.savedSnapshot(for: root)?.id, initial.id)
        checkThrowsError(try store.update(snapshot: initial, replacements: [IndexReplacement(path: file.id, result: nil)], eventID: 43, eventIdentity: "cancelled", cancellation: cancelled))
        checkEqual(try store.node(path: file.id, snapshot: initial)?.size, 11)
        let wrongRoot = leaf(root.appendingPathComponent("wrong"), size: 99)
        checkThrowsError(try store.update(snapshot: initial, replacements: [IndexReplacement(path: file.id, result: scan(wrongRoot))], eventID: 43, eventIdentity: "failed"))
        checkEqual(store.savedSnapshot(for: root)?.id, initial.id)
        checkEqual(try store.node(path: file.id, snapshot: initial)?.size, 11)
        let partial = scan(folder(root, children: []), skipped: 1, issues: ["permission denied"])
        checkThrowsError(try store.update(snapshot: initial, replacements: [IndexReplacement(path: root.path, result: partial)], eventID: 43, eventIdentity: "partial"))
        checkEqual(store.savedSnapshot(for: root)?.eventID, 40)
        checkEqual(try store.node(path: file.id, snapshot: initial)?.size, 11)
        let next = try store.save(scan(folder(root, children: [])), eventID: 44, eventIdentity: "next")
        checkThrowsError(try store.loadFolder(path: root.path, snapshot: initial))
        checkThrowsError(try store.update(snapshot: initial, replacements: [], eventID: 45, eventIdentity: "stale"))
        checkEqual(store.savedSnapshot(for: root)?.id, next.id)
        try mutate(next.databaseURL, sql: "PRAGMA user_version=999")
        checkNil(store.savedSnapshot(for: root))
        try mutate(next.databaseURL, sql: "PRAGMA user_version=2")
        checkNil(store.savedSnapshot(for: root), "Older caches without directory identities must be rebuilt")
        try mutate(next.databaseURL, sql: "PRAGMA user_version=3; UPDATE metadata SET root_path='/wrong'")
        checkNil(store.savedSnapshot(for: root))
        try Data("damaged".utf8).write(to: next.databaseURL)
        checkNil(store.savedSnapshot(for: root))
        let repaired = try store.save(scan(folder(root, children: [file])), eventID: 45, eventIdentity: "repaired")
        checkEqual(store.savedSnapshot(for: root)?.id, repaired.id)
        try store.invalidate(for: root)
        checkNil(store.savedSnapshot(for: root))
    }

    func testIncrementalCreateDeleteRenameAndTotals() throws {
        let changedURL = root.appendingPathComponent("changes%_")
        let siblingURL = root.appendingPathComponent("changesXY")
        let original = leaf(changedURL.appendingPathComponent("old"), size: 10)
        let untouched = leaf(siblingURL.appendingPathComponent("preserve"), size: 17)
        let initial = try store.save(scan(folder(root, children: [folder(changedURL, children: [original]), folder(siblingURL, children: [untouched])])), eventID: 1, eventIdentity: "v")
        let added = leaf(changedURL.appendingPathComponent("added"), size: 30)
        let created = try store.update(snapshot: initial, replacements: [IndexReplacement(path: changedURL.path, result: scan(folder(changedURL, children: [original, added])))], eventID: 2, eventIdentity: "v")
        checkEqual(try store.node(path: root.path, snapshot: created)?.size, 57)
        checkEqual(try store.node(path: root.path, snapshot: created)?.allocatedSize, 57)
        checkEqual(try store.node(path: changedURL.path, snapshot: created)?.allocatedSize, 40)
        checkEqual(try store.node(path: changedURL.path, snapshot: created)?.fileIdentity, "directory:\(changedURL.path)")
        checkEqual(try store.node(path: root.path, snapshot: created)?.fileCount, 3)
        checkEqual(try store.node(path: untouched.id, snapshot: created)?.size, 17)
        let renamed = leaf(changedURL.appendingPathComponent("renamed"), size: 30)
        let moved = try store.update(snapshot: created, replacements: [IndexReplacement(path: added.id, result: nil), IndexReplacement(path: renamed.id, result: scan(renamed))], eventID: 3, eventIdentity: "v")
        checkNil(try store.node(path: added.id, snapshot: moved))
        checkEqual(try store.node(path: renamed.id, snapshot: moved)?.size, 30)
        checkEqual(try store.node(path: root.path, snapshot: moved)?.allocatedSize, 57)
        let removed = try store.update(snapshot: moved, replacements: [IndexReplacement(path: changedURL.path, result: nil)], eventID: 4, eventIdentity: "v")
        checkEqual(try store.node(path: root.path, snapshot: removed)?.size, 17)
        checkEqual(try store.node(path: root.path, snapshot: removed)?.allocatedSize, 17)
        checkEqual(try store.node(path: root.path, snapshot: removed)?.fileCount, 1)
        checkEqual(try store.node(path: untouched.id, snapshot: removed)?.size, 17)
        checkEqual(removed.eventID, 4)
        checkThrowsError(try store.update(snapshot: removed, replacements: [IndexReplacement(path: root.path, result: nil)], eventID: 5, eventIdentity: "v"))
        checkEqual(store.savedSnapshot(for: root)?.id, removed.id)
    }

    func testIncrementalCrossSubtreeHardLinks() throws {
        let a = root.appendingPathComponent("a")
        let z = root.appendingPathComponent("z")
        let original = leaf(z.appendingPathComponent("original"), size: 4096, identity: "device:inode")
        let initial = try store.save(scan(folder(root, children: [folder(a, children: []), folder(z, children: [original])])), eventID: 1, eventIdentity: "v")
        let newLink = leaf(a.appendingPathComponent("link"), size: 4096, identity: "device:inode")
        let linked = try store.update(snapshot: initial, replacements: [IndexReplacement(path: a.path, result: scan(folder(a, children: [newLink])))], eventID: 2, eventIdentity: "v")
        checkEqual(try store.node(path: root.path, snapshot: linked)?.size, 8192)
        checkEqual(try store.node(path: root.path, snapshot: linked)?.allocatedSize, 4096)
        checkEqual(try store.node(path: original.id, snapshot: linked)?.allocatedSize, 0)
        checkEqual(try store.node(path: newLink.id, snapshot: linked)?.allocatedSize, 4096)
        checkEqual(try store.node(path: z.path, snapshot: linked)?.allocatedSize, 0)
        let removed = try store.update(snapshot: linked, replacements: [IndexReplacement(path: a.path, result: nil)], eventID: 3, eventIdentity: "v")
        checkEqual(try store.node(path: original.id, snapshot: removed)?.allocatedSize, 4096)
        checkEqual(try store.node(path: z.path, snapshot: removed)?.allocatedSize, 4096)
        checkEqual(try store.node(path: root.path, snapshot: removed)?.allocatedSize, 4096)
    }

    private func leaf(_ url: URL, size: Int64, identity: String? = nil) -> DiskNode {
        DiskNode(url: url, name: url.lastPathComponent, isDirectory: false, size: size, allocatedSize: size,
                 fileCount: 1, rawAllocatedSize: size, fileIdentity: identity)
    }
    private func folder(_ url: URL, children: [DiskNode]) -> DiskNode {
        DiskNode(url: url, name: url.lastPathComponent, isDirectory: true,
                 size: children.reduce(0) { $0 + $1.size }, allocatedSize: children.reduce(0) { $0 + $1.allocatedSize },
                 fileCount: children.reduce(0) { $0 + $1.fileCount }, children: children,
                 fileIdentity: "directory:\(url.path)")
    }
    private func scan(_ root: DiskNode, skipped: Int = 0, issues: [String] = []) -> ScanResult {
        ScanResult(root: root, elapsed: 0.125, skippedCount: skipped, issues: issues)
    }
    private func mutate(_ url: URL, sql: String) throws {
        var database: OpaquePointer?
        guard sqlite3_open(url.path, &database) == SQLITE_OK else { throw CheckFailure("Could not open fixture database") }
        defer { sqlite3_close(database) }
        guard sqlite3_exec(database, sql, nil, nil, nil) == SQLITE_OK else { throw CheckFailure("Could not alter fixture metadata") }
    }
}
