import Darwin
import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

extension AsyncMappingChecks {
    static func livePageRetirement(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "live-page-retirement")
        let folderCount = 128
        for folder in 0..<folderCount {
            try fixture.file("folder-\(folder)/page-first", size: 3)
            try fixture.file("folder-\(folder)/page-second", size: 5)
        }
        let stage = try fixture.stage(), signals = Signals()
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        var callbacks = 0
        var gateTimedOut = false
        AsyncListingProducer.testRecords = { _, records in
            callbacks += 1
            if callbacks == 2 {
                entered.signal()
                gateTimedOut = release.wait(timeout: .now() + 30) == .timedOut
            }
            return records
        }
        defer {
            release.signal()
            stage.crawler.cancelAsyncDiscovery()
            AsyncListingProducer.testRecords = nil
        }
        let mailbox = try start(stage, signals: signals, workerCount: 1)
        try require(entered.wait(timeout: .now() + 10) == .success, "Discovery did not reach the second callback gate")
        let retained = mailbox.capture()
        let originalSequences = retained.batches.map(\.sequence)
        try require(originalSequences == [1, 2] && !mailbox.discoveryFinished,
                    "Page fixture did not retain a partial page while discovery was active")
        while mailbox.pendingCount > 0 {
            try require(try stage.crawler.stepAsync(), "Writer stopped before draining the partial page")
        }
        try require(mailbox.pendingBytes == 0 && mailbox.lastCommittedSequence == 2 && !mailbox.discoveryFinished,
                    "Writer could not retire every currently available batch while discovery was gated")
        try require(mailbox.capture().batches.isEmpty && retained.batches.map(\.sequence) == originalSequences,
                    "Retiring the live page damaged an earlier reader snapshot")
        release.signal()
        try signals.until("append after a fully drained partial page") { mailbox.discoveryFinished }
        try require(!gateTimedOut, "Discovery gate timed out instead of receiving the writer release")
        let appended = mailbox.capture()
        try require(appended.batches.map(\.sequence) == Array(Int64(3)...Int64(folderCount + 2)),
                    "New discovery reused retired offsets or lost batches at the next page boundary")
        try require(retained.batches.map(\.sequence) == originalSequences,
                    "Appending to the live page changed an earlier reader snapshot")
        let before = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())!.folder
        let matches = try stage.search(query: "page-", cancellation: ScanCancellation(), removals: [], limit: 1)
        try require(before.fileCount == folderCount * 2 && before.size == Int64(folderCount * 8)
            && matches.count == folderCount * 2 && matches.nodes.first?.size == 5,
                    "Live page retirement lost or double-counted newly discovered files")
        try finish(stage)
        try verifyTree(stage, fixture: fixture)
        try require(mailbox.pendingCount == 0 && mailbox.pendingBytes == 0,
                    "Page-boundary persistence did not drain every live batch")
        print("PASS: discovery resumes into a fully drained partial page and crosses a page boundary without changing retained snapshots")
    }

    static func backlogDiscovery(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "writer-held-backlog")
        for index in 0..<2048 {
            try fixture.file("backlog-\(index)-" + String(repeating: "x", count: 180), size: index % 17 + 1)
        }
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { stage.crawler.cancelAsyncDiscovery() }
        try signals.until("complete long-name discovery without writer progress") { mailbox.discoveryFinished }
        try require(mailbox.discoveredRecordCount == 2048 && mailbox.deferredDirectoryCount == 0,
                    "Writer backlog left undiscovered records")
        try require(mailbox.pendingBytes > 256 * 1024, "Fixture did not accumulate a substantial writer backlog")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                    "Discovery advanced the gated SQLite writer")
        let view = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())
        try require(view?.folder.children.count == 2048 && view?.folder.fileCount == 2048,
                    "Writer backlog made discovered files unavailable for browsing")
        let results = try stage.search(query: "backlog-", cancellation: ScanCancellation(), removals: [], limit: 1)
        try require(results.count == 2048 && results.nodes.first?.size == 17,
                    "Writer backlog changed exact search counts or global size ranking")
        let victim = view!.folder.children[0]
        let removal = ScanRemoval(path: victim.id, identity: victim.fileIdentity!)
        let hidden = try stage.view(path: nil, removals: [removal], cancellation: ScanCancellation())
        try require(hidden?.folder.fileCount == 2047 && !hidden!.folder.children.contains { $0.id == victim.id },
                    "A discovered file could not be removed while persistence was gated")
        try finish(stage)
        try verifyTree(stage, fixture: fixture)
        try require(mailbox.pendingBytes == 0 && mailbox.pendingCount == 0,
                    "Committed backlog remained retained by the mailbox")
        print("PASS: writer-held backlog finishes discovery and supports browsing, search and removal before persistence")
    }

    static func wideFrontierDiscovery(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "wide-frontier")
        let folderCount = 4100
        for index in 0..<folderCount { try fixture.file("folder-\(index)/frontier-leaf-\(index)", size: index % 17 + 1) }
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { stage.crawler.cancelAsyncDiscovery() }
        try signals.until("all descendants beyond the former frontier ceiling", timeout: 30) { mailbox.discoveryFinished }
        try require(mailbox.discoveredRecordCount == folderCount * 2 && mailbox.deferredDirectoryCount == 0,
                    "Wide directory discovery handed unvisited descendants to the durable writer")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                    "Wide discovery required SQLite writer progress")
        let before = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())!.folder
        let expectedSize = Int64((0..<folderCount).reduce(0) { $0 + $1 % 17 + 1 })
        try require(before.children.count == folderCount && before.fileCount == folderCount && before.size == expectedSize,
                    "RAM totals omitted descendants beyond the former frontier ceiling")
        let search = try stage.search(query: "frontier-leaf-", cancellation: ScanCancellation(), removals: [], limit: 1)
        try require(search.count == folderCount && search.nodes.first?.size == 17,
                    "RAM search omitted wide-frontier descendants")
        let retained = mailbox.capture()
        let retainedSequences = retained.batches.map(\.sequence)
        try require(retainedSequences.count > 4096, "Fixture did not exercise a large retained batch queue")
        var steps = 0
        while try stage.crawler.stepAsync() {
            steps += 1
            if steps.isMultiple(of: 256) {
                let committed = mailbox.lastCommittedSequence
                let current = mailbox.capture()
                try require(current.batches.allSatisfy { $0.sequence > committed },
                            "Retired batches remained visible after a queue drain boundary")
                try require(retained.batches.map(\.sequence) == retainedSequences,
                            "Queue retirement mutated an older reader's retained snapshot")
            }
        }
        try finish(stage)
        let after = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())!.folder
        try require(after.children.count == folderCount && after.fileCount == folderCount && after.size == expectedSize
            && after.allocatedSize == before.allocatedSize && after.scanState == .complete,
                    "Draining the wide backlog lost records or changed totals")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == Int64(folderCount * 2 + 1),
                    "Wide backlog was not persisted exactly once")
        try require(mailbox.pendingCount == 0 && mailbox.pendingBytes == 0
            && retained.batches.map(\.sequence) == retainedSequences,
                    "Drained inventory retained live batches or invalidated a pinned snapshot")
        print("PASS: more than 4,096 directories finish discovery before writes; queue draining preserves retained snapshots and totals")
    }

    static func backlogCancellation(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "large-backlog-cancel")
        let folderCount = 8
        for folder in 0..<folderCount {
            for index in 0..<128 { try fixture.file("folder-\(folder)/source-\(index)") }
        }
        let batchCount = 20_000
        let count = folderCount * batchCount + folderCount
        let suffix = String(repeating: "x", count: 220)
        let rootPath = Data(fixture.root.path.utf8)
        let cancellation = ScanCancellation(), stage = try fixture.stage(cancellation), signals = Signals()
        var nextName = 0
        let nameLock = NSLock()
        AsyncListingProducer.testRecords = { path, records in
            if path == rootPath { return records }
            let record = records[0]
            nameLock.lock()
            let firstName = nextName
            nextName += batchCount
            let endName = nextName
            nameLock.unlock()
            return (firstName..<endName).map { index in
                FileMetadata(name: "synthetic-\(index)-" + suffix, device: record.device, inode: record.inode,
                    isDirectory: false, isSymbolicLink: false, isMountPoint: false, linkCount: 1,
                    size: 7, allocatedSize: record.allocatedSize)
            }
        }
        defer { AsyncListingProducer.testRecords = nil; stage.crawler.cancelAsyncDiscovery() }
        let mailbox = try start(stage, signals: signals)
        try signals.until("discovery beyond the former persistence backlog limit", timeout: 30) { mailbox.discoveryFinished }
        try require(mailbox.discoveredRecordCount == count && mailbox.deferredDirectoryCount == 0,
                    "Large persistence backlog stopped directory discovery")
        try require(mailbox.pendingBytes > 128 * 1024 * 1024,
                    "Synthetic inventory did not exceed the former persistence backlog limit")
        let retained = mailbox.capture()
        try require(retained.batches.reduce(0) { $0 + $1.records.count } == count,
                    "Large persistence backlog did not retain every discovered record")
        try require(retained.batches.allSatisfy { $0.byteCount < 128 * 1024 * 1024 },
                    "Fixture exceeded the old backlog limit in one batch instead of accumulated discovery")
        let listings = Dictionary(grouping: retained.batches, by: \.parentPath)
        try require(listings.values.allSatisfy { $0.reduce(0) { $0 + $1.records.count } < 131_072 },
                    "Backlog fixture also exceeded the old listing limit")
        cancellation.cancel()
        stage.crawler.cancelAsyncDiscovery()
        try require(mailbox.pendingCount == 0 && mailbox.pendingBytes == 0 && !retained.isCurrent(),
                    "Cancellation left a backlog or accepted a stale retained inventory")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                    "Backlog discovery or cancellation advanced the gated writer")
        print("PASS: discovery exceeds a 128 MiB persistence backlog without writes; cancellation clears pending work and rejects retained snapshots")
    }

    static func wideListingDiscovery(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "wide-listing-discovery")
        for index in 0..<128 { try fixture.file("source-\(index)") }
        let count = 131_073
        let stage = try fixture.stage(), signals = Signals()
        AsyncListingProducer.testRecords = { _, records in
            let record = records[0]
            return (0..<count).map { index in
                FileMetadata(name: "synthetic-\(index)", device: record.device, inode: record.inode,
                    isDirectory: false, isSymbolicLink: false, isMountPoint: false, linkCount: 1,
                    size: 7, allocatedSize: record.allocatedSize)
            }
        }
        defer { AsyncListingProducer.testRecords = nil; stage.crawler.cancelAsyncDiscovery() }
        let mailbox = try start(stage, signals: signals)
        try signals.until("listing beyond the former entry ceiling", timeout: 30) { mailbox.discoveryFinished }
        try require(mailbox.discoveredRecordCount == count && mailbox.deferredDirectoryCount == 0,
                    "Large listing handed remaining entries to the durable writer")
        try require(mailbox.pendingBytes < 128 * 1024 * 1024,
                    "Wide-listing fixture also exceeded the old persistence backlog limit")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                    "Wide-listing discovery advanced the gated writer")
        print("PASS: more than 131,072 entries in one listing finish discovery before writes")
    }
}
