import Darwin
import Foundation

// The directory queue and each listing batch are committed independently.
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
    var forceFallback = false
    var reusableListing: ((ListingStage.QueuedDirectory) throws -> StageReusableListing?)?
    var preferredDirectory: (() -> Data?)?
    #if ORBIT_TESTING
    var testFallbackAfterFirstBuffer: (() -> Void)?
    var testBeforeListing: (() -> Void)?
    #endif
    enum Interrupt: Error { case afterBatch }

    init(rootURL: URL, database: String, checkpoint: StageCheckpoint,
         cancellation: ScanCancellation = ScanCancellation(), writeChunkSize: Int = 512, mirror: Bool = false,
         expectedSnapshot: SnapshotExpectation? = nil, generation: String? = nil, excluding: [URL] = []) throws {
        self.rootURL = rootURL.standardizedFileURL
        self.rootPath = Data(self.rootURL.path.utf8)
        self.root = try SecureRoot(url: self.rootURL)
        self.store = try ListingStage(path: database, mirror: mirror)
        self.cancellation = cancellation
        self.writeChunkSize = max(128, writeChunkSize)
        self.excludedDirectories = Set(excluding.compactMap { url in
            var info = stat()
            guard lstat(url.path, &info) == 0, (info.st_mode & S_IFMT) == S_IFDIR else { return nil }
            return DirectoryIdentity(device: info.st_dev, inode: info.st_ino)
        })
        self.excludedPaths = IndexLocations.exclusionPaths(excluding)
        try root.validate(url: self.rootURL)
        try store.preserveScanStart(root: rootPath, identity: checkpoint.identity, eventID: checkpoint.eventID, expected: expectedSnapshot, generation: generation)
        try store.registerDirectory(path: rootPath, device: root.identity.device, inode: root.identity.inode)
        try store.initializeMirrorRoot(url: self.rootURL, identity: root.identity)
    }

    deinit { store.close() }

    func markChanged(directory: String) throws {
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
              onListingStart: ((Data, String) -> Void)? = nil, onListingEnd: (() -> Void)? = nil) throws -> Bool {
        guard !cancellation.isCancelled else { throw CancellationError() }
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
                let message = ScanError.filesystem(path, failure!).localizedDescription
                try store.blockDirectory(path: directory.path, parent: parent, name: name, message: message)
                return true
            }
            if replaced || failure == ENOENT || failure == ENOTDIR || failure == ELOOP {
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
        let revision = try store.dirtyRevision(parent: directory.path)
        let fullGeneration = try store.fullRefreshState().generation
        // Commit the new epoch with its first batch, or with the trim marker for an empty listing.
        var epoch: Int64?
        var batches = 0
        var pending: [FileMetadata] = []
        var listingIssues: [String] = []
        var listingIssueCount = 0
        func included(_ records: [FileMetadata]) -> [FileMetadata] {
            records.filter { record in
                guard record.isDirectory else { return true }
                var child = directory.path
                if child.last != 47 { child.append(47) }
                child.append(contentsOf: record.name.utf8)
                return !excludedPaths.contains(child) && !excludedDirectories.contains(DirectoryIdentity(device: record.device, inode: record.inode))
            }
        }
        func commitPending(source: StageReuseSource? = nil) throws {
            guard !pending.isEmpty else { return }
            let append = { try self.store.append(parent: directory.path, epoch: epoch, records: pending, rootDevice: self.root.identity.device) }
            epoch = try source.map { try $0.commit(append) } ?? append()
            onRecordCount?(pending.count)
            pending.removeAll(keepingCapacity: true)
            try beforeAccounting?()
            while try store.reconcileLinksChunk(cancellation: cancellation) { }
            batches += 1
            try onBatch?(directory.path, batches)
            if let maxBatches, batches >= maxBatches { throw Interrupt.afterBatch }
        }
        func list(forceFallback: Bool, testFallback: (() -> Void)?) throws {
            try StreamingBulkDirectory.stream(descriptor, url: URL(fileURLWithPath: path, isDirectory: true),
                cancellation: cancellation, progress: { _, _ in }, issue: {
                    listingIssueCount += 1
                    if listingIssues.count < 100 { listingIssues.append($0) }
                }, forceFallback: forceFallback, testFallbackAfterFirstBuffer: testFallback) { records in
                    pending.append(contentsOf: included(records))
                    if batches == 0 || pending.count >= writeChunkSize { try commitPending() }
                }
            try commitPending()
        }
        var reused: StageReusableListing?
        do {
            if let reusableListing, fullGeneration == 0, try !store.isDirty(parent: directory.path) {
                reused = try reusableListing(directory)
            }
            if let listing = reused {
                do {
                    while true {
                        guard !cancellation.isCancelled, !Task<Never, Never>.isCancelled else { throw CancellationError() }
                        let page = try listing.page(limit: writeChunkSize)
                        if page.isEmpty { break }
                        pending = included(page)
                        try commitPending(source: listing.source)
                        if page.count == writeChunkSize {
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
                epoch = nil
                try list(forceFallback: true, testFallback: nil)
            }
        } catch ListingStage.Reconcile.pruneRequired { return true }
        catch { return try recoverDirectory(error) }
        try root.validate(url: rootURL)
        let finish = { try self.store.finish(parent: directory.path, epoch: epoch, expectedRevision: revision,
                                            expectedFullGeneration: fullGeneration, issueCount: listingIssueCount, issues: listingIssues) }
        let finished: Bool
        do { finished = try reused.map { try $0.source.commit(finish) } ?? finish() }
        catch is StageReuseSource.Failure { return true }
        if finished { onListingComplete?() }
        return true
    }
}
