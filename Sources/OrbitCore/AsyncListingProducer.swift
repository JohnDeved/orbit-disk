import Darwin
import Foundation

struct PendingListingCompletion {
    let issueCount: Int
    let issues: [String]
}

struct PendingInventoryBatch {
    var sequence: Int64
    let parentPath: Data
    let parentIdentity: DirectoryIdentity
    let components: [DirectoryComponent]
    let records: [FileMetadata]
    let completion: PendingListingCompletion?
    let failure: Error?
    let logicalSize: Int64
    let allocatedSize: Int64
    let fileCount: Int64
    let index: PendingInventoryBatchIndex
    let byteCount: Int
    let expectedRevision: Int64
    let expectedFullGeneration: Int64

    init(sequence: Int64, parentPath: Data, parentIdentity: DirectoryIdentity,
         components: [DirectoryComponent], records: [FileMetadata],
         completion: PendingListingCompletion?, failure: Error?, expectedRevision: Int64 = 0, expectedFullGeneration: Int64 = 0) {
        self.sequence = sequence; self.parentPath = parentPath; self.parentIdentity = parentIdentity
        self.components = components; self.records = records; self.completion = completion; self.failure = failure
        self.expectedRevision = expectedRevision; self.expectedFullGeneration = expectedFullGeneration
        index = PendingInventoryBatchIndex(records: records)
        logicalSize = index.logicalSize
        allocatedSize = index.allocatedSize
        fileCount = index.fileCount
        byteCount = 256 + parentPath.count + components.reduce(0) { $0 + 64 + $1.name.utf8.count }
            + index.metadataByteCount
            + index.estimatedByteCount
            + (completion?.issues.reduce(0) { $0 + 32 + $1.utf8.count } ?? 0)
    }
}

struct PendingInventorySnapshot {
    let generation: String
    let revision: UInt64
    let rootPath: Data
    let rootIdentity: DirectoryIdentity
    let batches: [PendingInventoryBatch]
    var inaccessibleDirectories: Set<Data> = []
    let current: () -> Bool
    func isCurrent() -> Bool { current() }
}

// Captured batches retain their arrays until a reader finishes its SQLite snapshot.
final class PendingInventoryMailbox: @unchecked Sendable {
    let generation = UUID().uuidString
    let rootPath: Data
    let rootIdentity: DirectoryIdentity
    private let condition = NSCondition()
    private let cancellation: ScanCancellation
    private let onAccepted: () -> Void
    private let onBatch: (PendingInventoryBatch) -> Void
    private static let pageSize = 128
    private var pages: [Int64: [PendingInventoryBatch?]] = [:]
    private var sequence: Int64 = 0
    private var revision: UInt64 = 0
    private var bytes = 0
    private var peakBytes = 0
    private var frontier = 0
    private var peakFrontier = 0
    private var frontierBytes = 0
    private var peakFrontierBytesValue = 0
    private var committed: Int64 = 0
    private var discoveredRecords = 0
    private var discoveredBytes: Int64 = 0
    private var reuseDeferred = 0
    private var retryDeferred = 0
    private var finished = false
    private var finishedAt: TimeInterval?
    private var cancelled = false

    init(rootPath: Data, rootIdentity: DirectoryIdentity,
         cancellation: ScanCancellation, onAccepted: @escaping () -> Void,
         onBatch: @escaping (PendingInventoryBatch) -> Void = { _ in }) {
        self.rootPath = rootPath; self.rootIdentity = rootIdentity
        self.cancellation = cancellation; self.onAccepted = onAccepted; self.onBatch = onBatch
    }

    private func read<T>(_ value: () -> T) -> T {
        condition.lock(); defer { condition.unlock() }; return value()
    }
    var pendingBytes: Int { read { bytes } }
    var peakPendingBytes: Int { read { peakBytes } }
    var pendingCount: Int { read { Int(sequence - committed) } }
    var discoveryFinished: Bool { read { finished } }
    var discoveryFinishedAt: TimeInterval? { read { finishedAt } }
    var lastCommittedSequence: Int64 { read { committed } }
    var discoveredRecordCount: Int { read { discoveredRecords } }
    var discoveredLogicalSize: Int64 { read { discoveredBytes } }
    var reuseDeferredDirectoryCount: Int { read { reuseDeferred } }
    var retryDeferredDirectoryCount: Int { read { retryDeferred } }
    var deferredDirectoryCount: Int { read { reuseDeferred + retryDeferred } }
    var frontierCount: Int { read { frontier } }
    var peakFrontierCount: Int { read { peakFrontier } }
    var pendingFrontierBytes: Int { read { frontierBytes } }
    var peakFrontierBytes: Int { read { peakFrontierBytesValue } }
    var isCancelled: Bool { read { cancelled } || cancellation.isCancelled }
    var isDrained: Bool { read { finished && committed == sequence } }

    func capture() -> PendingInventorySnapshot {
        condition.lock()
        let revision = revision
        let retained = Array(pages)
        condition.unlock()
        // Readers copy page references under the lock; flattening cannot hold up discovery.
        return PendingInventorySnapshot(generation: generation, revision: revision, rootPath: rootPath,
            rootIdentity: rootIdentity, batches: retained.sorted { $0.key < $1.key }.flatMap { $0.value.compactMap { $0 } },
            current: { [weak self] in self.map { !$0.isCancelled } ?? false })
    }

    fileprivate func observe(_ batch: PendingInventoryBatch) throws {
        guard !isCancelled else { throw CancellationError() }
        onBatch(batch)
        condition.lock()
        guard !cancelled && !cancellation.isCancelled else { condition.unlock(); throw CancellationError() }
        discoveredRecords += batch.records.count; discoveredBytes += batch.logicalSize; revision &+= 1
        condition.unlock()
    }

    fileprivate func notifyAccepted() { onAccepted() }

    func enqueue(_ observed: PendingInventoryBatch) throws {
        condition.lock()
        guard !cancelled && !cancellation.isCancelled else { condition.unlock(); throw CancellationError() }
        var batch = observed
        sequence += 1; batch.sequence = sequence
        pages[(sequence - 1) / Int64(Self.pageSize), default: []].append(batch)
        bytes += batch.byteCount
        peakBytes = max(peakBytes, bytes); revision &+= 1
        condition.broadcast(); condition.unlock()
        onAccepted()
    }

    func next(maxRecords: Int) -> [PendingInventoryBatch] {
        condition.lock(); defer { condition.unlock() }
        if committed == sequence && !finished && !cancelled {
            _ = condition.wait(until: Date(timeIntervalSinceNow: 0.025))
        }
        guard !cancelled, committed < sequence, let first = batch(at: committed + 1) else { return [] }
        var result: [PendingInventoryBatch] = []
        var count = 0
        for sequence in (committed + 1)...sequence {
            guard let batch = batch(at: sequence), batch.parentPath == first.parentPath,
                  result.isEmpty || count + batch.records.count <= maxRecords else { break }
            result.append(batch); count += batch.records.count
            if batch.completion != nil || batch.failure != nil || count >= maxRecords { break }
        }
        return result
    }

    func acknowledge(through sequence: Int64) {
        condition.lock()
        let end = min(sequence, self.sequence)
        while committed < end {
            let index = committed
            let page = index / Int64(Self.pageSize), offset = Int(index % Int64(Self.pageSize))
            if let batch = pages[page]?[offset] { bytes -= batch.byteCount }
            pages[page]?[offset] = nil
            committed += 1
            if offset == Self.pageSize - 1 { pages.removeValue(forKey: page) }
        }
        revision &+= 1
        condition.broadcast(); condition.unlock()
    }

    private func batch(at sequence: Int64) -> PendingInventoryBatch? {
        let index = sequence - 1
        return pages[index / Int64(Self.pageSize)]?[Int(index % Int64(Self.pageSize))]
    }

    func updateFrontier(count: Int, bytes: Int) {
        condition.lock()
        guard !cancelled else { condition.unlock(); return }
        frontier = count; frontierBytes = bytes
        peakFrontier = max(peakFrontier, count); peakFrontierBytesValue = max(peakFrontierBytesValue, bytes)
        condition.unlock()
    }

    fileprivate func deferredDirectoriesForReuse(_ count: Int) {
        condition.lock()
        if !cancelled { reuseDeferred += count }
        condition.unlock()
    }

    fileprivate func deferRetries(_ count: Int) {
        condition.lock(); if !cancelled { retryDeferred += count }; condition.unlock()
    }

    func finish() {
        condition.lock()
        guard !cancelled else { condition.unlock(); return }
        finished = true; finishedAt = ProcessInfo.processInfo.systemUptime
        revision &+= 1; condition.broadcast(); condition.unlock(); onAccepted()
    }

    func cancel(notify: Bool = true) {
        condition.lock(); cancelled = true; finished = true; revision &+= 1
        pages.removeAll(); committed = sequence; bytes = 0
        frontier = 0; frontierBytes = 0
        condition.broadcast(); condition.unlock()
        if notify { onAccepted() }
    }

}

final class AsyncListingProducer: @unchecked Sendable {
    #if ORBIT_TESTING
    static var testRecords: ((Data, [FileMetadata]) -> [FileMetadata])?
    static var testBeforeDirectory: ((Data, Int64) -> Void)?
    static var testWorkersFinished: (() -> Void)?
    static var testFallbackAfterFirstBuffer: ((Data) -> Void)?
    private let recordsHook: ((Data, [FileMetadata]) -> [FileMetadata])?
    private let directoryHook: ((Data, Int64) -> Void)?
    private let finishedHook: (() -> Void)?
    private let fallbackHook: ((Data) -> Void)?
    #endif
    private enum ObservationFailure: Error { case repeatedName }
    let mailbox: PendingInventoryMailbox
    private let rootURL: URL
    private let root: SecureRoot
    private let producerCancellation = ScanCancellation()
    private let publicationLock = NSLock()
    private let excludedDirectories: Set<DirectoryIdentity>
    private let excludedPaths: Set<Data>
    private let deferDirectory: ((Data) -> Bool)?
    private let preferredDirectory: (() -> Data?)?
    private let live: LiveMappingInventory?
    private let seedPath: String?
    private let startingGeneration: Int64
    private let reuse: DiscoveryReuse?
    private let forceFallback: Bool
    private let workerCount: Int
    private let scheduler: DiscoveryScheduler
    private let spool: DiscoveryListingSpool
    private let queue = DispatchQueue(label: "local.orbitdisk.discovery", qos: .userInitiated, attributes: .concurrent)

    init(rootURL: URL, rootIdentity: DirectoryIdentity, cancellation: ScanCancellation,
         excludedDirectories: Set<DirectoryIdentity>, excludedPaths: Set<Data>,
         deferDirectory: ((Data) -> Bool)?, preferredDirectory: (() -> Data?)?,
         live: LiveMappingInventory? = nil, seedPath: String? = nil, startingGeneration: Int64 = 0, reuse: DiscoveryReuse? = nil,
         forceFallback: Bool = false, workerCount: Int = 4, onAccepted: @escaping () -> Void) throws {
        self.rootURL = rootURL; root = try SecureRoot(url: rootURL)
        guard root.identity == rootIdentity else { throw SecureTraversalError.identityChanged }
        self.excludedDirectories = excludedDirectories; self.excludedPaths = excludedPaths
        self.deferDirectory = deferDirectory; self.preferredDirectory = preferredDirectory
        self.live = live; self.startingGeneration = startingGeneration
        self.seedPath = seedPath; self.reuse = reuse; self.forceFallback = forceFallback
        self.workerCount = max(1, min(workerCount, 4))
        mailbox = PendingInventoryMailbox(rootPath: Data(rootURL.path.utf8), rootIdentity: rootIdentity,
            cancellation: cancellation, onAccepted: onAccepted, onBatch: { live?.append($0, notify: false) })
        scheduler = DiscoveryScheduler(root: DiscoveryDirectory(path: Data(rootURL.path.utf8),
            identity: rootIdentity, components: []), mailbox: mailbox)
        spool = DiscoveryListingSpool(mailbox: mailbox)
        #if ORBIT_TESTING
        recordsHook = Self.testRecords; directoryHook = Self.testBeforeDirectory
        finishedHook = Self.testWorkersFinished; fallbackHook = Self.testFallbackAfterFirstBuffer
        #endif
    }

    func start() {
        let workers = DispatchGroup()
        for _ in 0..<workerCount {
            workers.enter()
            queue.async { defer { workers.leave() }; self.runWorker() }
        }
        workers.notify(queue: queue) {
            self.spool.finish {
                #if ORBIT_TESTING
                self.finishedHook?()
                #endif
            }
        }
    }

    func cancel() {
        publicationLock.lock()
        producerCancellation.cancel()
        mailbox.cancel(notify: false)
        publicationLock.unlock()
        scheduler.cancel(); spool.cancel()
        mailbox.notifyAccepted()
    }

    private func withPublication(_ operation: () throws -> Void) throws {
        publicationLock.lock(); defer { publicationLock.unlock() }
        guard !producerCancellation.isCancelled, !mailbox.isCancelled else { throw CancellationError() }
        try operation()
    }

    private func updateLive(_ operation: (LiveMappingInventory) -> Void) throws {
        try withPublication { if let live { operation(live) } }
        live?.onChange()
    }

    private func publish(_ batch: PendingInventoryBatch, ticket: Int64, save: Bool = true) throws {
        try withPublication { try mailbox.observe(batch) }
        live?.onChange()
        mailbox.notifyAccepted()
        if save { spool.append(batch, ticket: ticket) }
    }

    private func runWorker() {
        let cursor: SecureDirectoryCursor
        do { cursor = try SecureDirectoryCursor(root: root, rootURL: rootURL) }
        catch { cancel(); return }
        defer { cursor.close() }
        let buffer = StreamingBulkDirectory.Buffer()
        let seed = seedPath.flatMap { try? DiscoveryCacheReader(path: $0, rootIdentity: root.identity, cancellation: producerCancellation) }
        let reuse = reuse?.clone(cancellation: producerCancellation)
        while let job = scheduler.next(preferred: preferredDirectory?()) {
            #if ORBIT_TESTING
            directoryHook?(job.directory.path, job.ticket)
            #endif
            let children = discover(job, cursor: cursor, buffer: buffer, seed: seed, reuse: reuse)
            spool.close(job.ticket)
            scheduler.complete(children: children)
        }
    }

    private func discover(_ job: DiscoveryScheduler.Job, cursor: SecureDirectoryCursor,
                          buffer: StreamingBulkDirectory.Buffer, seed: DiscoveryCacheReader?,
                          reuse: DiscoveryReuse?) -> [DiscoveryDirectory] {
        let directory = job.directory
        var children: [DiscoveryDirectory] = []
        var expectedRevision: Int64 = 0, expectedGeneration = seed?.generation ?? startingGeneration
        func batch(_ records: [FileMetadata], completion: PendingListingCompletion? = nil,
                   failure: Error? = nil) -> PendingInventoryBatch {
            PendingInventoryBatch(sequence: 1, parentPath: directory.path, parentIdentity: directory.identity,
                components: directory.components, records: records, completion: completion, failure: failure,
                expectedRevision: expectedRevision, expectedFullGeneration: expectedGeneration)
        }
        do {
            try updateLive { $0.begin(directory.path, identity: directory.identity, notify: false) }
            let queued = ListingStage.QueuedDirectory(path: directory.path, device: directory.identity.device, inode: directory.identity.inode)
            let saved = try? seed?.listing(queued)
            expectedRevision = saved?.revision ?? 0
            expectedGeneration = saved?.generation ?? seed?.generation ?? startingGeneration
            let descriptor = try cursor.openDirectory(directory.components)
            defer { close(descriptor) }
            var issueCount = 0
            var issues: [String] = []
            var tail: [FileMetadata] = []
            var names = Set<Int>()
            func accept(_ observations: [FileMetadata]) throws -> [FileMetadata] {
                let priorCount = children.count
                var accepted: [FileMetadata]?
                var deferred = 0
                for (offset, record) in observations.enumerated() {
                    guard names.insert(record.name.hashValue).inserted else {
                        children.removeSubrange(priorCount...)
                        throw ObservationFailure.repeatedName
                    }
                    if !record.isDirectory { accepted?.append(record); continue }
                    let path = HardLinkPathOrder.path(directory.path, Data(record.name.utf8))
                    let identity = DirectoryIdentity(device: record.device, inode: record.inode)
                    if excludedPaths.contains(path) || excludedDirectories.contains(identity) {
                        if accepted == nil {
                            accepted = Array(observations[..<offset]); accepted?.reserveCapacity(observations.count)
                        }
                        continue
                    }
                    accepted?.append(record)
                    if !record.isMountPoint && record.device == root.identity.device {
                        if deferDirectory?(path) == true { deferred += 1; continue }
                        children.append(DiscoveryDirectory(path: path, identity: identity,
                            components: directory.components + [DirectoryComponent(name: record.name, expected: identity)]))
                    }
                }
                if deferred > 0 { mailbox.deferredDirectoriesForReuse(deferred) }
                return accepted ?? observations
            }
            var cachedRecords = saved?.records
            var reused: StageReusableListing?
            if cachedRecords == nil, expectedGeneration == 0, expectedRevision == 0 {
                do {
                    reused = try reuse?.listing(queued)
                    if let reused {
                        var records: [FileMetadata] = []
                        while true {
                            if mailbox.isCancelled { throw CancellationError() }
                            let page = try reused.page(limit: 1024)
                            records.append(contentsOf: page)
                            if page.count < 1024 { break }
                        }
                        cachedRecords = records
                    }
                } catch { cachedRecords = nil; reused = nil }
            }
            if let cachedRecords {
                do {
                    let accepted = try accept(cachedRecords)
                    let observed = batch(accepted, completion: PendingListingCompletion(issueCount: 0, issues: []))
                    if let reused { try reused.source.commit { try publish(observed, ticket: job.ticket, save: false) } }
                    else { try publish(observed, ticket: job.ticket, save: false) }
                    return children
                } catch {
                    children.removeAll(keepingCapacity: true); names.removeAll(keepingCapacity: true)
                }
            }
            func stream(fallback: Bool) throws {
                var forceFailure: (() -> Void)?
                #if ORBIT_TESTING
                if !fallback, let fallbackHook { forceFailure = { fallbackHook(directory.path) } }
                #endif
                try StreamingBulkDirectory.stream(descriptor,
                    url: URL(fileURLWithPath: String(decoding: directory.path, as: UTF8.self), isDirectory: true),
                    cancellation: producerCancellation, progress: { _, _ in }, issue: {
                        issueCount += 1; if issues.count < 100 { issues.append($0) }
                    }, forceFallback: fallback, testFallbackAfterFirstBuffer: forceFailure, buffer: buffer) { records in
                        guard !self.mailbox.isCancelled else { throw CancellationError() }
                        #if ORBIT_TESTING
                        let observations = self.recordsHook?(directory.path, records) ?? records
                        #else
                        let observations = records
                        #endif
                        let accepted = try accept(observations)
                        if records.count < 128 { tail = accepted }
                        else { try self.publish(batch(accepted), ticket: job.ticket) }
                    }
            }
            do { try stream(fallback: forceFallback) }
            catch StreamingBulkDirectory.Failure.needsFallback {
                try publish(batch([], failure: StreamingBulkDirectory.Failure.needsFallback), ticket: job.ticket)
                children.removeAll(keepingCapacity: true); names.removeAll(keepingCapacity: true)
                tail.removeAll(); issues.removeAll(); issueCount = 0
                try updateLive { $0.begin(directory.path, identity: directory.identity, notify: false) }
                try stream(fallback: true)
            }
            try root.validate(url: rootURL)
            try publish(batch(tail, completion: PendingListingCompletion(issueCount: issueCount, issues: issues)), ticket: job.ticket)
        } catch {
            guard !mailbox.isCancelled else { return [] }
            var code: Int32?
            var missing = false
            if let secure = error as? SecureTraversalError {
                switch secure {
                case .filesystem(let value): code = value
                case .identityChanged: missing = true
                case .invalidComponent: break
                }
            } else if let scan = error as? ScanError, case .filesystem(_, let value) = scan { code = value }
            if missing || code == ENOENT || code == ENOTDIR || code == ELOOP {
                try? updateLive { $0.unavailable(directory.path, identity: directory.identity, missing: true, notify: false) }
                children.removeAll()
            } else if code == EACCES || code == EPERM {
                try? updateLive { $0.unavailable(directory.path, identity: directory.identity, missing: false, notify: false) }
                children.removeAll()
            }
            mailbox.deferRetries(1)
            do { try publish(batch([], failure: error), ticket: job.ticket) }
            catch { return [] }
        }
        return children
    }
}
