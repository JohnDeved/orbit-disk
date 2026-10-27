import Darwin
import Foundation

struct PendingListingCompletion {
    let issueCount: Int
    let issues: [String]
}

struct PendingInventoryBatch {
    let sequence: Int64
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

    init(sequence: Int64, parentPath: Data, parentIdentity: DirectoryIdentity,
         components: [DirectoryComponent], records: [FileMetadata],
         completion: PendingListingCompletion?, failure: Error?) {
        self.sequence = sequence; self.parentPath = parentPath; self.parentIdentity = parentIdentity
        self.components = components; self.records = records; self.completion = completion; self.failure = failure
        logicalSize = records.reduce(0) { $0 + ($1.isDirectory ? 0 : $1.size) }
        allocatedSize = records.reduce(0) { $0 + ($1.isDirectory ? 0 : $1.allocatedSize) }
        fileCount = records.reduce(0) { $0 + ($1.isDirectory ? 0 : 1) }
        index = PendingInventoryBatchIndex(records: records)
        byteCount = 256 + parentPath.count + components.reduce(0) { $0 + 64 + $1.name.utf8.count }
            + records.reduce(0) { $0 + 160 + $1.name.utf8.count * 2 }
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
    fileprivate let current: () -> Bool
    func isCurrent() -> Bool { current() }
}

// Captured batches retain their arrays until a reader finishes its SQLite snapshot.
final class PendingInventoryMailbox: @unchecked Sendable {
    let generation = UUID().uuidString
    let rootPath: Data
    let rootIdentity: DirectoryIdentity
    let byteLimit: Int
    private let condition = NSCondition()
    private let cancellation: ScanCancellation
    private let onAccepted: () -> Void
    private var batches: [PendingInventoryBatch?] = []
    private var head = 0
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
    private var capacityDeferred = 0
    private var reuseDeferred = 0
    private var retryDeferred = 0
    private var finished = false
    private var finishedAt: TimeInterval?
    private var cancelled = false

    init(rootPath: Data, rootIdentity: DirectoryIdentity, byteLimit: Int,
         cancellation: ScanCancellation, onAccepted: @escaping () -> Void) {
        self.rootPath = rootPath; self.rootIdentity = rootIdentity
        self.byteLimit = max(256 * 1024, byteLimit)
        self.cancellation = cancellation; self.onAccepted = onAccepted
    }

    private func read<T>(_ value: () -> T) -> T {
        condition.lock(); defer { condition.unlock() }; return value()
    }
    var pendingBytes: Int { read { bytes } }
    var peakPendingBytes: Int { read { peakBytes } }
    var pendingCount: Int { read { batches.count - head } }
    var discoveryFinished: Bool { read { finished } }
    var discoveryFinishedAt: TimeInterval? { read { finishedAt } }
    var lastCommittedSequence: Int64 { read { committed } }
    var discoveredRecordCount: Int { read { discoveredRecords } }
    var discoveredLogicalSize: Int64 { read { discoveredBytes } }
    var capacityDeferredDirectoryCount: Int { read { capacityDeferred } }
    var reuseDeferredDirectoryCount: Int { read { reuseDeferred } }
    var retryDeferredDirectoryCount: Int { read { retryDeferred } }
    var deferredDirectoryCount: Int { read { capacityDeferred + reuseDeferred + retryDeferred } }
    var frontierCount: Int { read { frontier } }
    var peakFrontierCount: Int { read { peakFrontier } }
    var pendingFrontierBytes: Int { read { frontierBytes } }
    var peakFrontierBytes: Int { read { peakFrontierBytesValue } }
    var isCancelled: Bool { read { cancelled } || cancellation.isCancelled }
    var isDrained: Bool { read { finished && head == batches.count } }

    func capture() -> PendingInventorySnapshot {
        condition.lock(); defer { condition.unlock() }
        return PendingInventorySnapshot(generation: generation, revision: revision, rootPath: rootPath,
            rootIdentity: rootIdentity, batches: cancelled ? [] : batches[head...].compactMap { $0 },
            current: { [weak self] in self.map { !$0.isCancelled } ?? false })
    }

    fileprivate func enqueue(parentPath: Data, parentIdentity: DirectoryIdentity, components: [DirectoryComponent],
                             records: [FileMetadata], completion: PendingListingCompletion? = nil,
                             failure: Error? = nil) throws {
        condition.lock()
        let batch = PendingInventoryBatch(sequence: sequence + 1, parentPath: parentPath,
            parentIdentity: parentIdentity, components: components, records: records,
            completion: completion, failure: failure)
        // A single long pathname can exceed the budget; defer it to the durable queue instead.
        guard batch.byteCount <= byteLimit else { condition.unlock(); throw ProducerFailure.oversizedBatch }
        while !cancelled && !cancellation.isCancelled && bytes + batch.byteCount > byteLimit {
            _ = condition.wait(until: Date(timeIntervalSinceNow: 0.05))
        }
        guard !cancelled && !cancellation.isCancelled else { condition.unlock(); throw CancellationError() }
        sequence = batch.sequence; batches.append(batch); bytes += batch.byteCount
        discoveredRecords += records.count; discoveredBytes += batch.logicalSize
        peakBytes = max(peakBytes, bytes); revision &+= 1
        condition.broadcast(); condition.unlock()
        onAccepted()
    }

    func next(maxRecords: Int) -> [PendingInventoryBatch] {
        condition.lock(); defer { condition.unlock() }
        if head == batches.count && !finished && !cancelled {
            _ = condition.wait(until: Date(timeIntervalSinceNow: 0.025))
        }
        guard !cancelled, head < batches.count, let first = batches[head] else { return [] }
        var result: [PendingInventoryBatch] = []
        var count = 0
        for stored in batches[head...] {
            guard let batch = stored, batch.parentPath == first.parentPath,
                  result.isEmpty || count + batch.records.count <= maxRecords else { break }
            result.append(batch); count += batch.records.count
            if batch.completion != nil || batch.failure != nil || count >= maxRecords { break }
        }
        return result
    }

    func acknowledge(through sequence: Int64) {
        condition.lock()
        while head < batches.count, let batch = batches[head], batch.sequence <= sequence {
            bytes -= batch.byteCount; batches[head] = nil; head += 1
        }
        committed = max(committed, sequence); revision &+= 1
        if head == batches.count { batches.removeAll(keepingCapacity: true); head = 0 }
        else if head >= 256 { batches.removeFirst(head); head = 0 }
        condition.broadcast(); condition.unlock()
    }

    fileprivate func updateFrontier(count: Int, bytes: Int) {
        condition.lock(); frontier = count; frontierBytes = bytes
        peakFrontier = max(peakFrontier, count); peakFrontierBytesValue = max(peakFrontierBytesValue, bytes)
        condition.unlock()
    }

    fileprivate func deferredDirectory(forReuse: Bool) {
        condition.lock()
        if forReuse { reuseDeferred += 1 } else { capacityDeferred += 1 }
        condition.unlock()
    }

    fileprivate func deferRetries(_ count: Int) {
        condition.lock(); retryDeferred += count; condition.unlock()
    }

    fileprivate func finish() {
        condition.lock(); finished = true; finishedAt = ProcessInfo.processInfo.systemUptime
        revision &+= 1; condition.broadcast(); condition.unlock(); onAccepted()
    }

    func cancel() {
        condition.lock(); cancelled = true; finished = true; revision &+= 1
        batches.removeAll(); head = 0; bytes = 0
        condition.broadcast(); condition.unlock(); onAccepted()
    }

    fileprivate enum ProducerFailure: Error { case oversizedBatch }
}

final class AsyncListingProducer: @unchecked Sendable {
    #if ORBIT_TESTING
    static var testRecords: ((Data, [FileMetadata]) -> [FileMetadata])?
    #endif
    private enum ObservationFailure: Error { case repeatedName, listingLimit }
    private struct Directory {
        let path: Data
        let identity: DirectoryIdentity
        let components: [DirectoryComponent]
        var byteCount: Int { 128 + path.count + components.reduce(0) { $0 + 64 + $1.name.utf8.count } }
    }
    let mailbox: PendingInventoryMailbox
    private let rootURL: URL
    private let root: SecureRoot
    private let cursor: SecureDirectoryCursor
    private let bulkBuffer = StreamingBulkDirectory.Buffer()
    private let cancellation: ScanCancellation
    private let producerCancellation = ScanCancellation()
    private let excludedDirectories: Set<DirectoryIdentity>
    private let excludedPaths: Set<Data>
    private let deferDirectory: ((Data) -> Bool)?
    private let preferredDirectory: (() -> Data?)?
    private let frontierLimit: Int
    private let frontierByteLimit = 4 * 1024 * 1024
    private let queue = DispatchQueue(label: "local.orbitdisk.discovery", qos: .userInitiated)

    init(rootURL: URL, rootIdentity: DirectoryIdentity, cancellation: ScanCancellation,
         excludedDirectories: Set<DirectoryIdentity>, excludedPaths: Set<Data>,
         deferDirectory: ((Data) -> Bool)?, preferredDirectory: (() -> Data?)?, byteLimit: Int, frontierLimit: Int,
         onAccepted: @escaping () -> Void) throws {
        self.rootURL = rootURL; root = try SecureRoot(url: rootURL)
        guard root.identity == rootIdentity else { throw SecureTraversalError.identityChanged }
        cursor = try SecureDirectoryCursor(root: root, rootURL: rootURL)
        self.cancellation = cancellation; self.excludedDirectories = excludedDirectories
        self.excludedPaths = excludedPaths; self.deferDirectory = deferDirectory; self.preferredDirectory = preferredDirectory
        self.frontierLimit = max(1, frontierLimit)
        mailbox = PendingInventoryMailbox(rootPath: Data(rootURL.path.utf8), rootIdentity: rootIdentity,
            byteLimit: byteLimit, cancellation: cancellation, onAccepted: onAccepted)
    }

    func start() { queue.async { self.discover() } }
    func cancel() { producerCancellation.cancel(); mailbox.cancel() }

    private func discover() {
        let first = Directory(path: Data(rootURL.path.utf8), identity: root.identity, components: [])
        var frontier = [first]
        var frontierBytes = first.byteCount
        var lastPreference: Data?
        var checkPreference = false
        mailbox.updateFrontier(count: 1, bytes: frontierBytes)
        defer { cursor.close(); mailbox.updateFrontier(count: 0, bytes: 0); mailbox.finish() }
        while !frontier.isEmpty && !mailbox.isCancelled {
            let preference = preferredDirectory?()
            if preference != lastPreference { lastPreference = preference; checkPreference = true }
            var selected = frontier.count - 1
            if checkPreference, let preference {
                if let index = frontier.firstIndex(where: { candidate in
                    if candidate.path == preference { return true }
                    guard preference.starts(with: candidate.path) else { return false }
                    return candidate.path == Data([47]) || (preference.count > candidate.path.count && preference[candidate.path.count] == 47)
                }) {
                    selected = index
                    checkPreference = frontier[index].path != preference
                } else { checkPreference = false }
            }
            let directory = frontier.remove(at: selected)
            frontierBytes -= directory.byteCount
            mailbox.updateFrontier(count: frontier.count, bytes: frontierBytes)
            do {
                let descriptor = try cursor.openDirectory(directory.components)
                defer { close(descriptor) }
                var issueCount = 0
                var issues: [String] = []
                var tail: [FileMetadata] = []
                var names = Set<Int>()
                try StreamingBulkDirectory.stream(descriptor,
                    url: URL(fileURLWithPath: String(decoding: directory.path, as: UTF8.self), isDirectory: true),
                    cancellation: producerCancellation, progress: { _, _ in }, issue: {
                        issueCount += 1; if issues.count < 100 { issues.append($0) }
                    }, buffer: bulkBuffer) { records in
                        guard !self.mailbox.isCancelled else { throw CancellationError() }
                        #if ORBIT_TESTING
                        let observations = Self.testRecords?(directory.path, records) ?? records
                        #else
                        let observations = records
                        #endif
                        // A collision defers this listing; it cannot introduce a duplicate into the RAM totals.
                        for record in observations {
                            guard names.count < 131_072 else { throw ObservationFailure.listingLimit }
                            guard names.insert(record.name.hashValue).inserted else { throw ObservationFailure.repeatedName }
                        }
                        var accepted: [FileMetadata] = []
                        accepted.reserveCapacity(observations.count)
                        for record in observations {
                            if !record.isDirectory { accepted.append(record); continue }
                            var path = directory.path
                            if path.last != 47 { path.append(47) }
                            path.append(contentsOf: record.name.utf8)
                            let identity = DirectoryIdentity(device: record.device, inode: record.inode)
                            if record.isDirectory && (self.excludedPaths.contains(path) || self.excludedDirectories.contains(identity)) { continue }
                            accepted.append(record)
                            if !record.isMountPoint && record.device == self.root.identity.device {
                                if frontier.count >= self.frontierLimit {
                                    self.mailbox.deferredDirectory(forReuse: false)
                                    continue
                                }
                                if self.deferDirectory?(path) == true {
                                    self.mailbox.deferredDirectory(forReuse: true)
                                    continue
                                }
                                let child = Directory(path: path, identity: identity,
                                    components: directory.components + [DirectoryComponent(name: record.name, expected: identity)])
                                if frontierBytes + child.byteCount <= self.frontierByteLimit {
                                    frontier.append(child); frontierBytes += child.byteCount
                                } else { self.mailbox.deferredDirectory(forReuse: false) }
                            }
                        }
                        self.mailbox.updateFrontier(count: frontier.count, bytes: frontierBytes)
                        if records.count < 128 { tail = accepted }
                        else {
                            try self.mailbox.enqueue(parentPath: directory.path, parentIdentity: directory.identity,
                                components: directory.components, records: accepted)
                        }
                    }
                try root.validate(url: rootURL)
                try mailbox.enqueue(parentPath: directory.path, parentIdentity: directory.identity,
                    components: directory.components, records: tail,
                    completion: PendingListingCompletion(issueCount: issueCount, issues: issues))
            } catch {
                guard !mailbox.isCancelled else { return }
                mailbox.deferRetries(1)
                do {
                    try mailbox.enqueue(parentPath: directory.path, parentIdentity: directory.identity,
                        components: directory.components, records: [], failure: error)
                } catch { mailbox.deferRetries(frontier.count); return }
                if error is CancellationError { return }
                if let failure = error as? StreamingBulkDirectory.Failure, case .needsFallback = failure {
                    mailbox.deferRetries(frontier.count)
                    return
                }
            }
        }
    }
}
