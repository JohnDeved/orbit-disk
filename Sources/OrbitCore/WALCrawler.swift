import Darwin
import Foundation

final class DurableCrawler {
    let rootURL: URL
    let rootPath: Data
    let root: SecureRoot
    let store: ListingStage
    var lifetimeLease: StageLease?
    let cancellation: ScanCancellation
    let writeChunkSize: Int
    let excludedDirectories: Set<DirectoryIdentity>
    let excludedPaths: Set<Data>
    let liveInventory: LiveMappingInventory
    var forceFallback = false
    var reusableListing: ((ListingStage.QueuedDirectory) throws -> StageReusableListing?)?
    var preferredDirectory: (() -> Data?)?
    private let asyncLock = NSLock()
    private var asyncProducer: AsyncListingProducer?
    private var asyncActive = false
    private var asyncListing: (path: Data, epoch: Int64?, batches: Int, skipped: Bool)?
    private var asyncWriterCursor: SecureDirectoryCursor?
    var asyncInventory: PendingInventoryMailbox? {
        asyncLock.lock(); defer { asyncLock.unlock() }
        guard let mailbox = asyncProducer?.mailbox, !mailbox.isCancelled else { return nil }
        return mailbox
    }
    #if ORBIT_TESTING
    var testFallbackAfterFirstBuffer: (() -> Void)?
    var testBeforeListing: (() -> Void)?
    #endif
    enum Interrupt: Error { case afterBatch }

    init(rootURL: URL, database: String, checkpoint: StageCheckpoint,
         cancellation: ScanCancellation = ScanCancellation(), writeChunkSize: Int = 1024, mirror: Bool = false,
         expectedSnapshot: SnapshotExpectation? = nil, generation: String? = nil, excluding: [URL] = []) throws {
        let cloudRoots = CloudStoragePolicy.scanExclusions()
        if let reason = CloudStoragePolicy.exclusionReason(for: rootURL, excluding: cloudRoots) {
            throw ScanError.excluded(reason)
        }
        let excluding = excluding + cloudRoots
        self.rootURL = rootURL.standardizedFileURL
        self.rootPath = Data(self.rootURL.path.utf8)
        self.root = try CloudStoragePolicy.withoutMaterialization { try SecureRoot(url: rootURL.standardizedFileURL) }
        self.liveInventory = LiveMappingInventory(root: self.rootURL, identity: self.root.identity, cancellation: cancellation)
        self.store = try ListingStage(path: database, mirror: mirror)
        self.cancellation = cancellation
        self.writeChunkSize = max(128, writeChunkSize)
        self.excludedDirectories = try CloudStoragePolicy.withoutMaterialization {
            Set(excluding.compactMap { url in
                var info = stat()
                guard lstat(url.path, &info) == 0, (info.st_mode & S_IFMT) == S_IFDIR else { return nil }
                return DirectoryIdentity(device: info.st_dev, inode: info.st_ino)
            })
        }
        self.excludedPaths = try CloudStoragePolicy.withoutMaterialization { IndexLocations.exclusionPaths(excluding) }
        try CloudStoragePolicy.withoutMaterialization { try root.validate(url: self.rootURL) }
        try store.preserveScanStart(root: rootPath, identity: checkpoint.identity, eventID: checkpoint.eventID, expected: expectedSnapshot, generation: generation)
        try store.registerDirectory(path: rootPath, device: root.identity.device, inode: root.identity.inode)
        try store.initializeMirrorRoot(url: self.rootURL, identity: root.identity)
    }

    deinit { cancelAsyncDiscovery(); store.close() }

    @discardableResult
    func startAsyncDiscovery(deferDirectory: ((Data) -> Bool)? = nil, reuse: DiscoveryReuse? = nil, workerCount: Int = 4,
                             onAccepted: @escaping () -> Void = {}) throws -> Bool {
        guard !cancellation.isCancelled else { return false }
        #if ORBIT_TESTING
        if testBeforeListing != nil || testFallbackAfterFirstBuffer != nil { return false }
        #endif
        let producer = try AsyncListingProducer(rootURL: rootURL, rootIdentity: root.identity,
            cancellation: cancellation, excludedDirectories: excludedDirectories, excludedPaths: excludedPaths,
            deferDirectory: deferDirectory, preferredDirectory: preferredDirectory, live: liveInventory,
            seedPath: store.databasePath, startingGeneration: store.fullRefreshState().generation, reuse: reuse, forceFallback: forceFallback,
            workerCount: workerCount, onAccepted: onAccepted)
        asyncLock.lock()
        guard asyncProducer == nil else { asyncLock.unlock(); return false }
        asyncProducer = producer; asyncActive = true
        asyncLock.unlock()
        producer.start()
        return true
    }

    func cancelAsyncDiscovery() {
        asyncLock.lock(); asyncActive = false; let producer = asyncProducer; asyncLock.unlock()
        producer?.cancel()
    }

    var hasActiveAsyncDiscovery: Bool {
        asyncLock.lock(); defer { asyncLock.unlock() }; return asyncActive
    }

    // One writer step consumes one transaction group; callbacks may invalidate the remaining queue.
    func stepAsync(beforeAccounting: (() throws -> Void)? = nil, onRecordCount: ((Int) -> Void)? = nil,
                   onBatch: ((Data, Int) throws -> Void)? = nil, onListingComplete: (() -> Void)? = nil,
                   onListingStart: ((Data, String) -> Void)? = nil, onListingEnd: (() -> Void)? = nil) throws -> Bool {
        // Cancellation may originate on another thread; cursor teardown stays on the writer.
        defer { if !hasActiveAsyncDiscovery { asyncWriterCursor = nil } }
        do {
            return try CloudStoragePolicy.withoutMaterialization {
                try consumeAsync(beforeAccounting: beforeAccounting, onRecordCount: onRecordCount,
                    onBatch: onBatch, onListingComplete: onListingComplete,
                    onListingStart: onListingStart, onListingEnd: onListingEnd)
            }
        } catch { cancelAsyncDiscovery(); throw error }
    }

    private func consumeAsync(beforeAccounting: (() throws -> Void)?, onRecordCount: ((Int) -> Void)?,
                              onBatch: ((Data, Int) throws -> Void)?, onListingComplete: (() -> Void)?,
                              onListingStart: ((Data, String) -> Void)?, onListingEnd: (() -> Void)?) throws -> Bool {
        guard !cancellation.isCancelled else { cancelAsyncDiscovery(); throw CancellationError() }
        guard hasActiveAsyncDiscovery, let mailbox = asyncInventory else { return false }
        try root.validate(url: rootURL)
        // Finish marking the old directory queue before accepting this generation's listings.
        // Otherwise their completions are rejected and every discovered folder is walked again.
        if try store.markFullRefreshChunk() { return true }
        if asyncListing == nil {
            let leaves = mailbox.nextFreshLeaves()
            if leaves.count > 1, try consumeFreshLeaves(leaves, mailbox: mailbox,
                beforeAccounting: beforeAccounting, onRecordCount: onRecordCount, onBatch: onBatch,
                onListingComplete: onListingComplete, onListingStart: onListingStart, onListingEnd: onListingEnd) {
                return true
            }
        }
        let pending = mailbox.next(maxRecords: asyncListing == nil ? 128 : writeChunkSize)
        guard let first = pending.first, let last = pending.last else {
            if mailbox.isDrained || mailbox.isCancelled {
                asyncLock.lock(); asyncActive = false; asyncLock.unlock()
                asyncListing = nil
                return false
            }
            return true
        }
        onListingStart?(first.parentPath, "\(first.parentIdentity.device):\(first.parentIdentity.inode)")
        defer { onListingEnd?() }
        guard try store.fullRefreshState().generation == first.expectedFullGeneration else { cancelAsyncDiscovery(); return true }
        if asyncListing == nil {
            var skipped = false
            if let directory = try store.directory(path: first.parentPath),
               directory.device == first.parentIdentity.device, directory.inode == first.parentIdentity.inode {
                do {
                    if asyncWriterCursor == nil { asyncWriterCursor = try SecureDirectoryCursor(root: root, rootURL: rootURL) }
                    let descriptor = try asyncWriterCursor!.openDirectory(first.components)
                    close(descriptor)
                }
                catch { skipped = true }
            } else { skipped = true }
            asyncListing = (first.parentPath, nil, 0, skipped)
        }
        guard var listing = asyncListing, listing.path == first.parentPath else {
            cancelAsyncDiscovery(); return true
        }
        let records = pending.flatMap(\.records)
        let commit = AsyncInventoryCommit(generation: mailbox.generation, sequence: last.sequence)
        var completed = false
        let ended = last.completion != nil || last.failure != nil
        do {
            if listing.skipped {
                try store.acknowledgeAsync(commit)
            } else {
                if let completion = last.completion, listing.epoch == nil, records.count < 128 {
                    completed = try store.completeFreshListing(parent: first.parentPath, records: records,
                        rootDevice: root.identity.device, expectedRevision: first.expectedRevision, expectedFullGeneration: first.expectedFullGeneration,
                        issueCount: completion.issueCount, issues: completion.issues, asyncCommit: commit)
                }
                if !completed {
                    if !records.isEmpty || (last.completion == nil && last.failure == nil) {
                        listing.epoch = try store.append(parent: first.parentPath, epoch: listing.epoch,
                            records: records, rootDevice: root.identity.device, asyncCommit: commit)
                    }
                    if let completion = last.completion {
                        completed = try store.finish(parent: first.parentPath, epoch: listing.epoch,
                            expectedRevision: first.expectedRevision, expectedFullGeneration: first.expectedFullGeneration, issueCount: completion.issueCount,
                            issues: completion.issues, asyncCommit: commit)
                    } else if last.failure != nil && records.isEmpty {
                        try store.acknowledgeAsync(commit)
                    }
                }
            }
        } catch ListingStage.Reconcile.pruneRequired {
            listing.skipped = true
            try store.acknowledgeAsync(commit)
        } catch { cancelAsyncDiscovery(); throw error }
        listing.batches += 1
        if listing.skipped || (last.completion != nil && !completed) || last.failure != nil {
            mailbox.requireReconciliation()
        }
        asyncListing = ended ? nil : listing
        onRecordCount?(records.count)
        do {
            try beforeAccounting?()
            while try store.reconcileLinksChunk(cancellation: cancellation) { }
        } catch { cancelAsyncDiscovery(); throw error }
        mailbox.acknowledge(through: last.sequence)
        do { try onBatch?(first.parentPath, listing.batches) }
        catch StreamingBulkDirectory.Failure.needsFallback {
            cancelAsyncDiscovery()
            if completed { try store.markDirty(parent: first.parentPath) }
            return true
        } catch { cancelAsyncDiscovery(); throw error }
        if completed { onListingComplete?() }
        return true
    }

    private func consumeFreshLeaves(_ leaves: [PendingInventoryBatch], mailbox: PendingInventoryMailbox,
                                    beforeAccounting: (() throws -> Void)?, onRecordCount: ((Int) -> Void)?,
                                    onBatch: ((Data, Int) throws -> Void)?, onListingComplete: (() -> Void)?,
                                    onListingStart: ((Data, String) -> Void)?, onListingEnd: (() -> Void)?) throws -> Bool {
        var listings: [ListingStage.FreshLeafListing] = []
        for leaf in leaves {
            guard !cancellation.isCancelled else { throw CancellationError() }
            guard let directory = try store.directory(path: leaf.parentPath),
                  directory.device == leaf.parentIdentity.device, directory.inode == leaf.parentIdentity.inode,
                  let completion = leaf.completion,
                  try store.canCompleteFreshListing(parent: leaf.parentPath) else { break }
            do {
                if asyncWriterCursor == nil { asyncWriterCursor = try SecureDirectoryCursor(root: root, rootURL: rootURL) }
                let descriptor = try asyncWriterCursor!.openDirectory(leaf.components)
                close(descriptor)
            } catch { break }
            listings.append(ListingStage.FreshLeafListing(directory: directory, records: leaf.records,
                expectedRevision: leaf.expectedRevision, expectedFullGeneration: leaf.expectedFullGeneration,
                issueCount: completion.issueCount, issues: completion.issues,
                asyncCommit: AsyncInventoryCommit(generation: mailbox.generation, sequence: leaf.sequence)))
        }
        guard listings.count > 1 else { return false }
        let committed = try store.completeFreshLeafListings(listings, rootDevice: root.identity.device, cancellation: cancellation)
        guard committed > 0 else { return false }
        let saved = leaves.prefix(committed)
        onRecordCount?(saved.reduce(0) { $0 + $1.records.count })
        try beforeAccounting?()
        while try store.reconcileLinksChunk(cancellation: cancellation) {}
        mailbox.acknowledge(through: leaves[committed - 1].sequence)
        for leaf in saved {
            guard !cancellation.isCancelled else { throw CancellationError() }
            onListingStart?(leaf.parentPath, "\(leaf.parentIdentity.device):\(leaf.parentIdentity.inode)")
            do {
                defer { onListingEnd?() }
                try onBatch?(leaf.parentPath, 1)
                onListingComplete?()
            } catch StreamingBulkDirectory.Failure.needsFallback {
                cancelAsyncDiscovery()
                try store.markDirty(parent: leaf.parentPath)
                return true
            }
        }
        return true
    }

    func markChanged(directory: String) throws {
        asyncInventory?.requireReconciliation()
        var path = Data(directory.utf8)
        var rootPrefix = rootPath
        if rootPrefix.last != 47 { rootPrefix.append(47) }
        guard path == rootPath || path.starts(with: rootPrefix) else {
            try store.markFullRefresh()
            return
        }
        while try store.directory(path: path) == nil {
            guard path != rootPath, let slash = path.lastIndex(of: 47) else { throw SecureTraversalError.invalidComponent }
            path = slash == 0 ? Data([47]) : Data(path.prefix(upTo: slash))
        }
        if path != rootPath, let slash = path.lastIndex(of: 47) {
            let parent = slash == 0 ? Data([47]) : Data(path.prefix(upTo: slash))
            if try store.directory(path: parent) != nil { try store.markDirty(parent: parent) }
        }
        try store.markDirty(parent: path)
    }

    func step(maxBatches: Int? = nil, beforeAccounting: (() throws -> Void)? = nil,
              onRecordCount: ((Int) -> Void)? = nil, onBatch: ((Data, Int) throws -> Void)? = nil,
              onListingComplete: (() -> Void)? = nil,
              onListingStart: ((Data, String) -> Void)? = nil, onListingEnd: (() -> Void)? = nil,
              onEnumerationStart: (() -> Void)? = nil) throws -> Bool {
        try CloudStoragePolicy.withoutMaterialization {
            try stepLocal(maxBatches: maxBatches, beforeAccounting: beforeAccounting, onRecordCount: onRecordCount,
                onBatch: onBatch, onListingComplete: onListingComplete, onListingStart: onListingStart,
                onListingEnd: onListingEnd, onEnumerationStart: onEnumerationStart)
        }
    }

    private func stepLocal(maxBatches: Int?, beforeAccounting: (() throws -> Void)?,
                           onRecordCount: ((Int) -> Void)?, onBatch: ((Data, Int) throws -> Void)?,
                           onListingComplete: (() -> Void)?, onListingStart: ((Data, String) -> Void)?,
                           onListingEnd: (() -> Void)?, onEnumerationStart: (() -> Void)?) throws -> Bool {
        guard !cancellation.isCancelled else { asyncWriterCursor = nil; cancelAsyncDiscovery(); throw CancellationError() }
        if hasActiveAsyncDiscovery {
            if maxBatches != nil { cancelAsyncDiscovery() }
            else if try stepAsync(beforeAccounting: beforeAccounting, onRecordCount: onRecordCount, onBatch: onBatch,
                                 onListingComplete: onListingComplete, onListingStart: onListingStart,
                                 onListingEnd: onListingEnd) { return true }
        }
        asyncWriterCursor = nil
        try root.validate(url: rootURL)
        if try store.markFullRefreshChunk() { return true }
        if try store.pruneOneChunk() { return true }
        if try store.trimOneChunk() { return true }
        if try store.reconcileLinksChunk(cancellation: cancellation) { return true }
        guard let directory = try store.nextDirectory(preferred: preferredDirectory?()) else { return false }
        guard let path = String(data: directory.path, encoding: .utf8),
              let rootString = String(data: rootPath, encoding: .utf8) else { throw SecureTraversalError.invalidComponent }
        let prefix = rootString == "/" ? "/" : rootString + "/"
        let relative: Substring
        if directory.path == rootPath { relative = "" }
        else {
            guard directory.path.starts(with: Data(prefix.utf8)) else { throw SecureTraversalError.invalidComponent }
            relative = path.dropFirst(prefix.count)
        }
        var components: [DirectoryComponent] = []
        var accumulated = rootPath
        for part in relative.split(separator: "/", omittingEmptySubsequences: true) {
            let name = String(part)
            guard !name.isEmpty else { throw SecureTraversalError.invalidComponent }
            if accumulated.last != 47 { accumulated.append(47) }
            accumulated.append(contentsOf: name.utf8)
            guard let expected = try store.directory(path: accumulated) else { throw SecureTraversalError.identityChanged }
            components.append(DirectoryComponent(name: name, expected: DirectoryIdentity(device: expected.device, inode: expected.inode)))
        }
        guard accumulated == directory.path else { throw SecureTraversalError.invalidComponent }
        func recoverDirectory(_ error: Error) throws -> Bool {
            guard directory.path != rootPath, let slash = directory.path.lastIndex(of: 47) else { throw error }
            let parent = slash == 0 ? Data([47]) : Data(directory.path.prefix(upTo: slash))
            let name = Data(directory.path.suffix(from: directory.path.index(after: slash)))
            var failure: Int32?
            var replaced = false
            if let secure = error as? SecureTraversalError {
                switch secure {
                case .filesystem(let code): failure = code
                case .identityChanged: replaced = true
                case .invalidComponent: throw error
                }
            } else if let scan = error as? ScanError, case .filesystem(_, let code) = scan {
                failure = code
            } else { throw error }
            if failure == EACCES || failure == EPERM {
                liveInventory.unavailable(directory.path, identity: DirectoryIdentity(device: directory.device, inode: directory.inode), missing: false)
                let message = ScanError.filesystem(path, failure!).localizedDescription
                try store.blockDirectory(path: directory.path, parent: parent, name: name, message: message)
                return true
            }
            if replaced || failure == ENOENT || failure == ENOTDIR || failure == ELOOP {
                liveInventory.unavailable(directory.path, identity: DirectoryIdentity(device: directory.device, inode: directory.inode), missing: true)
                try store.invalidateDirectory(path: directory.path, parent: parent, name: name)
                return true
            }
            throw error
        }
        let descriptor: Int32
        do { descriptor = try root.openDirectory(components) }
        catch { return try recoverDirectory(error) }
        defer { close(descriptor) }
        onListingStart?(directory.path, "\(directory.device):\(directory.inode)")
        defer { onListingEnd?() }
        #if ORBIT_TESTING
        testBeforeListing?()
        #endif
        var revision = try store.dirtyRevision(parent: directory.path)
        let fullGeneration = try store.fullRefreshState().generation
        // Commit the new epoch with its first batch, or with the trim marker for an empty listing.
        var epoch: Int64?
        var batches = 0
        var pending: [FileMetadata] = []
        var listingIssues: [String] = []
        var listingIssueCount = 0
        var lastCommit = ProcessInfo.processInfo.systemUptime
        var bufferingFreshListing = false
        if maxBatches == nil && revision == 0,
           directory.path != rootPath, preferredDirectory?() != directory.path {
            bufferingFreshListing = try store.canCompleteFreshListing(parent: directory.path)
        }
        var completedFreshListing = false
        func included(_ records: [FileMetadata]) -> [FileMetadata] {
            records.filter { record in
                guard record.isDirectory else { return true }
                var child = directory.path
                if child.last != 47 { child.append(47) }
                child.append(contentsOf: record.name.utf8)
                return !excludedPaths.contains(child) && !excludedDirectories.contains(DirectoryIdentity(device: record.device, inode: record.inode))
                    && !root.scope.excludes(path: child)
            }
        }
        func accountCommittedBatch(_ count: Int) throws {
            onRecordCount?(count)
            try beforeAccounting?()
            while try store.reconcileLinksChunk(cancellation: cancellation) { }
            batches += 1
            try onBatch?(directory.path, batches)
            if let maxBatches, batches >= maxBatches { throw Interrupt.afterBatch }
            lastCommit = ProcessInfo.processInfo.systemUptime
        }
        func commitPending(source: StageReuseSource? = nil, limit: Int? = nil) throws {
            guard !pending.isEmpty else { return }
            let records = limit.map { Array(pending.prefix($0)) } ?? pending
            let append = { try self.store.append(parent: directory.path, epoch: epoch, records: records, rootDevice: self.root.identity.device) }
            epoch = try source.map { try $0.commit(append) } ?? append()
            if records.count == pending.count { pending.removeAll(keepingCapacity: true) }
            else { pending.removeFirst(records.count) }
            try accountCommittedBatch(records.count)
        }
        func list(forceFallback: Bool, testFallback: (() -> Void)?) throws {
            onEnumerationStart?()
            let observedIdentity = DirectoryIdentity(device: directory.device, inode: directory.inode)
            liveInventory.begin(directory.path, identity: observedIdentity)
            try StreamingBulkDirectory.stream(descriptor, url: URL(fileURLWithPath: path, isDirectory: true),
                cancellation: cancellation, progress: { _, _ in }, issue: {
                    listingIssueCount += 1
                    if listingIssues.count < 100 { listingIssues.append($0) }
                }, forceFallback: forceFallback, testFallbackAfterFirstBuffer: testFallback) { records in
                    let accepted = included(records)
                    liveInventory.append(PendingInventoryBatch(sequence: 1, parentPath: directory.path,
                        parentIdentity: observedIdentity, components: components, records: accepted, completion: nil, failure: nil))
                    pending.append(contentsOf: accepted)
                    if bufferingFreshListing {
                        let isPreferred = preferredDirectory?() == directory.path
                        // A short batch is emitted at EOF; a full batch must be visible before the next filesystem read.
                        if records.count < 128 && pending.count < 128 && !isPreferred { return }
                        bufferingFreshListing = false
                        try commitPending(limit: 128)
                    }
                    if batches == 0 || pending.count >= writeChunkSize || ProcessInfo.processInfo.systemUptime - lastCommit >= 0.05 {
                        try commitPending()
                    }
                }
            liveInventory.append(PendingInventoryBatch(sequence: 1, parentPath: directory.path,
                parentIdentity: observedIdentity, components: components, records: [],
                completion: PendingListingCompletion(issueCount: listingIssueCount, issues: listingIssues), failure: nil))
            if bufferingFreshListing {
                try root.validate(url: rootURL)
                if try store.completeFreshListing(parent: directory.path, records: pending, rootDevice: root.identity.device,
                                                  expectedRevision: revision, expectedFullGeneration: fullGeneration,
                                                  issueCount: listingIssueCount, issues: listingIssues) {
                    completedFreshListing = true
                    bufferingFreshListing = false
                    let count = pending.count
                    pending.removeAll(keepingCapacity: true)
                    if count > 0 { try accountCommittedBatch(count) }
                    return
                }
                bufferingFreshListing = false
            }
            try commitPending()
        }
        var reused: StageReusableListing?
        do {
            if let reusableListing, fullGeneration == 0, try !store.isDirty(parent: directory.path) {
                reused = try reusableListing(directory)
            }
            if let listing = reused {
                bufferingFreshListing = false
                do {
                    while true {
                        guard !cancellation.isCancelled, !Task<Never, Never>.isCancelled else { throw CancellationError() }
                        let pageSize = batches == 0 ? 128 : writeChunkSize
                        let page = try listing.page(limit: pageSize)
                        if page.isEmpty { break }
                        pending = included(page)
                        try commitPending(source: listing.source)
                        if page.count == pageSize {
                            guard !cancellation.isCancelled, !Task<Never, Never>.isCancelled else { throw CancellationError() }
                            usleep(2_000)
                            guard !cancellation.isCancelled, !Task<Never, Never>.isCancelled else { throw CancellationError() }
                        }
                    }
                } catch is StageReuseSource.Failure {
                    pending.removeAll(keepingCapacity: true); epoch = nil; reused = nil
                }
            }
            #if ORBIT_TESTING
            let testFallback = testFallbackAfterFirstBuffer
            #else
            let testFallback: (() -> Void)? = nil
            #endif
            do { if reused == nil { try list(forceFallback: forceFallback, testFallback: testFallback) } }
            catch StreamingBulkDirectory.Failure.needsFallback {
                pending.removeAll(keepingCapacity: true); listingIssues.removeAll(keepingCapacity: true); listingIssueCount = 0
                if completedFreshListing {
                    try store.markDirty(parent: directory.path)
                    revision = try store.dirtyRevision(parent: directory.path)
                    completedFreshListing = false
                }
                epoch = nil
                lastCommit = ProcessInfo.processInfo.systemUptime
                try list(forceFallback: true, testFallback: nil)
            }
        } catch ListingStage.Reconcile.pruneRequired { return true }
        catch { return try recoverDirectory(error) }
        try root.validate(url: rootURL)
        if completedFreshListing {
            onListingComplete?()
            return true
        }
        let finish = { try self.store.finish(parent: directory.path, epoch: epoch, expectedRevision: revision,
                                            expectedFullGeneration: fullGeneration, issueCount: listingIssueCount, issues: listingIssues) }
        let finished: Bool
        do { finished = try reused.map { try $0.source.commit(finish) } ?? finish() }
        catch is StageReuseSource.Failure { return true }
        if finished { onListingComplete?() }
        return true
    }
}
