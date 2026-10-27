import CryptoKit
import Darwin
import Foundation

// The app replays filesystem events after the staged snapshot is published.
public final class ResumableStage: @unchecked Sendable {
    public enum Failure: LocalizedError {
        case checkpointIdentityChanged, checkpointRootChanged, alreadyPublished(String), inUse, invalidated
        public var errorDescription: String? {
            switch self {
            case .inUse: return "This location is being mapped in another Orbit Disk window."
            case .invalidated: return "This map was invalidated while mapping."
            case .checkpointIdentityChanged, .checkpointRootChanged: return "This location changed since mapping was paused."
            case .alreadyPublished: return "This map has already been saved."
            }
        }
    }
    let lease: StageLease
    public let checkpoint: StageCheckpoint
    public let hasPriorCheckpoint: Bool
    let expectedSnapshot: SnapshotExpectation
    private let expectedGeneration: String
    private let stagingDirectory: URL
    let crawler: DurableCrawler
    private let previewSnapshot: IndexSnapshot
    private let previewStore: DiskIndexStore
    private let removalReplay: StageRemovalReplay
    private var removalReplayPrepared = false
    private let revisionLock = NSLock()
    private var revisionValue: UInt64 = 0
    private var lastProgressTime: TimeInterval = 0
    private var progressItems = 0
    #if ORBIT_TESTING
    package var testPreviewSnapshot: IndexSnapshot { previewSnapshot }
    package static var testBeforeStagedRead: (@Sendable () -> Void)?
    package func testUnprojectedNode(path: String) throws -> DiskNode? {
        try previewStore.node(path: path, snapshot: previewSnapshot)
    }
    private var testScanner: DiskScanner?
    private var testBatchDisabled = false
    public func configureTestScanner(_ scanner: DiskScanner) {
        testScanner = scanner
        crawler.forceFallback = !scanner.useBulkEnumeration
    }
    public func configureTestFallbackAfterFirstBuffer(_ callback: @escaping () -> Void) {
        crawler.testFallbackAfterFirstBuffer = { [weak crawler] in
            crawler?.testFallbackAfterFirstBuffer = nil
            callback()
        }
    }
    #endif

    public static func hasCandidate(cache: URL, stagingDirectory: URL? = nil, root: URL) -> Bool {
        guard let digest = try? StageLease.key(root: root) else { return false }
        let path = (stagingDirectory ?? IndexLocations.locks(for: cache)).appendingPathComponent(".orbit-stages", isDirectory: true).appendingPathComponent(digest + ".sqlite").path
        var info = stat()
        return lstat(path, &info) == 0
    }

    public init(cache: URL, stagingDirectory: URL? = nil, root: URL, current: StageCheckpoint, cancellation: ScanCancellation = ScanCancellation()) throws {
        let root = root.standardizedFileURL
        let cache = IndexLocations.canonicalCache(cache)
        let lockDirectory = IndexLocations.locks(for: cache)
        let stagingDirectory = (stagingDirectory ?? lockDirectory).standardizedFileURL
        try FileManager.default.createDirectory(at: cache, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try IndexLocations.validateCache(cache)
        try FileManager.default.createDirectory(at: lockDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try FileManager.default.createDirectory(at: stagingDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        guard Data(current.rootPath.utf8) == Data(root.path.utf8) else { throw Failure.checkpointRootChanged }
        let captured: (SnapshotExpectation, String) = try {
            let mutationLease = try RootMutationLease(directory: lockDirectory, root: root, cancellation: cancellation)
            defer { withExtendedLifetime(mutationLease) {} }
            return (try SnapshotExpectation.capture(directory: cache, root: root), try mutationLease.invalidationGeneration())
        }()
        let (expectedSnapshot, generation) = captured
        let lease: StageLease
        let leaseDeadline = ProcessInfo.processInfo.systemUptime + 2
        while true {
            if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
            do { lease = try StageLease(cache: stagingDirectory, root: root); break }
            catch StageLease.Failure.busy where ProcessInfo.processInfo.systemUptime < leaseDeadline { usleep(10_000) }
            catch StageLease.Failure.busy { throw Failure.inUse }
        }
        try lease.validatePaths()
        let prior: (checkpoint: (identity: String, eventID: UInt64)?, expected: SnapshotExpectation?, completed: (id: String, published: Bool)?, generation: String?) = try {
            let store = try ListingStage(path: lease.databasePath, mirror: true)
            return (try store.scanStart(root: Data(root.path.utf8)), try store.scanExpectation(root: Data(root.path.utf8)),
                    try store.completedSnapshot(root: Data(root.path.utf8)), try store.scanGeneration(root: Data(root.path.utf8)))
        }()
        let previous = prior.checkpoint
        try lease.validatePaths()
        let mutationLease = try RootMutationLease(directory: lockDirectory, root: root, cancellation: cancellation)
        defer { withExtendedLifetime(mutationLease) {} }
        try expectedSnapshot.verify(directory: cache, root: root)
        guard try mutationLease.invalidationGeneration() == generation else { throw SnapshotExpectation.Failure.changed }
        if previous != nil {
            guard let previousGeneration = prior.generation else { throw SnapshotExpectation.Failure.changed }
            if previousGeneration != generation { throw Failure.invalidated }
        }
        if let completed = prior.completed, completed.published, expectedSnapshot != .absent { throw Failure.alreadyPublished(completed.id) }
        if let completed = prior.completed {
            if try expectedSnapshot == .matching(completed.id) ||
                SnapshotExpectation.descendsFromStage(completed.id, directory: cache, root: root) {
                throw Failure.alreadyPublished(completed.id)
            }
        }
        let checkpoint: StageCheckpoint
        if let previous {
            checkpoint = StageCheckpoint(rootPath: root.path, identity: previous.identity, eventID: previous.eventID)
        } else {
            checkpoint = current
        }
        if previous != nil && prior.expected == nil { throw ListingStage.Failure.sqlite("missing original snapshot expectation") }
        var originalExpectation = prior.expected ?? expectedSnapshot
        if originalExpectation != expectedSnapshot, expectedSnapshot == .absent, previous != nil {
            let store = try ListingStage(path: lease.databasePath, mirror: true)
            try store.rebaseExpectation(root: Data(root.path.utf8), from: originalExpectation, to: .absent)
            originalExpectation = .absent
        }
        guard originalExpectation == expectedSnapshot else { throw SnapshotExpectation.Failure.changed }
        let crawler = try DurableCrawler(rootURL: root, database: lease.databasePath, checkpoint: checkpoint, cancellation: cancellation, mirror: true, expectedSnapshot: originalExpectation, generation: generation,
                                         excluding: [cache, lockDirectory, stagingDirectory,
                                                     stagingDirectory.appendingPathComponent(".orbit-stages"),
                                                     lockDirectory.appendingPathComponent(".orbit-locks")])
        crawler.lifetimeLease = lease
        try lease.validatePaths()
        let removalReplay = try StageRemovalReplay(cache: cache, crawler: crawler, cancellation: cancellation, fresh: previous == nil)
        let preview = try crawler.store.previewMirror(rootURL: root, checkpoint: checkpoint)
        self.lease = lease; self.checkpoint = checkpoint; self.hasPriorCheckpoint = previous != nil; self.expectedSnapshot = originalExpectation; self.expectedGeneration = generation; self.crawler = crawler; self.stagingDirectory = stagingDirectory
        self.previewSnapshot = preview; self.previewStore = DiskIndexStore(directory: cache); self.removalReplay = removalReplay
    }

    public static func retirePublished(cache: URL, stagingDirectory: URL? = nil, root: URL, id: String, cancellation: ScanCancellation = ScanCancellation()) throws {
        let cache = IndexLocations.canonicalCache(cache)
        let lockDirectory = IndexLocations.locks(for: cache)
        let stagingDirectory = (stagingDirectory ?? lockDirectory).standardizedFileURL
        let lease = try StageLease(cache: stagingDirectory, root: root)
        let mutationLease = try RootMutationLease(directory: lockDirectory, root: root, cancellation: cancellation)
        defer { withExtendedLifetime(mutationLease) {} }
        let current = try SnapshotExpectation.capture(directory: cache, root: root)
        let completed: (id: String, published: Bool)? = try {
            let store = try ListingStage(path: lease.databasePath, mirror: true)
            return try store.completedSnapshot(root: Data(root.standardizedFileURL.path.utf8))
        }()
        guard let completed, completed.id == id else { throw SnapshotExpectation.Failure.changed }
        guard try completed.published || current == .matching(id) ||
                SnapshotExpectation.descendsFromStage(id, directory: cache, root: root) else { throw SnapshotExpectation.Failure.changed }
        try mutationLease.validate()
        try lease.removeStageFiles()
    }

    public static func retireInvalidated(cache: URL, stagingDirectory: URL? = nil, root: URL, cancellation: ScanCancellation = ScanCancellation()) throws {
        let cache = IndexLocations.canonicalCache(cache)
        let lockDirectory = IndexLocations.locks(for: cache)
        let stagingDirectory = (stagingDirectory ?? lockDirectory).standardizedFileURL
        let lease = try StageLease(cache: stagingDirectory, root: root)
        let mutationLease = try RootMutationLease(directory: lockDirectory, root: root, cancellation: cancellation)
        defer { withExtendedLifetime(mutationLease) {} }
        let prior: String? = try {
            let store = try ListingStage(path: lease.databasePath, mirror: true)
            return try store.scanGeneration(root: Data(root.standardizedFileURL.path.utf8))
        }()
        guard let prior, prior != (try mutationLease.invalidationGeneration()) else { throw SnapshotExpectation.Failure.changed }
        try mutationLease.validate()
        try lease.removeStageFiles()
    }

    public var revision: UInt64 { revisionLock.lock(); defer { revisionLock.unlock() }; return revisionValue }
    private func bumpRevision() { revisionLock.lock(); revisionValue &+= 1; revisionLock.unlock() }

    public func removalReplayReadiness() -> (cursor: RemovalCursor?, blocked: Bool, generation: UInt64) {
        let state = removalReplay.viewState()
        return (state.cursor, state.blocked, state.generation)
    }

    public func prepareRemovalReplay() throws {
        try removalReplay.prepare()
        removalReplayPrepared = true
        bumpRevision()
    }

    public func view(path: String?, removals: [ScanRemoval], cancellation: ScanCancellation) throws -> ScanPreviewView? {
        let state = removalReplay.viewState()
        guard !state.blocked else { return nil }
        #if ORBIT_TESTING
        Self.testBeforeStagedRead?()
        #endif
        let result = try previewStore.stagedView(path: path, snapshot: previewSnapshot, revision: revision,
                                                  removals: removals + state.overlay, cancellation: cancellation)
        let after = removalReplay.viewState()
        guard !after.blocked, after.generation == state.generation else { throw CancellationError() }
        return result
    }

    public func confirmsMissingDirectory(_ path: String) -> Bool {
        let state = removalReplay.viewState()
        guard !state.blocked else { return false }
        let result = (try? previewStore.stagedConfirmsMissingDirectory(path, snapshot: previewSnapshot)) ?? false
        let after = removalReplay.viewState()
        return !after.blocked && after.generation == state.generation && result
    }

    public func search(query: String, cancellation: ScanCancellation, removals: [ScanRemoval], limit: Int) throws -> (nodes: [DiskNode], count: Int, revision: UInt64, removalsChanged: Bool) {
        let state = removalReplay.viewState()
        guard !state.blocked else { throw StageRemovalReplay.Retry.cannotExpose }
        #if ORBIT_TESTING
        Self.testBeforeStagedRead?()
        #endif
        let result = try previewStore.stagedSearch(query: query, snapshot: previewSnapshot, revision: revision,
                                                   removals: removals + state.overlay, cancellation: cancellation, limit: limit)
        let after = removalReplay.viewState()
        guard !after.blocked, after.generation == state.generation else { throw CancellationError() }
        return result
    }

    public func advance(progress: @escaping @Sendable (ScanProgress) -> Void) throws -> Bool {
        if !removalReplayPrepared { try prepareRemovalReplay() }
        try crawler.store.prepareQueueIndexes(cancellation: crawler.cancellation)
        let replayGeneration = removalReplay.viewState().generation
        try removalReplay.refreshIfNeeded()
        let advanced = try crawler.step(onRecordCount: { [self] count in progressItems += count }, onBatch: { [self] path, _ in
            bumpRevision()
            #if ORBIT_TESTING
            if !testBatchDisabled, progressItems >= 512, let batch = testScanner?.testBatch {
                do { try batch() }
                catch is CancellationError { throw CancellationError() }
                catch {
                    testBatchDisabled = true
                    try testScanner?.testFallback?()
                    throw StreamingBulkDirectory.Failure.needsFallback
                }
            }
            #endif
            let now = ProcessInfo.processInfo.systemUptime
            guard lastProgressTime == 0 || now - lastProgressTime >= 0.1 else { return }
            lastProgressTime = now
            let totals = try crawler.store.folderTotals(path: Data(crawler.rootURL.path.utf8))
            progress(ScanProgress(itemCount: max(progressItems, Int(totals?.count ?? 0)), bytesScanned: totals?.logical ?? 0,
                                  currentPath: String(decoding: path, as: UTF8.self)))
            try removalReplay.refreshIfNeeded()
        }, onListingComplete: {
            #if ORBIT_TESTING
            self.testScanner?.testAfterListing?()
            #endif
        })
        try removalReplay.refreshIfNeeded()
        let stable = try removalReplay.settleIfPossible()
        if !advanced && !stable { usleep(50_000) }
        if advanced || removalReplay.viewState().generation != replayGeneration { bumpRevision() }
        return advanced || !stable
    }

    public func publish(elapsed: TimeInterval) throws -> IndexSnapshot {
        while true {
            try lease.validatePaths()
            do {
                return try MirrorPublisher.publish(crawler, checkpoint: checkpoint, directory: previewStore.directory,
                    elapsed: elapsed, expected: expectedSnapshot, expectedGeneration: expectedGeneration,
                    publicationFence: { try self.removalReplay.fencePublication($0) })
            } catch StageRemovalReplay.Retry.journalChanged {
                try prepareRemovalReplay()
                while try advance(progress: { _ in }) { }
            }
        }
    }
}
