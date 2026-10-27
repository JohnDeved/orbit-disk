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

    func testReplacementPathValidation() throws {
        let token = ScanCancellation()
        let paths = (0..<10_000).map { "/fixture/folder-\($0)" }
        try DiskIndexStore.validateReplacementPaths(paths, root: "/fixture", cancellation: token)
        try DiskIndexStore.validateReplacementPaths(["/fixture/a", "/fixture/a-b", "/fixture/a0"], root: "/fixture", cancellation: token)
        try DiskIndexStore.validateReplacementPaths(["/fixture/a", "/fixture/a/\u{0301}child"], root: "/fixture", cancellation: token)
        for overlapping in [
            ["/fixture/a", "/fixture/a"],
            ["/fixture/a/child", "/fixture/a"],
            ["/fixture/a", "/fixture/a/child"],
            ["/fixture/é", "/fixture/e\u{301}/child"],
            ["/", "/fixture"]
        ] {
            checkThrowsError(try DiskIndexStore.validateReplacementPaths(overlapping, root: "/", cancellation: token))
        }
        checkThrowsError(try DiskIndexStore.validateReplacementPaths(["/fixture-other/file"], root: "/fixture", cancellation: token))
        token.cancel()
        checkThrowsError(try DiskIndexStore.validateReplacementPaths([], root: "/fixture", cancellation: token)) { checkTrue($0 is CancellationError) }
        checkThrowsError(try DiskIndexStore.validateReplacementPaths(paths, root: "/fixture", cancellation: token)) { checkTrue($0 is CancellationError) }

        let a = folder(root.appendingPathComponent("a"), children: [leaf(root.appendingPathComponent("a/child"), size: 1)])
        let saved = try store.save(scan(folder(root, children: [a])), eventID: 1, eventIdentity: "volume")
        let replacements = [IndexReplacement(path: a.id, result: nil), IndexReplacement(path: a.children[0].id, result: nil)]
        checkThrowsError(try store.update(snapshot: saved, replacements: replacements, eventID: 2, eventIdentity: "volume"))
        checkEqual(store.savedSnapshot(for: root)?.id, saved.id)
    }

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
        let cancelled = ScanCancellation(); cancelled.cancel()
        checkThrowsError(try store.loadFolder(path: wideURL.path, snapshot: loaded, cancellation: cancelled)) { checkTrue($0 is CancellationError) }
        checkThrowsError(try store.node(path: empty.id, snapshot: loaded, cancellation: cancelled)) { checkTrue($0 is CancellationError) }
        checkThrowsError(try store.ancestors(path: empty.id, snapshot: loaded, cancellation: cancelled)) { checkTrue($0 is CancellationError) }
        let active = ScanCancellation()
        checkEqual(try store.loadFolder(path: wideURL.path, snapshot: loaded, cancellation: active).children.count, 2_500)
        checkEqual(try store.node(path: empty.id, snapshot: loaded, cancellation: active)?.id, empty.id)
        checkEqual(try store.ancestors(path: empty.id, snapshot: loaded, cancellation: active).map(\.id), [root.path, empty.id])

        let slash = URL(fileURLWithPath: "/")
        let rootSnapshot = try store.save(scan(folder(slash, children: [folder(URL(fileURLWithPath: "/Users"), children: [])])), eventID: 0, eventIdentity: "root")
        checkEqual(try store.loadFolder(path: "/", snapshot: rootSnapshot).children.first?.name, "Users")
        checkTrue(rootSnapshot.databaseURL != saved.databaseURL)
    }

    func testParentValidationPreservesSnapshot() throws {
        let leading = leaf(root.appendingPathComponent("\u{0301}leading.bin"), size: 4)
        let decomposed = root.appendingPathComponent("e\u{0301}", isDirectory: true)
        let composed = root.appendingPathComponent("é", isDirectory: true)
        let child = leaf(composed.appendingPathComponent("child.bin"), size: 8)
        let initial = try store.save(scan(folder(root, children: [leading, folder(decomposed, children: [child])])),
                                     eventID: 1, eventIdentity: "v")
        let wrongDepth = leaf(root.appendingPathComponent("other/wrong.bin"), size: 1)
        checkThrowsError(try store.save(scan(folder(root, children: [wrongDepth])), eventID: 2, eventIdentity: "v"))
        checkEqual(store.savedSnapshot(for: root)?.id, initial.id)
        checkEqual(try store.node(path: leading.id, snapshot: initial)?.size, 4)
        let sibling = leaf(URL(fileURLWithPath: root.path + "-sibling/wrong.bin"), size: 1)
        checkThrowsError(try store.save(scan(folder(root, children: [sibling])), eventID: 3, eventIdentity: "v"))
        checkEqual(store.savedSnapshot(for: root)?.id, initial.id)
        checkEqual(try store.node(path: leading.id, snapshot: initial)?.size, 4)

        let cwd = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
        let relative = leaf(URL(string: "relative-child.bin")!, size: 5)
        let relativeSnapshot = try store.save(scan(folder(cwd, children: [relative])), eventID: 4, eventIdentity: "v")
        checkEqual(relativeSnapshot.rootURL.path, cwd.path)
        let slashRoot = URL(fileURLWithPath: "/a", isDirectory: true)
        let repeatedSlash = leaf(URL(string: "/a//child.bin")!, size: 6)
        let repeatedSnapshot = try store.save(scan(folder(slashRoot, children: [repeatedSlash])), eventID: 5, eventIdentity: "v")
        checkEqual(repeatedSnapshot.rootURL.path, slashRoot.path)
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
        let absent = try store.search(query: "no such filename", snapshot: snapshot)
        checkTrue(absent.nodes.isEmpty)
        checkEqual(absent.totalCount, 0)
        for limit in [0, -1] {
            let limited = try store.search(query: "resume", snapshot: snapshot, limit: limit)
            checkTrue(limited.nodes.isEmpty)
            checkEqual(limited.totalCount, 2)
        }
        checkEqual(try store.ancestors(path: unicode.id, snapshot: snapshot).map(\.id), [root.path, directory.path, unicode.id])
        let cancellation = ScanCancellation(); cancellation.cancel()
        checkThrowsError(try store.search(query: "resume", snapshot: snapshot, cancellation: cancellation)) { checkTrue($0 is CancellationError) }
    }

    func testPreviewReusesQueryAcrossEmptyAndNestedFolders() throws {
        let emptyFolders = (0..<500).map { folder(root.appendingPathComponent("empty-\($0)"), children: []) }
        let nestedURL = root.appendingPathComponent("zzz-nested")
        let innerURL = nestedURL.appendingPathComponent("inner")
        let nested = folder(nestedURL, children: [folder(innerURL, children: [folder(innerURL.appendingPathComponent("last"), children: [])])])
        let snapshot = try store.save(scan(folder(root, children: emptyFolders + [nested])), eventID: 0, eventIdentity: "preview")
        let loaded = try store.loadFolder(path: root.path, snapshot: snapshot)
        checkEqual(loaded.children.count, 501)
        let reopened = try checkUnwrap(loaded.children.first { $0.id == nested.id })
        checkEqual(reopened.children.first?.children.first?.name, "last")
        checkTrue(loaded.children.filter { $0.id != nested.id }.allSatisfy { $0.children.isEmpty })
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
        func stale(_ error: Error) {
            guard case .staleSnapshot? = error as? DiskIndexError else { return checkTrue(false, "Expected stale snapshot") }
        }
        checkThrowsError(try store.loadFolder(path: root.path, snapshot: initial, cancellation: ScanCancellation()), stale)
        checkThrowsError(try store.node(path: root.path, snapshot: initial, cancellation: ScanCancellation()), stale)
        checkThrowsError(try store.ancestors(path: root.path, snapshot: initial, cancellation: ScanCancellation()), stale)
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

    func testIncrementalHardLinksRecalculateNestedAncestors() throws {
        let a = root.appendingPathComponent("a")
        let nested = a.appendingPathComponent("nested")
        let z = root.appendingPathComponent("z")
        let original = leaf(z.appendingPathComponent("original"), size: 4096, identity: "shared")
        let unrelated = leaf(z.appendingPathComponent("unrelated"), size: 123, identity: "unique")
        let initial = try store.save(scan(folder(root, children: [folder(a, children: [folder(nested, children: [])]), folder(z, children: [original, unrelated])])), eventID: 1, eventIdentity: "v")
        let newLink = leaf(nested.appendingPathComponent("link"), size: 4096, identity: "shared")
        let linked = try store.update(snapshot: initial, replacements: [IndexReplacement(path: nested.path, result: scan(folder(nested, children: [newLink])))], eventID: 2, eventIdentity: "v")
        checkEqual(try store.node(path: nested.path, snapshot: linked)?.allocatedSize, 4096)
        checkEqual(try store.node(path: a.path, snapshot: linked)?.allocatedSize, 4096)
        checkEqual(try store.node(path: z.path, snapshot: linked)?.allocatedSize, 123)
        checkEqual(try store.node(path: root.path, snapshot: linked)?.allocatedSize, 4219)
        let removed = try store.update(snapshot: linked, replacements: [IndexReplacement(path: nested.path, result: nil)], eventID: 3, eventIdentity: "v")
        checkEqual(try store.node(path: a.path, snapshot: removed)?.allocatedSize, 0)
        checkEqual(try store.node(path: z.path, snapshot: removed)?.allocatedSize, 4219)
        checkEqual(try store.node(path: root.path, snapshot: removed)?.allocatedSize, 4219)
        checkEqual(try store.node(path: unrelated.id, snapshot: removed)?.allocatedSize, 123)
    }

    func testIncrementalMixedHardLinkBatchMatchesFullScan() throws {
        let fm = FileManager.default
        let root = try checkUnwrap(self.root)
        let a = root.appendingPathComponent("a")
        let aNested = a.appendingPathComponent("nested")
        let b = root.appendingPathComponent("b")
        let bNested = b.appendingPathComponent("nested")
        let c = root.appendingPathComponent("c")
        let d = root.appendingPathComponent("d")
        for directory in [aNested, bNested, c, d] {
            try fm.createDirectory(at: directory, withIntermediateDirectories: true)
        }
        let alpha = c.appendingPathComponent("alpha")
        let beta = c.appendingPathComponent("beta")
        let gamma = d.appendingPathComponent("gamma")
        let delta = d.appendingPathComponent("delta")
        let unrelated = d.appendingPathComponent("unrelated")
        for (index, file) in [alpha, beta, gamma, delta, unrelated].enumerated() {
            try Data(repeating: UInt8(index + 1), count: (index + 1) * 4_096).write(to: file)
        }
        let aAlpha = aNested.appendingPathComponent("alpha")
        let aBeta = aNested.appendingPathComponent("beta")
        let aGamma = aNested.appendingPathComponent("gamma")
        let bBeta = bNested.appendingPathComponent("beta")
        let bGamma = bNested.appendingPathComponent("gamma")
        let bDelta = bNested.appendingPathComponent("delta")
        try fm.linkItem(at: alpha, to: aAlpha)
        try fm.linkItem(at: beta, to: bBeta)
        try fm.linkItem(at: gamma, to: aGamma)
        let scanner = DiskScanner()
        let initial = try store.save(scanner.scan(root), eventID: 1, eventIdentity: "batch")
        for owner in [aAlpha, bBeta, aGamma, delta] {
            checkGreaterThanOrEqual(try checkUnwrap(store.node(path: owner.path, snapshot: initial)).allocatedSize, 1)
        }

        try fm.removeItem(at: aAlpha)
        try fm.removeItem(at: aGamma)
        try fm.linkItem(at: beta, to: aBeta)
        try fm.linkItem(at: gamma, to: bGamma)
        try fm.linkItem(at: delta, to: bDelta)
        let updated = try store.update(snapshot: initial, replacements: [
            IndexReplacement(path: aNested.path, result: scanner.scan(aNested)),
            IndexReplacement(path: bNested.path, result: scanner.scan(bNested)),
        ], eventID: 2, eventIdentity: "batch")
        let fullStore = DiskIndexStore(directory: fixture.appendingPathComponent("full-scan-cache"))
        let full = try fullStore.save(scanner.scan(root), eventID: 2, eventIdentity: "batch")
        for url in [root, a, aNested, b, bNested, c, d, alpha, beta, gamma, delta, unrelated, aBeta, bBeta, bGamma, bDelta] {
            let actual = try checkUnwrap(store.node(path: url.path, snapshot: updated))
            let expected = try checkUnwrap(fullStore.node(path: url.path, snapshot: full))
            checkEqual(actual.size, expected.size, "logical size: \(url.path)")
            checkEqual(actual.allocatedSize, expected.allocatedSize, "allocated size: \(url.path)")
            checkEqual(actual.fileCount, expected.fileCount, "file count: \(url.path)")
        }
        for owner in [alpha, aBeta, bGamma, bDelta] {
            checkGreaterThanOrEqual(try checkUnwrap(store.node(path: owner.path, snapshot: updated)).allocatedSize, 1)
        }
        for path in [aAlpha, aGamma] { checkNil(try store.node(path: path.path, snapshot: updated)) }
    }

    func testInterruptedIncrementalUpdatePreservesSnapshot() throws {
        let branch = root.appendingPathComponent("branch")
        let files = (0..<10_000).map { leaf(branch.appendingPathComponent("file-\($0)"), size: 1, identity: "identity-\($0)") }
        let updatedBranch = folder(branch, children: files)
        let initial = try store.save(scan(folder(root, children: [updatedBranch])), eventID: 1, eventIdentity: "v")
        let readCancellation = ScanCancellation()
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.001) { readCancellation.cancel() }
        checkThrowsError(try store.loadFolder(path: branch.path, snapshot: initial, cancellation: readCancellation)) {
            checkTrue($0 is CancellationError)
        }
        checkEqual(try store.loadFolder(path: branch.path, snapshot: initial).children.count, files.count)
        let cancellation = ScanCancellation()
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.01) { cancellation.cancel() }
        checkThrowsError(try store.update(snapshot: initial, replacements: [IndexReplacement(path: branch.path, result: scan(updatedBranch))], eventID: 2, eventIdentity: "v", cancellation: cancellation)) {
            checkTrue($0 is CancellationError)
        }
        checkEqual(store.savedSnapshot(for: root)?.id, initial.id)
        checkEqual(store.savedSnapshot(for: root)?.eventID, 1)
        checkEqual(try store.node(path: root.path, snapshot: initial)?.fileCount, files.count)
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
