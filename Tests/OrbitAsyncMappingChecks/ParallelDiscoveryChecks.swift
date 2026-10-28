import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

enum ParallelDiscoveryChecks {
    static func run(_ base: URL) throws {
        for fallback in [false, true] { try independentSiblings(base, fallback: fallback) }
        try independentSiblings(base, fallback: false, retryNative: true)
        try cancellation(base)
        print("PASS: parallel discovery streams live siblings, orders durable listings, deduplicates links and stops publication on cancellation")
    }

    private static func independentSiblings(_ base: URL, fallback: Bool, retryNative: Bool = false) throws {
        let label = retryNative ? "parallel-native-retry" : fallback ? "parallel-fallback" : "parallel-native"
        let fixture = try AsyncFixture(base, label)
        let slow = try fixture.directory("slow")
        for index in 0..<384 { try fixture.file("slow/slow-\(index)", size: index + 1) }
        for name in ["fast-a", "fast-b", "fast-c"] {
            for index in 0..<160 { try fixture.file("\(name)/entry-\(name)-\(index)", size: index + 1) }
        }
        let nested = try fixture.file("fast-a/nested/visible-leaf", size: 19)
        let owner = try fixture.file("slow/shared", size: 8192)
        try FileManager.default.linkItem(at: owner, to: fixture.root.appendingPathComponent("fast-a/shared"))
        let rootPath = Data(fixture.root.path.utf8), slowPath = Data(slow.path.utf8)
        let probe = ParallelProbe(root: rootPath, slow: slowPath)
        let stage = try fixture.stage(), signals = Signals()
        if fallback { stage.configureTestScanner(DiskScanner(useBulkEnumeration: false)) }
        stage.prioritize(path: slow.path)
        install(probe, retryNative: retryNative)
        defer {
            probe.releaseAll(); stage.crawler.cancelAsyncDiscovery()
            try? probe.waitForWorkers()
            uninstall()
        }
        let mailbox = try AsyncMappingChecks.start(stage, signals: signals, workerCount: 4)
        try probe.waitForGates(1)
        try require(probe.claims()[slowPath] == 1, "The prioritized sibling did not receive the first directory ticket")
        try signals.until("other workers publish while the first sibling is stalled") {
            do {
                let found = try stage.search(query: "entry-fast-", cancellation: ScanCancellation(), removals: [], limit: 10)
                let leaf = try stage.view(path: nested.deletingLastPathComponent().path, removals: [], cancellation: ScanCancellation())
                let leading = mailbox.capture().batches.contains { $0.parentPath == slowPath && $0.records.count == 128 }
                return found.count == 480 && leaf?.folder.children.first?.name == "visible-leaf" && leading
            } catch is CancellationError { return false }
        }
        let partial = mailbox.capture().batches
        try require(!mailbox.discoveryFinished, "Discovery finished while a directory worker was still gated")
        try require(partial.contains { $0.parentPath == slowPath && $0.records.count == 128 && $0.completion == nil },
                    "The leading directory ticket buffered its first batch until listing completion")
        try require(partial.allSatisfy { $0.parentPath == rootPath || $0.parentPath == slowPath },
                    "Later directory tickets interleaved with a partial leading listing")
        try require(mailbox.discoveredRecordCount > partial.reduce(0, { $0 + $1.records.count }),
                    "Discovery progress waited for the ordered durable queue")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                    "Parallel RAM discovery unexpectedly required a writer transaction")
        probe.releaseAll()
        try probe.waitForWorkers()
        try signals.until("parallel discovery completion") { mailbox.discoveryFinished }
        try probe.verify()
        try ordered(mailbox.capture().batches, root: rootPath, claims: probe.claims(), retryNative: retryNative)
        let expected = try DiskScanner().scan(fixture.root).root
        let live = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())
        try require(live?.root.size == expected.size && live?.root.allocatedSize == expected.allocatedSize
            && live?.root.fileCount == expected.fileCount, "Cross-worker live totals or hard-link ownership differ")
        try finish(stage)
        try verifyTree(stage, fixture: fixture)
        try require(mailbox.isDrained, "Parallel durable queue did not drain")
        let database = stage.crawler.store.databasePath
        try require(try scalar(database, "SELECT size FROM nodes WHERE parent IS NULL") == expected.size
            && scalar(database, "SELECT allocated FROM nodes WHERE parent IS NULL") == expected.allocatedSize
            && scalar(database, "SELECT file_count FROM nodes WHERE parent IS NULL") == Int64(expected.fileCount),
            "Parallel durable accounting differs from independent filesystem totals")
        let metadata = try FileMetadata.read(owner)
        try require(try scalar(database, "SELECT SUM(allocated) FROM nodes WHERE is_dir=0 AND dev=\(metadata.device) AND inode=\(Int64(bitPattern: metadata.inode))") == metadata.allocatedSize,
                    "Different discovery workers charged the same hard link more than once")
    }

    private static func ordered(_ batches: [PendingInventoryBatch], root: Data, claims: [Data: Int64],
                                retryNative: Bool) throws {
        try require(!batches.isEmpty, "Parallel discovery returned no durable batches")
        var announced = Set<Data>([root]), completed = Set<Data>()
        var fallbackPaths = Set<Data>(), names = Set<Data>()
        var active: Data?, previousTicket: Int64 = -1
        for (offset, batch) in batches.enumerated() {
            try require(batch.sequence == Int64(offset + 1), "Parallel durable sequences have a gap or duplicate")
            try require(announced.contains(batch.parentPath), "A child's durable listing preceded its parent record")
            guard let ticket = claims[batch.parentPath] else {
                throw AsyncCheckFailure(description: "Durable listing has no worker ticket")
            }
            if active != batch.parentPath {
                if let active { try require(completed.contains(active), "A directory ticket ended without a completion marker") }
                try require(ticket > previousTicket, "Durable listings did not preserve worker ticket order")
                try require(!completed.contains(batch.parentPath), "A completed directory returned in a later queue segment")
                active = batch.parentPath; previousTicket = ticket
                names.removeAll(keepingCapacity: true)
            }
            if let error = batch.failure {
                guard retryNative, let failure = error as? StreamingBulkDirectory.Failure,
                      case .needsFallback = failure else {
                    throw AsyncCheckFailure(description: "Parallel fixture encountered an unexpected listing failure")
                }
                try require(fallbackPaths.insert(batch.parentPath).inserted && batch.completion == nil,
                            "A native listing retried more than once or completed before fallback")
                names.removeAll(keepingCapacity: true)
            }
            for record in batch.records {
                try require(names.insert(Data(record.name.utf8)).inserted,
                            "Parallel listing published a duplicate name without restarting its epoch")
            }
            for record in batch.records where record.isDirectory {
                announced.insert(HardLinkPathOrder.path(batch.parentPath, Data(record.name.utf8)))
            }
            if batch.completion != nil {
                try require(completed.insert(batch.parentPath).inserted, "A directory published duplicate completion markers")
            }
        }
        try require(completed == Set(claims.keys), "A claimed directory never completed its durable listing")
        if retryNative {
            try require(fallbackPaths == Set(claims.keys), "Native retry fixture did not exercise every worker listing")
        }
    }

    private static func cancellation(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "parallel-cancellation")
        for index in 0..<12 { try fixture.file("folder-\(index)/unpublished", size: 11) }
        let rootPath = Data(fixture.root.path.utf8)
        let probe = ParallelProbe(root: rootPath, slow: nil)
        let stage = try fixture.stage(), signals = Signals()
        install(probe)
        defer {
            probe.releaseAll(); stage.crawler.cancelAsyncDiscovery()
            try? probe.waitForWorkers()
            uninstall()
        }
        let mailbox = try AsyncMappingChecks.start(stage, signals: signals, workerCount: 4)
        try probe.waitForGates(4)
        let inventory = stage.crawler.liveInventory
        let before = try inventory.view(path: nil, removals: [], cancellation: ScanCancellation())
        let beforeCount = mailbox.discoveredRecordCount
        let retained = mailbox.capture()
        try require(before?.folder.children.count == 12 && beforeCount == 12,
                    "Cancellation fixture did not stop all workers before child enumeration")
        stage.crawler.cancelAsyncDiscovery()
        probe.releaseAll()
        try probe.waitForWorkers()
        try probe.verify()
        let after = try inventory.view(path: nil, removals: [], cancellation: ScanCancellation())
        try require(after?.revision == before?.revision && after?.root.size == 0 && after?.root.fileCount == 0,
                    "A worker published live changes after cancellation")
        try require(mailbox.isCancelled && mailbox.capture().batches.isEmpty && !retained.isCurrent()
            && mailbox.discoveredRecordCount == beforeCount, "Cancelled workers appended durable batches or progress")
        try require(probe.claims().count == 5, "Cancelled workers claimed additional queued directories")
    }

    private static func install(_ probe: ParallelProbe, retryNative: Bool = false) {
        AsyncListingProducer.testBeforeDirectory = probe.beforeDirectory
        AsyncListingProducer.testRecords = probe.records
        AsyncListingProducer.testWorkersFinished = probe.workersFinished
        AsyncListingProducer.testFallbackAfterFirstBuffer = retryNative ? { _ in } : nil
    }

    private static func uninstall() {
        AsyncListingProducer.testBeforeDirectory = nil
        AsyncListingProducer.testRecords = nil
        AsyncListingProducer.testWorkersFinished = nil
        AsyncListingProducer.testFallbackAfterFirstBuffer = nil
    }
}

private final class ParallelProbe: @unchecked Sendable {
    private let lock = NSLock()
    private let root: Data
    private let slow: Data?
    private var tickets: [Data: Int64] = [:]
    private var duplicateClaim = false
    private var slowBatches = 0
    private var timedOut = false
    private var finishes = 0
    private let entered = DispatchSemaphore(value: 0)
    private let release = DispatchSemaphore(value: 0)
    private let completion = Signals()

    init(root: Data, slow: Data?) { self.root = root; self.slow = slow }

    func beforeDirectory(_ path: Data, _ ticket: Int64) {
        lock.lock()
        if tickets.updateValue(ticket, forKey: path) != nil { duplicateClaim = true }
        let hold = slow == nil && path != root
        lock.unlock()
        if hold { gate() }
    }

    func records(_ path: Data, _ records: [FileMetadata]) -> [FileMetadata] {
        lock.lock()
        if path == slow { slowBatches += 1 }
        let hold = path == slow && slowBatches == 2
        lock.unlock()
        if hold { gate() }
        return records
    }

    private func gate() {
        entered.signal()
        if release.wait(timeout: .now() + 30) == .timedOut {
            lock.lock(); timedOut = true; lock.unlock()
        }
    }

    func waitForGates(_ count: Int) throws {
        for _ in 0..<count {
            try require(entered.wait(timeout: .now() + 10) == .success, "Parallel discovery did not reach the worker gate")
        }
    }

    func releaseAll() { for _ in 0..<4 { release.signal() } }

    func workersFinished() {
        lock.lock(); finishes += 1; lock.unlock(); completion.signal()
    }

    func waitForWorkers() throws {
        try completion.until("parallel worker shutdown") {
            self.lock.lock(); defer { self.lock.unlock() }; return self.finishes > 0
        }
    }

    func claims() -> [Data: Int64] {
        lock.lock(); defer { lock.unlock() }; return tickets
    }

    func verify() throws {
        lock.lock(); defer { lock.unlock() }
        try require(!timedOut && !duplicateClaim && finishes == 1, "Parallel worker gates timed out, repeated work or finished more than once")
    }
}
