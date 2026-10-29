import Darwin
import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

@main struct AsyncMappingChecks {
    static let fm = FileManager.default

    static func start(_ stage: ResumableStage, signals: Signals,
                      deferDirectory: ((Data) -> Bool)? = nil, workerCount: Int = 4) throws -> PendingInventoryMailbox {
        try stage.prepareRemovalReplay()
        stage.testDisableAsyncDiscovery = true
        try require(stage.crawler.startAsyncDiscovery(deferDirectory: deferDirectory, workerCount: workerCount,
                    onAccepted: { signals.signal() }), "Fresh native discovery did not start")
        guard let mailbox = stage.crawler.asyncInventory else { throw AsyncCheckFailure(description: "Missing discovery mailbox") }
        return mailbox
    }

    static func main() throws {
        setbuf(stdout, nil)
        if CommandLine.arguments.count == 4, CommandLine.arguments[1] == "crash-child" {
            try crashChild(URL(fileURLWithPath: CommandLine.arguments[2]), boundary: CommandLine.arguments[3])
            return
        }
        guard CommandLine.arguments.count == 2 else { fatalError("Usage: AsyncMappingChecks FIXTURE_ROOT") }
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        try SecureCursorChecks.run(base)
        try DiscoveryHotPathChecks.run(base)
        try MappingProgressChecks.run(base)
        try MetadataBookkeepingChecks.run(base)
        try BoundedChildrenChecks.run(base)
        try automaticSelection(base)
        try liveViewsWithoutDatabase(base)
        try resumedLiveDiscovery(base)
        try sharedLiveDiscovery(base)
        try refreshedLiveDiscovery(base)
        try freshLeafGroups(base)
        try cancelledLeafGroup(base)
        try removalDuringLeafGroup(base)
        try liveCacheInvalidation(base)
        try ParallelDiscoveryChecks.run(base)
        try pendingNavigationAndSearch(base)
        try snapshotBoundaries(base)
        try livePageRetirement(base)
        try preAccountingHardLinks(base)
        try journalRemoval(base)
        try concurrentReplacement(base)
        try cachedWriterAncestorReplacement(base)
        try directoryFailurePreservesProgress(base)
        try duplicateListings(base)
        try backlogDiscovery(base)
        try wideFrontierDiscovery(base)
        try backlogCancellation(base)
        try wideListingDiscovery(base)
        try crashResume(base)
        print("PASS: asynchronous discovery and durable mapping checks")
    }

    static func automaticSelection(_ base: URL) throws {
        let native = try AsyncFixture(base, "automatic-native")
        try native.file("folder/leaf")
        let fresh = try native.stage()
        _ = try fresh.advance(progress: { _ in })
        try require(fresh.crawler.asyncInventory != nil, "Fresh native mapping did not select asynchronous discovery")
        try finish(fresh)
        try verifyTree(fresh, fixture: native)
        let fallback = try AsyncFixture(base, "automatic-fallback")
        try fallback.file("folder/leaf")
        let serial = try fallback.stage()
        serial.configureTestScanner(DiskScanner(useBulkEnumeration: false))
        try finish(serial)
        try require(serial.crawler.asyncInventory != nil, "Fallback walker did not use asynchronous discovery")
        try verifyTree(serial, fixture: fallback)
        print("PASS: native and fallback mappings both select asynchronous discovery")
    }

    static func pendingNavigationAndSearch(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "pending-reader")
        let nested = try fixture.directory("parent/nested")
        for index in 0..<360 { try fixture.file("parent/nested/match-\(index)", size: index + 1) }
        let owner = try fixture.file("parent/owner", size: 8192)
        try fm.linkItem(at: owner, to: fixture.root.appendingPathComponent("linked"))
        try fm.createSymbolicLink(atPath: fixture.root.appendingPathComponent("symbolic").path, withDestinationPath: "parent")
        try fixture.file("parent/nested/が-é-quoted'file", size: 13)
        let sparse = fixture.root.appendingPathComponent("sparse")
        let fd = open(sparse.path, O_CREAT | O_WRONLY | O_EXCL, 0o600)
        try require(fd >= 0, "Could not create sparse fixture")
        let sparseResult = ftruncate(fd, 8 * 1024 * 1024)
        close(fd)
        try require(sparseResult == 0, "Could not size sparse fixture")
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { stage.crawler.cancelAsyncDiscovery() }
        try signals.until("nested discovery while writer is gated") { mailbox.discoveryFinished }
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                    "Discovery wrote SQLite before the writer was allowed to run")
        guard let view = try stage.view(path: nested.path, removals: [], cancellation: ScanCancellation()) else {
            throw AsyncCheckFailure(description: "Cannot navigate to a folder discovered only in memory")
        }
        try require(view.folder.children.count == 361, "Memory folder listing is incomplete")
        try require(!stage.confirmsMissingDirectory(nested.path), "Memory-only directory was reported missing")
        let matches = try stage.search(query: "match-", cancellation: ScanCancellation(), removals: [], limit: 300)
        try require(matches.count == 360 && matches.nodes.count == 300, "Pending search lost exact match count or result limit")
        try require(matches.nodes.first?.size == 360 && matches.nodes.last?.size == 61, "Pending search is not globally size-first")
        let victim = view.folder.children.first { $0.name == "match-359" }!
        let removal = ScanRemoval(path: victim.id, identity: victim.fileIdentity!)
        let hidden = try stage.search(query: "match-", cancellation: ScanCancellation(), removals: [removal], limit: 300)
        try require(hidden.count == 359 && !hidden.nodes.contains { $0.id == victim.id }, "Pending removal did not project into search")
        let rootRecord = try FileMetadata.read(fixture.root)
        let wrongIdentity = ScanRemoval(path: victim.id, identity: "\(rootRecord.device):0")
        let preserved = try stage.search(query: "match-", cancellation: ScanCancellation(), removals: [wrongIdentity], limit: 300)
        try require(preserved.count == 360, "Old identity removal hid a replacement at the same path")
        while try stage.advance(progress: { _ in }) {
            let transitioning = try stage.search(query: "match-", cancellation: ScanCancellation(), removals: [], limit: 300)
            try require(transitioning.count == 360 && transitioning.nodes.count == 300,
                        "Commit/retirement boundary duplicated or lost search results")
            let combined = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())!.folder
            let reference = try DiskScanner().scan(fixture.root).root
            try require(combined.size == reference.size && combined.allocatedSize == reference.allocatedSize && combined.fileCount == reference.fileCount,
                        "Commit/retirement boundary changed complete combined totals")
        }
        try verifyTree(stage, fixture: fixture)
        print("PASS: nested memory navigation, exact search, removal identity, commit retirement and metadata parity")
    }

    static func journalRemoval(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "queued-removal")
        let victim = try fixture.file("queued/victim", size: 41)
        try fixture.file("queued/survivor", size: 19)
        try fixture.file("remove-folder/nested/leaf", size: 29)
        let unaffected = try fixture.directory("unrelated/deep/nested")
        for index in 0..<17 { try fixture.file("unrelated/deep/nested/retained-\(index)", size: index + 1) }
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { stage.crawler.cancelAsyncDiscovery() }
        try signals.until("queued deletion discovery") { mailbox.discoveryFinished }
        let originalPending = try stage.view(path: unaffected.path, removals: [], cancellation: ScanCancellation())
        try require(originalPending?.folder.children.count == 17, "Unrelated nested RAM inventory was not discovered")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                    "Deletion fixture writer was not gated")
        let folder = fixture.root.appendingPathComponent("remove-folder")
        let removals = [ScanRemoval(path: victim.path, identity: try identity(victim)),
                        ScanRemoval(path: folder.path, identity: try identity(folder))]
        let journal = try RemovalJournal(cache: fixture.cache)
        let operation = try journal.begin(root: fixture.root, rootIdentity: identity(fixture.root), items: removals)
        let oldDescriptor = open(victim.path, O_RDONLY)
        try require(oldDescriptor >= 0, "Could not retain deleted inode for replacement check")
        defer { close(oldDescriptor) }
        try fm.removeItem(at: victim)
        try fm.removeItem(at: folder)
        let replacement = try fixture.file("queued/victim", size: 53)
        try require(try identity(replacement) != removals[0].identity, "Filesystem reused the test inode")
        try operation.complete()
        try stage.prepareRemovalReplay()
        let retained = try stage.view(path: unaffected.path, removals: [], cancellation: ScanCancellation())
        try require(retained?.folder.children.count == 17 && retained?.folder.size == 153,
                    "Ordinary deletion discarded unrelated nested RAM progress before the writer ran")
        let retainedMatches = try stage.search(query: "retained-", cancellation: ScanCancellation(), removals: [], limit: 300)
        try require(retainedMatches.count == 17 && retainedMatches.nodes.count == 17,
                    "Ordinary deletion made unrelated RAM search results unavailable")
        let afterRemoval = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())
        try require(afterRemoval?.folder.fileCount == 18,
                    "Deletion projection lost unrelated progress or exposed removed pending records")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                    "Deletion visibility check advanced the gated inventory writer")
        try finish(stage)
        try verifyTree(stage, fixture: fixture)
        let result = try stage.search(query: "victim", cancellation: ScanCancellation(), removals: removals, limit: 300)
        try require(result.count == 1 && result.nodes.first?.size == 53, "Journal replay resurrected an old queued record or hid its replacement")
        let removed = try stage.search(query: "remove-folder", cancellation: ScanCancellation(), removals: [], limit: 300)
        try require(removed.count == 0, "Queued directory reappeared after journal deletion")
        print("PASS: removal preserves unrelated RAM navigation/search before writes and keeps the replacement inode after reconciliation")
    }

    static func preAccountingHardLinks(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "before-link-accounting")
        let owner = try fixture.file("first-link", size: 8192)
        try fm.linkItem(at: owner, to: fixture.root.appendingPathComponent("second-link"))
        let expected = try DiskScanner().scan(fixture.root).root
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { stage.crawler.cancelAsyncDiscovery() }
        try signals.until("hardlink discovery") { mailbox.discoveryFinished }
        var observed = false
        while try stage.crawler.stepAsync(beforeAccounting: {
            observed = true
            let actual = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())!.folder
            try require(actual.size == expected.size && actual.allocatedSize == expected.allocatedSize && actual.fileCount == expected.fileCount,
                        "Async commit watermark exposed incomplete hardlink accounting")
        }) {}
        try require(observed, "No pre-accounting boundary reached")
        try finish(stage)
        try verifyTree(stage, fixture: fixture)
        print("PASS: committed watermark preserves allocated totals before deferred hardlink accounting")
    }

    static func concurrentReplacement(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "concurrent-directory-replacement")
        try fixture.file("branch/old-leaf", size: 47)
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { stage.crawler.cancelAsyncDiscovery() }
        try signals.until("directory replacement discovery") { mailbox.discoveryFinished }
        try fm.moveItem(at: fixture.root.appendingPathComponent("branch"), to: fixture.base.appendingPathComponent("old-branch"))
        try fixture.file("branch/new-leaf", size: 59)
        try finish(stage)
        try verifyTree(stage, fixture: fixture)
        let stale = try stage.search(query: "old-leaf", cancellation: ScanCancellation(), removals: [], limit: 300)
        try require(stale.count == 0, "A stale queued child survived its parent's identity change")
        print("PASS: concurrent directory replacement rejects stale queued metadata and relists safely")
    }

    static func cachedWriterAncestorReplacement(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "cached-writer-ancestor-replacement")
        try fixture.file("branch/nested/old-leaf", size: 47)
        try fixture.file("unrelated/retained", size: 13)
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { stage.crawler.cancelAsyncDiscovery() }
        try signals.until("cached writer ancestor discovery") { mailbox.discoveryFinished }
        let branch = fixture.root.appendingPathComponent("branch")
        var replaced = false
        while try stage.crawler.stepAsync(onBatch: { path, _ in
            if !replaced && path == Data(branch.path.utf8) {
                try fm.moveItem(at: branch, to: fixture.base.appendingPathComponent("old-branch"))
                try fixture.file("branch/new-leaf", size: 59)
                replaced = true
            }
        }) {}
        try require(replaced, "Writer did not reach the cached ancestor before replacement")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes WHERE name='old-leaf'") == 0,
                    "Writer accepted descendant metadata through a detached cached ancestor")
        try finish(stage)
        try verifyTree(stage, fixture: fixture)
        print("PASS: cached writer ancestors are revalidated before descendants and replacements reconcile safely")
    }

    static func snapshotBoundaries(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "snapshot-boundaries")
        for index in 0..<140 { try fixture.file("rank-parent/rank-\(index)", size: index + 1) }
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { stage.crawler.cancelAsyncDiscovery(); DiskIndexStore.testAfterPendingWatermark = nil }
        try signals.until("snapshot fixture discovery") { mailbox.discoveryFinished }
        _ = try stage.advance(progress: { _ in })
        let retained = mailbox.capture()
        let store = DiskIndexStore(directory: fixture.cache)
        let hook = HookOutcome()
        DiskIndexStore.testAfterPendingWatermark = {
            hook.run { try finish(stage) }
        }
        let pinned = try store.stagedSearch(query: "rank-", snapshot: stage.testPreviewSnapshot, revision: 1,
            pending: retained, removals: [], cancellation: ScanCancellation(), limit: 1)
        try hook.verify()
        DiskIndexStore.testAfterPendingWatermark = nil
        try require(pinned.count == 141 && pinned.nodes.first?.name == "rank-parent" && pinned.nodes.first?.size == 9870,
                    "Pinned SQLite snapshot lost retired pending rows or ancestor ranking")
        let newer = try store.stagedSearch(query: "rank-", snapshot: stage.testPreviewSnapshot, revision: 2,
            pending: retained, removals: [], cancellation: ScanCancellation(), limit: 1)
        try require(newer.count == 141 && newer.nodes.first?.size == 9870,
                    "Newer SQLite snapshot counted already-committed pending rows twice")
        stage.crawler.cancelAsyncDiscovery()
        do {
            _ = try store.stagedSearch(query: "rank-", snapshot: stage.testPreviewSnapshot, revision: 3,
                pending: retained, removals: [], cancellation: ScanCancellation(), limit: 1)
            throw AsyncCheckFailure(description: "Cancelled pending snapshot was accepted")
        } catch is CancellationError {}
        print("PASS: pinned snapshots retain queued rows, newer snapshots filter them, and cancellation rejects stale reads")
    }

    static func directoryFailurePreservesProgress(_ base: URL) throws {
        for remove in [false, true] {
            if !remove && geteuid() == 0 { continue }
            let fixture = try AsyncFixture(base, remove ? "missing-directory" : "denied-directory")
            var folders: [URL] = []
            for index in 0..<10 {
                folders.append(try fixture.directory("branch-\(index)"))
                try fixture.file("branch-\(index)/retained-leaf", size: 11)
            }
            let failure = FirstDirectoryFailure(remove: remove)
            defer { failure.restore() }
            let stage = try fixture.stage(), signals = Signals()
            stage.crawler.preferredDirectory = failure.preferredPath
            let mailbox = try start(stage, signals: signals, deferDirectory: failure.intercept)
            defer { stage.crawler.cancelAsyncDiscovery() }
            try signals.until("discovery continues after first directory fails") { mailbox.discoveryFinished }
            let rejected = try failure.path()
            try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                        "Directory failure fixture writer was not gated")
            let before = try stage.search(query: "retained-leaf", cancellation: ScanCancellation(), removals: [], limit: 300)
            try require(before.count == 9, "One directory failure stopped discovery of unrelated queued directories")
            for folder in folders where folder.path != rejected {
                let view = try stage.view(path: folder.path, removals: [], cancellation: ScanCancellation())
                try require(view?.folder.children.count == 1, "Directory failure lost unrelated RAM navigation")
            }
            while try stage.crawler.stepAsync() {
                let surviving = try stage.search(query: "retained-leaf", cancellation: ScanCancellation(), removals: [], limit: 300)
                try require(surviving.count == 9, "Writer discarded unrelated RAM after a directory failure")
            }
            try finish(stage)
            try verifyTree(stage, fixture: fixture)
        }
        print("PASS: denied and missing first directories retain unrelated discovery and RAM progress")
    }

    static func duplicateListings(_ base: URL) throws {
        for crossBatch in [false, true] {
            let fixture = try AsyncFixture(base, crossBatch ? "duplicate-across-batches" : "duplicate-within-batch")
            let target = try fixture.directory("suspect")
            for index in 0..<384 { try fixture.file("suspect/duplicate-fixture-\(index)", size: index + 1) }
            for index in 0..<17 { try fixture.file("unrelated/retained-\(index)", size: 1) }
            let stage = try fixture.stage(), signals = Signals()
            let injection = DuplicateListingInjection(target: target, crossBatch: crossBatch)
            AsyncListingProducer.testRecords = injection.records
            defer { AsyncListingProducer.testRecords = nil; stage.crawler.cancelAsyncDiscovery() }
            let mailbox = try start(stage, signals: signals)
            try signals.until("duplicate listing recovery discovery") { mailbox.discoveryFinished }
            try injection.verify()
            try require(mailbox.retryDeferredDirectoryCount == 1 && mailbox.deferredDirectoryCount >= 1,
                        "Rejected listing was reported as fully discovered")
            let accepted = mailbox.capture().batches.filter { $0.parentPath == Data(target.path.utf8) }.flatMap(\.records)
            let expectedCount = crossBatch ? 128 : 0
            try require(accepted.count == expectedCount && Set(accepted.map { Data($0.name.utf8) }).count == expectedCount,
                        "Duplicate callback records reached the RAM inventory")
            let matching = try stage.search(query: "duplicate-fixture-", cancellation: ScanCancellation(), removals: [], limit: 300)
            try require(matching.count == expectedCount, "Rejected duplicate batch changed the exact RAM match count")
            let unrelated = try stage.search(query: "retained-", cancellation: ScanCancellation(), removals: [], limit: 300)
            try require(unrelated.count == 17, "Duplicate callback stopped unrelated discovery")
            while try stage.crawler.stepAsync() {
                let retained = try stage.search(query: "retained-", cancellation: ScanCancellation(), removals: [], limit: 300)
                try require(retained.count == 17, "Duplicate listing repair discarded unrelated RAM progress")
            }
            AsyncListingProducer.testRecords = nil
            try finish(stage)
            try verifyTree(stage, fixture: fixture)
            let repaired = try stage.search(query: "duplicate-fixture-", cancellation: ScanCancellation(), removals: [], limit: 300)
            try require(repaired.count == 384, "Serial repair did not replace the rejected listing completely")
        }
        print("PASS: within-batch and cross-batch duplicate names are rejected before exposure and repaired without losing unrelated RAM")
    }

    static func crashChild(_ base: URL, boundary: String) throws {
        let fixture = try AsyncFixture(base, boundary)
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        try signals.until("crash child discovery") { mailbox.discoveryFinished }
        if boundary == "before-write" { kill(getpid(), SIGKILL) }
        ListingStage.testListingCommit = { observed in
            let expected = boundary == "group-before-commit" ? "fresh-group-written"
                : boundary == "group-after-commit" ? "fresh-group-committed" : "batch-committed"
            if observed == expected { kill(getpid(), SIGKILL) }
        }
        try finish(stage)
        throw AsyncCheckFailure(description: "Missed async crash boundary")
    }

    static func crashResume(_ base: URL) throws {
        let crashBase = base.appendingPathComponent("crashes")
        for boundary in ["before-write", "after-commit", "group-before-commit", "group-after-commit"] {
            let fixture = try AsyncFixture(crashBase, boundary)
            if boundary.hasPrefix("group-") {
                for index in 0..<8 { try fixture.file("leaf-\(index)/file", size: index + 1) }
            } else {
                for index in 0..<260 { try fixture.file("root-file-\(index)", size: index + 1) }
                try fixture.file("child/nested/file", size: 1024)
            }
            let process = Process()
            process.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
            process.arguments = ["crash-child", crashBase.path, boundary]
            try process.run(); process.waitUntilExit()
            try require(process.terminationReason == .uncaughtSignal && process.terminationStatus == SIGKILL,
                        "Child did not crash at the requested boundary")
            let stage = try fixture.stage()
            try require(stage.hasPriorCheckpoint, "Crash lost the durable root checkpoint")
            let rows = try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes")
            try require(boundary == "before-write" ? rows == 1 : rows > 1, "Crash boundary did not preserve the expected committed prefix")
            if boundary.hasPrefix("group-") {
                let files = try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes WHERE is_dir=0")
                try require(boundary == "group-before-commit" ? files == 0 : files > 0,
                            "Group crash exposed an uncommitted or lost a committed prefix")
            }
            try finish(stage)
            try require(stage.crawler.asyncInventory != nil, "Resumed discovery fell back to the serial writer")
            try verifyTree(stage, fixture: fixture)
        }
        print("PASS: SIGKILL before writes and after commit resumes pending directory work without loss")
    }
}
