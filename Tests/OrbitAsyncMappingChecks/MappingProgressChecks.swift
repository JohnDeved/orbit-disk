import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

private final class ProgressSamples: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [MappingProgress] = []
    let signals = Signals()
    func append(_ value: ScanProgress) {
        if let mapping = value.mapping { lock.lock(); values.append(mapping); lock.unlock() }
        signals.signal()
    }
    var samples: [MappingProgress] { lock.lock(); defer { lock.unlock() }; return values }
    var latest: MappingProgress? { samples.max { $0.revision < $1.revision } }
}

enum MappingProgressChecks {
    static func run(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "mapping-progress")
        for index in 0..<256 { try fixture.file("file-\(index)") }
        var stage: ResumableStage? = try fixture.stage()
        let discovery = ProgressSamples()
        stage!.configureProgress(previousFileCount: 512)
        try stage!.startDiscovery(progress: { discovery.append($0) })
        try discovery.signals.until("memory completion before cache writes") { discovery.latest?.discoveryComplete == true }
        try require(discovery.latest?.cachedFileCount == 0 && discovery.latest?.cacheFraction == 0,
                    "Discovery claimed uncommitted files were cached")
        try require(stage!.crawler.asyncInventory!.progressSnapshot.acceptedSaveWork > 256,
                    "Save workload omitted listing completion work")
        let preview = DiskScanPreview()
        preview.attach(stage: stage!)
        try require(try preview.discoveredFileCount(cancellation: ScanCancellation()) == 256,
                    "Count-only preview lost files that are not yet cached")
        let victim = try stage!.view(path: nil, removals: [], cancellation: ScanCancellation())!.root.children[0]
        let removal = ScanRemoval(path: victim.id, identity: victim.fileIdentity!)
        try require(try preview.discoveredFileCount(removals: [removal, removal], cancellation: ScanCancellation()) == 255,
                    "Count-only preview did not apply identity-aware, deduplicated removal overlays")
        preview.detachStage()
        while try stage!.advance(progress: { discovery.append($0) }) {}
        try require(discovery.latest?.cachedFileCount == 256 && discovery.latest?.cacheFinishing == true,
                    "Writer completion lost exact durable count or publication phase")
        let revisions = discovery.samples.map(\.revision)
        try require(Set(revisions).count == revisions.count, "Concurrent progress snapshots reused a revision")
        stage = nil

        let resumed = try fixture.stage(), progress = ProgressSamples()
        try resumed.startDiscovery(progress: { progress.append($0) })
        try require(progress.samples.first?.cachedFileCount == 256,
                    "Resuming the stage reset its durable file count")
        try progress.signals.until("seed discovery") { resumed.crawler.asyncInventory?.discoveryFinished == true }
        try require(resumed.crawler.asyncInventory!.progressSnapshot.acceptedSaveWork == 0,
                    "Already cached seed listings were counted as pending writes")
        while try resumed.advance(progress: { progress.append($0) }) {}

        try fixture.file("new-file")
        try resumed.crawler.markChanged(directory: fixture.root.path)
        let previousRevision = progress.latest!.revision
        while try resumed.advance(progress: { progress.append($0) }) {}
        try require(progress.samples.contains { $0.revision > previousRevision && !$0.discoveryComplete && $0.cacheFraction == nil },
                    "New enumeration did not withdraw completed discovery and its stale save denominator")
        try require(progress.latest?.cachedFileCount == 257, "Retry upserts duplicated cached file counts")
        _ = try resumed.publish(elapsed: 1, progress: { progress.append($0) })
        try require(progress.latest?.cacheFraction == 1 && progress.latest?.cacheFinishing == false
            && progress.latest?.discoveryComplete == true, "Publication did not finish both phases")
        resumed.crawler.cancelAsyncDiscovery()
        try require(try resumed.discoveredFileCount(removals: [removal], cancellation: ScanCancellation()) == 256,
                    "Count-only durable fallback disagreed with its removal-projected root")
        let cancelled = ScanCancellation(); cancelled.cancel()
        do {
            _ = try resumed.discoveredFileCount(cancellation: cancelled)
            throw AsyncCheckFailure(description: "Cancelled count-only read returned a result")
        } catch is CancellationError {}
        try mailboxAccounting(base)
        try heterogeneousEstimate(base)
        try sharedDiscoveryBeforeImport(base)
        try pendingRetriesStayIncomplete(base)
        print("PASS: live discovery, seed resume, reconciliation and publication report independent exact cache counts")
    }

    private static func mailboxAccounting(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "progress-spool")
        let file = try fixture.file("entry")
        let root = try FileMetadata.read(fixture.root), record = try FileMetadata.read(file)
        let identity = DirectoryIdentity(device: root.device, inode: root.inode)
        let mailbox = PendingInventoryMailbox(rootPath: Data(fixture.root.path.utf8), rootIdentity: identity,
                                              cancellation: ScanCancellation(), onAccepted: {})
        let batch = PendingInventoryBatch(sequence: 1, parentPath: Data(fixture.root.path.utf8), parentIdentity: identity,
            components: [], records: [record], completion: PendingListingCompletion(issueCount: 0, issues: []), failure: nil)
        try mailbox.observe(batch)
        try require(mailbox.pendingCount == 0 && mailbox.progressSnapshot.acceptedSaveWork == 2
            && mailbox.progressSnapshot.cacheFraction == 0, "RAM spool backlog was excluded from accepted save work")
        try mailbox.enqueue(batch)
        mailbox.acknowledge(through: 1)
        mailbox.acknowledge(through: 1)
        try require(mailbox.progressSnapshot.settledSaveWork == 2, "Repeated acknowledgements inflated committed work")
        try mailbox.observe(batch, save: false, needsImport: true)
        try require(mailbox.progressSnapshot.cacheFraction == nil, "Foreign map reuse claimed a complete save denominator")
        try require(!mailbox.progressSnapshot.uncertain && mailbox.progressSnapshot.cacheUncertain,
                    "Already observed reuse records invalidated the discovery phase")
        mailbox.cancel()
        try require(mailbox.progressSnapshot.cancelled && mailbox.progressSnapshot.cacheFraction == nil,
                    "Cancelled discovery reported successful caching")
    }

    private static func heterogeneousEstimate(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "progress-heterogeneous")
        for directory in 0..<72 {
            try fixture.file("small-\(directory)/first")
            try fixture.file("small-\(directory)/second")
        }
        for index in 0..<256 { try fixture.file("large/file-\(index)") }
        let held = Data(fixture.root.appendingPathComponent("large").path.utf8)
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        AsyncListingProducer.testBeforeDirectory = { path, _ in
            if path == held { entered.signal(); _ = release.wait(timeout: .now() + 20) }
        }
        let stage = try fixture.stage(), samples = ProgressSamples()
        defer { release.signal(); stage.crawler.cancelAsyncDiscovery(); AsyncListingProducer.testBeforeDirectory = nil }
        try stage.startDiscovery(progress: { samples.append($0) })
        try require(entered.wait(timeout: .now() + 10) == .success, "Heterogeneous fixture did not hold its unexplored large folder")
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while stage.crawler.asyncInventory!.progressSnapshot.completedDirectories < 73,
              ProcessInfo.processInfo.systemUptime < deadline { Thread.sleep(forTimeInterval: 0.001) }
        let work = stage.crawler.asyncInventory!.progressSnapshot
        try require(work.files == 144 && work.completedDirectories == 73 && work.activeDirectories == 1,
                    "Heterogeneous fixture did not finish the small folders independently")
        Thread.sleep(forTimeInterval: 0.11)
        _ = try stage.advance(progress: { samples.append($0) })
        try require(samples.latest?.estimateBasis == .currentScan && samples.latest!.discoveryFraction! > 0
            && samples.latest!.discoveryFraction! < 1 && !samples.latest!.discoveryComplete,
                    "Actual discovery counters did not produce an explicitly incomplete cold estimate")
        var warm = DiscoveryEstimator(previousFileCount: 400)
        let historical = warm.estimate(work, complete: false, now: 0)
        try require(historical.fraction == 0.36 && historical.basis == .previousMap,
                    "A previous complete count was not used for the same heterogeneous observations")
        release.signal()
        try samples.signals.until("memory completion after writer ingestion started") {
            samples.latest?.discoveryComplete == true
        }
        try require(samples.latest?.discoveryFraction == 1 && samples.latest!.cachedFileCount! < 400
            && samples.latest!.cacheFraction! < 1,
                    "Waiting for journal settlement incorrectly made memory completion wait for cache catch-up")
        while try stage.advance(progress: { samples.append($0) }) {}
        try require(samples.latest?.discoveryFraction == 1 && samples.latest?.cachedFileCount == 400,
                    "Heterogeneous discovery did not replace its estimate with final actual counts")
    }

    private static func sharedDiscoveryBeforeImport(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "progress-shared-import")
        let child = try fixture.directory("child")
        for index in 0..<256 { try fixture.file("child/file-\(index)") }
        let saved: IndexSnapshot
        do {
            let source = try ResumableStage(cache: fixture.cache, root: child,
                current: StageCheckpoint(rootPath: child.path, identity: "async-check", eventID: 0))
            try finish(source)
            saved = try source.publish(elapsed: 0)
        }
        let parent = try fixture.stage(), samples = ProgressSamples()
        parent.configureReuse { directory, checkpoint in
            directory.path == child.path ? StageReuseCandidate(snapshot: saved, checkpoint: checkpoint) : nil
        }
        defer { parent.crawler.cancelAsyncDiscovery() }
        try parent.startDiscovery(progress: { samples.append($0) })
        try samples.signals.until("shared memory completion before import") { samples.latest?.discoveryComplete == true }
        try require(parent.crawler.asyncInventory!.progressSnapshot.cacheUncertain,
                    "Shared progress fixture did not use imported records")
        try require(samples.latest?.discoveryFraction == 1 && samples.latest?.cachedFileCount == 0
            && samples.latest?.cacheFraction == nil,
                    "Completed shared discovery waited for cache import or claimed an unknown save denominator")
        try require(try parent.discoveredFileCount(cancellation: ScanCancellation()) == 256,
                    "Shared completion was reported before the records became readable in memory")
        let completedRevision = samples.latest!.revision
        while try parent.advance(progress: { samples.append($0) }) {}
        try require(samples.samples.filter { $0.revision > completedRevision }.allSatisfy(\.discoveryComplete),
                    "Importing already discovered records was reported as new filesystem enumeration")
        try require(parent.testReusedDirectories == 1, "Shared progress fixture did not import its saved listing")
        try require(samples.latest?.discoveryComplete == true && samples.latest?.cachedFileCount == 256,
                    "Shared discovery or committed counts regressed during import")
    }

    private static func pendingRetriesStayIncomplete(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "progress-pending-retry")
        let child = try fixture.directory("child")
        for index in 0..<256 { try fixture.file("child/file-\(index)") }
        let injection = DuplicateListingInjection(target: child, crossBatch: false)
        AsyncListingProducer.testRecords = injection.records
        let stage = try fixture.stage(), samples = ProgressSamples()
        defer { stage.crawler.cancelAsyncDiscovery(); AsyncListingProducer.testRecords = nil }
        try stage.startDiscovery(progress: { samples.append($0) })
        try samples.signals.until("discovery pass with deferred retry") {
            stage.crawler.asyncInventory?.discoveryFinished == true
        }
        try injection.verify()
        try require(stage.crawler.asyncInventory!.retryDeferredDirectoryCount == 1,
                    "Retry progress fixture did not defer its failed listing")
        try require(!samples.samples.contains { $0.discoveryComplete },
                    "A finished parallel pass claimed completion with undiscovered retry files")
        while try stage.advance(progress: { samples.append($0) }) {}
        try require(samples.latest?.discoveryComplete == true && samples.latest?.cachedFileCount == 256,
                    "Reconciled discovery did not finish after the deferred listing was read")
    }
}
