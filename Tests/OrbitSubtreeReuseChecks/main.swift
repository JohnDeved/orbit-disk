@testable import OrbitCore
import Darwin
import CoreServices
import Foundation

private struct Failure: Error { let message: String }
private func require(_ value: Bool, _ message: String) throws { if !value { throw Failure(message: message) } }
private func identity(_ url: URL) throws -> String {
    var info = stat()
    guard lstat(url.path, &info) == 0 else { throw Failure(message: "missing fixture") }
    return "\(info.st_dev):\(info.st_ino)"
}
private func finish(_ stage: ResumableStage) throws { while try stage.advance(progress: { _ in }) {} }
private func stage(_ root: URL, _ cache: URL, event: UInt64 = 20, reuse: Bool = true, cancellation: ScanCancellation = ScanCancellation()) throws -> ResumableStage {
    let value = try ResumableStage(cache: cache, root: root, current: StageCheckpoint(rootPath: root.path, identity: "events", eventID: event), cancellation: cancellation)
    if reuse {
        value.configureReuse { directory, checkpoint in
            guard let snapshot = DiskIndexStore(directory: cache).savedSnapshot(for: directory) else { return nil }
            return StageReuseCandidate(snapshot: snapshot, checkpoint: StageCheckpoint(rootPath: checkpoint.rootPath,
                identity: checkpoint.identity, eventID: min(checkpoint.eventID, snapshot.eventID)))
        }
    }
    return value
}
private func save(_ root: URL, _ cache: URL) throws -> IndexSnapshot {
    let value = try stage(root, cache, event: 5, reuse: false)
    try finish(value); return try value.publish(elapsed: 0)
}
private func verify(_ root: URL, _ cache: URL, _ snapshot: IndexSnapshot) throws {
    let expected = try DiskScanner().scan(root).root, store = DiskIndexStore(directory: cache)
    var queue = [expected]
    while let node = queue.popLast() {
        let actual = try store.node(path: node.id, snapshot: snapshot)
        try require(actual?.size == node.size && actual?.allocatedSize == node.allocatedSize
            && actual?.fileCount == node.fileCount, "reused totals differ at \(node.name)")
        queue.append(contentsOf: node.children)
    }
}
private final class Once: @unchecked Sendable {
    private let lock = NSLock(); private var taken = false
    func take() -> Bool { lock.lock(); defer { lock.unlock() }; if taken { return false }; taken = true; return true }
}
private struct PauseReuse: Error {}

private func checkBootTraversalUpgrade(_ cache: URL, current: StageCheckpoint) throws {
    var legacyObject = try JSONSerialization.jsonObject(with: Data(current.identity.utf8)) as! [String: Any]
    try require(legacyObject.removeValue(forKey: "bootTraversal") as? Int == 2, "root checkpoint lacks traversal scope")
    let legacyIdentity = String(decoding: try JSONSerialization.data(withJSONObject: legacyObject, options: [.sortedKeys]), as: UTF8.self)
    let legacy = StageCheckpoint(rootPath: "/", identity: legacyIdentity, eventID: current.eventID)
    try require(current.upgradesBootTraversal(from: legacy), "legacy root scope was not recognized")
    try require(!current.upgradesBootTraversal(from: current), "current root scope re-upgraded")
    legacyObject["rootFileID"] = 0
    let replaced = StageCheckpoint(rootPath: "/", identity: String(decoding: try JSONSerialization.data(withJSONObject: legacyObject), as: UTF8.self), eventID: current.eventID)
    try require(!current.upgradesBootTraversal(from: replaced), "replacement root was treated as a scope upgrade")

    let root = URL(fileURLWithPath: "/"), recordName = "orbit-boot-upgrade-fixture-\(UUID().uuidString)"
    var info = stat(); try require(lstat("/", &info) == 0, "missing boot root")
    do {
        let previous = try ResumableStage(cache: cache, root: root, current: legacy)
        let record = FileMetadata(name: recordName, device: info.st_dev, inode: 123, isDirectory: false,
            isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 19, allocatedSize: 512)
        try require(try previous.crawler.store.completeFreshListing(parent: Data("/".utf8), records: [record],
            rootDevice: info.st_dev, expectedRevision: 0, expectedFullGeneration: 0), "could not seed legacy root listing")
    }
    for _ in 0..<2 {
        let resumed = try ResumableStage(cache: cache, root: root, current: current)
        let state = try resumed.crawler.store.fullRefreshState()
        try require(resumed.hasPriorCheckpoint && resumed.checkpoint == current && state.pending && state.generation == 1,
                    "root stage did not atomically adopt one full-refresh generation")
        try require(try resumed.testUnprojectedNode(path: "/" + recordName)?.size == 19,
                    "scope upgrade discarded existing preview rows")
        let reader = try DiscoveryCacheReader(path: resumed.crawler.store.databasePath,
            rootIdentity: DirectoryIdentity(device: info.st_dev, inode: info.st_ino), cancellation: ScanCancellation())
        let listing = try reader.listing(ListingStage.QueuedDirectory(path: Data("/".utf8), device: info.st_dev, inode: info.st_ino))
        try require(listing != nil && listing?.records == nil && listing?.generation == 1,
                    "old complete root listing escaped immediate reconciliation")
    }
    print("PASS: legacy root scope upgrades once, retains preview rows and immediately rejects stale discovery listings")
}

private func checkNameFoldFallback(_ base: URL) throws {
    let files = FileManager.default, rawName = "が-file"
    for interrupted in [false, true] {
        let group = base.appendingPathComponent(interrupted ? "resumed" : "complete")
        let root = group.appendingPathComponent("root"), child = root.appendingPathComponent("child")
        let cache = group.appendingPathComponent("cache"), store = DiskIndexStore(directory: cache)
        try files.createDirectory(at: child, withIntermediateDirectories: true)
        for number in 0..<130 { try Data(repeating: 7, count: 25).write(to: child.appendingPathComponent(String(format: "%04d", number))) }
        let descriptor = open(child.path + "/" + rawName, O_CREAT | O_EXCL | O_WRONLY, 0o600)
        guard descriptor >= 0 else { throw Failure(message: "could not create raw kana filename") }
        let resized = ftruncate(descriptor, 9); close(descriptor)
        try require(resized == 0, "could not size raw kana file")
        let scanned = try DiskScanner().scan(child)
        try require(scanned.root.children.contains { $0.name.utf8.elementsEqual(rawName.utf8) }, "fixture lost raw kana spelling")
        let initial = try save(child, cache)
        let saved = try store.update(snapshot: initial, replacements: [IndexReplacement(path: child.path, result: scanned)],
                                     eventID: 6, eventIdentity: "events")
        let queries = [rawName, "か\u{3099}-file", "file"]
        let expectedSearch = try queries.map { try store.search(query: $0, snapshot: saved) }
        try require(expectedSearch[0].totalCount == 1, "saved kana search fixture did not match")
        do {
            let source = try StageReuseSource(StageReuseCandidate(snapshot: saved,
                checkpoint: StageCheckpoint(rootPath: root.path, identity: "events", eventID: 6)),
                cache: cache, parentRoot: root, parentIdentity: identity(root), cancellation: ScanCancellation())
            var info = stat(); try require(lstat(child.path, &info) == 0, "missing kana source")
            let listing = try source.listing(ListingStage.QueuedDirectory(path: Data(child.path.utf8),
                device: info.st_dev, inode: info.st_ino))!
            try require(try listing.page(limit: 128).count == 128, "fixture needs a reusable committed prefix")
            do { _ = try listing.page(limit: 128); throw Failure(message: "refolding a normalized kana name was accepted") }
            catch StageReuseSource.Failure.unsuitable {}
        }
        if interrupted {
            do {
                let parent = try stage(root, cache)
                try parent.prepareRemovalReplay()
                do {
                    while try parent.crawler.step(onBatch: { path, count in
                        if path == Data(child.path.utf8) && count == 1 { throw PauseReuse() }
                    }) {}
                    throw Failure(message: "reuse interruption was not reached")
                } catch is PauseReuse {}
                let totals = try parent.crawler.store.folderTotals(path: Data(child.path.utf8))
                try require(totals?.count == 128 && parent.checkpoint.eventID == 6, "interrupted reuse lost its committed prefix")
            }
        }
        let parent = try stage(root, cache, event: interrupted ? 30 : 20)
        if interrupted {
            let totals = try parent.crawler.store.folderTotals(path: Data(child.path.utf8))
            try require(totals?.count == 128 && parent.checkpoint.eventID == 6, "reopen lost reuse progress or event floor")
        }
        try finish(parent)
        let result = try parent.publish(elapsed: 0), expected = try DiskScanner().scan(root).root
        let actual = try store.loadFolder(path: root.path, snapshot: result)
        try require(actual.size == expected.size && actual.allocatedSize == expected.allocatedSize && actual.fileCount == 131,
                    "native fallback duplicated or lost reused rows")
        let actualFiles = try store.loadFolder(path: child.path, snapshot: result).children
        try require(actualFiles.count == 131 && Set(actualFiles.compactMap(\.fileIdentity)) == Set(scanned.root.children.compactMap(\.fileIdentity)),
                    "native fallback changed file identities")
        for (index, query) in queries.enumerated() {
            let found = try store.search(query: query, snapshot: result), reference = expectedSearch[index]
            try require(found.totalCount == reference.totalCount && found.nodes.map(\.fileIdentity) == reference.nodes.map(\.fileIdentity),
                        "saved-to-reused kana search changed matching semantics")
        }
    }
    print("PASS: normalized kana reuse falls back to native names; copied prefix survives reopen without changing search or accounting")
}

@main struct SubtreeReuseChecks {
    static func main() throws {
        setbuf(stdout, nil)
        if CommandLine.arguments.count == 4, CommandLine.arguments[1] == "--kill-after-reuse" {
            let root = URL(fileURLWithPath: CommandLine.arguments[2]), cache = URL(fileURLWithPath: CommandLine.arguments[3])
            let value = try stage(root, cache)
            value.configureTestScanner(DiskScanner(testBatch: { kill(getpid(), SIGKILL) }))
            try finish(value)
            throw Failure(message: "crash boundary not reached")
        }
        let files = FileManager.default
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).resolvingSymlinksInPath()
        func directory(_ path: String) throws -> URL {
            let result = base.appendingPathComponent(path, isDirectory: true)
            try files.createDirectory(at: result, withIntermediateDirectories: true); return result
        }
        func write(_ url: URL, _ count: Int = 25) throws { try Data(repeating: 7, count: count).write(to: url) }

        for importedFirst in [true, false] {
            let group = importedFirst ? "first" : "last"
            let child = try directory("\(group)/root/\(importedFirst ? "a" : "z")"), root = child.deletingLastPathComponent()
            let fresh = try directory("\(group)/root/\(importedFirst ? "z" : "a")")
            let cache = base.appendingPathComponent("\(group)/cache")
            let first = child.appendingPathComponent("shared")
            try write(first, 4096); try files.linkItem(at: first, to: fresh.appendingPathComponent("shared"))
            let nested = child.appendingPathComponent("nested", isDirectory: true)
            try files.createDirectory(at: nested, withIntermediateDirectories: true)
            try write(nested.appendingPathComponent("other"), 700)
            _ = try save(child, cache)
            let parent = try stage(root, cache)
            try finish(parent)
            try require(parent.testReusedDirectories == 1, "completed subtree root was not reused")
            try require(parent.checkpoint.eventID == 5, "parent forgot source event floor")
            let saved = try parent.publish(elapsed: 0)
            try require(saved.eventID == 5, "publication advanced past reused history")
            try verify(root, cache, saved)
        }
        print("PASS: completed descendant reuse, event floor and cross-source hardlink owners in both orders")

        let mixedChild = try directory("mixed/root/child"), mixedRoot = mixedChild.deletingLastPathComponent()
        let mixedCache = base.appendingPathComponent("mixed/cache")
        let sparse = try directory("mixed/root/child/sparse"), dense = try directory("mixed/root/child/branch/dense")
        for number in 0..<31 { try write(sparse.appendingPathComponent(String(number))) }
        for number in 0..<32 { try write(dense.appendingPathComponent(String(number))) }
        _ = try save(mixedChild, mixedCache)
        let mixed = try stage(mixedRoot, mixedCache)
        try finish(mixed)
        try require(mixed.testReusedDirectories == 2 && mixed.checkpoint.eventID == 5,
                    "tiny native fallback discarded the source or skipped a dense descendant")
        try verify(mixedRoot, mixedCache, mixed.publish(elapsed: 0))
        print("PASS: tiny listings use native traversal while retained source reuses larger descendants")

        let pagedChild = try directory("pages/root/child"), pagedRoot = pagedChild.deletingLastPathComponent()
        let pagedCache = base.appendingPathComponent("pages/cache")
        for number in 0..<129 { try write(pagedChild.appendingPathComponent(String(format: "%04d", number))) }
        let pagedSaved = try save(pagedChild, pagedCache)
        let pagedSource = try StageReuseSource(StageReuseCandidate(snapshot: pagedSaved,
            checkpoint: StageCheckpoint(rootPath: pagedRoot.path, identity: "events", eventID: 5)),
            cache: pagedCache, parentRoot: pagedRoot, parentIdentity: identity(pagedRoot), cancellation: ScanCancellation())
        var pagedInfo = stat(); _ = lstat(pagedChild.path, &pagedInfo)
        let pageReader = try pagedSource.listing(ListingStage.QueuedDirectory(path: Data(pagedChild.path.utf8),
            device: pagedInfo.st_dev, inode: pagedInfo.st_ino))!
        try require(try pageReader.page(limit: 128).count == 128, "first source page size")
        try require(try pageReader.page(limit: 128).count == 1, "short final source page size")
        try require(try pageReader.page(limit: 128).isEmpty && pageReader.page(limit: 128).isEmpty,
                    "source reader restarted after SQLITE_DONE")
        var pagedRootInfo = stat(); _ = lstat(pagedRoot.path, &pagedRootInfo)
        for validation in [StageReuseCandidate.HistoryValidation.sharedStream, .verifiedThroughParentCheckpoint] {
            let parentCheckpoint = StageCheckpoint(rootPath: pagedRoot.path, identity: "events", eventID: 20)
            let discovery = DiscoveryReuse(provider: { _, _ in
                StageReuseCandidate(snapshot: pagedSaved, checkpoint: parentCheckpoint, historyValidation: validation)
            }, cache: pagedCache, root: pagedRoot,
                identity: DirectoryIdentity(device: pagedRootInfo.st_dev, inode: pagedRootInfo.st_ino),
                checkpoint: parentCheckpoint, cancellation: ScanCancellation())
            let listing = try discovery.listing(ListingStage.QueuedDirectory(path: Data(pagedChild.path.utf8),
                device: pagedInfo.st_dev, inode: pagedInfo.st_ino))
            try require((listing != nil) == (validation == .verifiedThroughParentCheckpoint),
                        "source cursor floor was relaxed without explicit history verification")
        }
        let lookupEntered = DispatchSemaphore(value: 0), lookupRelease = DispatchSemaphore(value: 0)
        let otherLookup = DispatchSemaphore(value: 0), lookupFinished = DispatchSemaphore(value: 0)
        let sharedDiscovery = DiscoveryReuse(provider: { directory, _ in
            if directory == pagedChild { lookupEntered.signal(); lookupRelease.wait() }
            else { otherLookup.signal() }
            return nil
        }, cache: pagedCache, root: pagedRoot,
            identity: DirectoryIdentity(device: pagedRootInfo.st_dev, inode: pagedRootInfo.st_ino),
            checkpoint: StageCheckpoint(rootPath: pagedRoot.path, identity: "events", eventID: 20), cancellation: ScanCancellation())
        let otherDiscovery = sharedDiscovery.clone(cancellation: ScanCancellation())
        DispatchQueue.global().async {
            _ = try? sharedDiscovery.listing(ListingStage.QueuedDirectory(path: Data(pagedChild.path.utf8), device: pagedInfo.st_dev, inode: pagedInfo.st_ino))
            lookupFinished.signal()
        }
        try require(lookupEntered.wait(timeout: .now() + 2) == .success, "validation fixture did not start")
        DispatchQueue.global().async {
            _ = try? otherDiscovery.listing(ListingStage.QueuedDirectory(path: Data((pagedRoot.path + "/other").utf8), device: pagedInfo.st_dev, inode: pagedInfo.st_ino))
        }
        let independent = otherLookup.wait(timeout: .now() + 2) == .success
        lookupRelease.signal()
        try require(lookupFinished.wait(timeout: .now() + 2) == .success && independent,
                    "one history validator blocked another discovery worker")
        print("PASS: short final source pages reach stable exhaustion exactly once")

        let child = try directory("resume/root/child"), root = child.deletingLastPathComponent()
        let cache = base.appendingPathComponent("resume/cache")
        for number in 0..<600 { try write(child.appendingPathComponent(String(format: "%04d", number))) }
        _ = try save(child, cache)
        do {
            let parent = try stage(root, cache)
            while parent.testReusedDirectories == 0 { _ = try parent.advance(progress: { _ in }) }
            try require(parent.checkpoint.eventID == 5, "floor not committed before copied directory")
        }
        do {
            let parent = try stage(root, cache, event: 30)
            try require(parent.checkpoint.eventID == 5, "restart lost inherited event floor")
            try finish(parent)
            try require(parent.testReusedDirectories == 0, "restart recopied a completed directory")
            try verify(root, cache, parent.publish(elapsed: 0))
        }
        print("PASS: stop/reopen preserves copied directory and original event checkpoint")

        let cancelChild = try directory("cancel/root/child"), cancelRoot = cancelChild.deletingLastPathComponent()
        let cancelCache = base.appendingPathComponent("cancel/cache")
        for number in 0..<700 { try write(cancelChild.appendingPathComponent(String(format: "%04d", number))) }
        _ = try save(cancelChild, cancelCache)
        do {
            let cancellation = ScanCancellation(), parent = try stage(cancelRoot, cancelCache, cancellation: cancellation)
            parent.configureTestScanner(DiskScanner(testBatch: { cancellation.cancel() }))
            do { try finish(parent); throw Failure(message: "import ignored cancellation") }
            catch is CancellationError { }
            try require(parent.testReusedDirectories == 1 && parent.checkpoint.eventID == 5, "cancellation lost reuse provenance")
        }
        let cancelResume = try stage(cancelRoot, cancelCache, event: 30)
        let cancelledTotals = try cancelResume.crawler.store.folderTotals(path: Data(cancelChild.path.utf8))
        try require((cancelledTotals?.count ?? 0) >= 512 && cancelResume.checkpoint.eventID == 5, "cancellation lost imported pages")
        try finish(cancelResume)
        try verify(cancelRoot, cancelCache, cancelResume.publish(elapsed: 0))
        print("PASS: cancellation after copied pages propagates and preserves durable progress")

        let crashChild = try directory("crash/root/child"), crashRoot = crashChild.deletingLastPathComponent()
        let crashCache = base.appendingPathComponent("crash/cache")
        for number in 0..<700 { try write(crashChild.appendingPathComponent(String(format: "%04d", number))) }
        _ = try save(crashChild, crashCache)
        let process = Process()
        process.executableURL = URL(fileURLWithPath: CommandLine.arguments[0])
        process.arguments = ["--kill-after-reuse", crashRoot.path, crashCache.path]
        try process.run(); process.waitUntilExit()
        try require(process.terminationReason == .uncaughtSignal && process.terminationStatus == SIGKILL, "import process missed kill boundary")
        let crashResume = try stage(crashRoot, crashCache, event: 30)
        let crashTotals = try crashResume.crawler.store.folderTotals(path: Data(crashChild.path.utf8))
        try require(crashResume.checkpoint.eventID == 5 && (crashTotals?.count ?? 0) >= 512, "crash lost committed imported pages or event floor")
        try finish(crashResume)
        try verify(crashRoot, crashCache, crashResume.publish(elapsed: 0))
        print("PASS: SIGKILL after imported pages retains durable progress and resumes without double accounting")

        let legacyChild = try directory("legacy/root/child"), legacyRoot = legacyChild.deletingLastPathComponent()
        let legacyCache = base.appendingPathComponent("legacy/cache")
        try write(legacyChild.appendingPathComponent("file"))
        _ = try DiskIndexStore(directory: legacyCache).save(DiskScanner().scan(legacyChild), eventID: 5, eventIdentity: "events")
        let legacy = try stage(legacyRoot, legacyCache)
        try finish(legacy)
        try require(legacy.testReusedDirectories == 0 && legacy.checkpoint.eventID == 20, "source without removal provenance was accepted")
        try verify(legacyRoot, legacyCache, legacy.publish(elapsed: 0))
        print("PASS: legacy source without removal cursor falls back without lowering checkpoint")

        let updatedChild = try directory("updated/root/child"), updatedRoot = updatedChild.deletingLastPathComponent()
        let updatedCache = base.appendingPathComponent("updated/cache"), updatedFile = updatedChild.appendingPathComponent("file")
        try write(updatedFile)
        let before = try save(updatedChild, updatedCache)
        try write(updatedFile, 900)
        _ = try DiskIndexStore(directory: updatedCache).update(snapshot: before,
            replacements: [IndexReplacement(path: updatedChild.path, result: DiskScanner().scan(updatedChild))], eventID: 6, eventIdentity: "events")
        let updated = try stage(updatedRoot, updatedCache)
        try finish(updated)
        try require(updated.testReusedDirectories == 1 && updated.checkpoint.eventID == 6, "normal saved replacement rows were not reused")
        try verify(updatedRoot, updatedCache, updated.publish(elapsed: 0))
        print("PASS: saved refresh replacements reuse normal metadata without private staging columns")
        try checkNameFoldFallback(base.appendingPathComponent("name-fold"))

        for during in [false, true] {
            let group = during ? "during" : "before"
            let child = try directory("\(group)/root/child"), root = child.deletingLastPathComponent(), cache = base.appendingPathComponent("\(group)/cache")
            for number in 0..<700 { try write(child.appendingPathComponent(String(format: "%04d", number))) }
            _ = try save(child, cache)
            let victim = child.appendingPathComponent("0699"), victimIdentity = try identity(victim), rootIdentity = try identity(child)
            let remove: @Sendable () throws -> Void = {
                let journal = try RemovalJournal(cache: cache)
                let operation = try journal.begin(root: child, rootIdentity: rootIdentity, items: [ScanRemoval(path: victim.path, identity: victimIdentity)])
                try FileManager.default.removeItem(at: victim); try operation.complete()
            }
            if !during { try remove() }
            let parent = try stage(root, cache)
            let once = Once()
            if during { parent.configureTestScanner(DiskScanner(testBatch: { if once.take() { try remove() } })) }
            try finish(parent)
            if !during { try require(parent.testReusedDirectories == 0 && parent.checkpoint.eventID == 20, "historical deletion accepted an old source") }
            try verify(root, cache, parent.publish(elapsed: 0))
        }
        print("PASS: preexisting and between-page deletion intents cannot restore removed records")

        let pinnedChild = try directory("pinned/root/child"), pinnedRoot = pinnedChild.deletingLastPathComponent()
        let pinnedCache = base.appendingPathComponent("pinned/cache")
        for number in 0..<700 { try write(pinnedChild.appendingPathComponent(String(format: "%04d", number))) }
        let pinnedSaved = try save(pinnedChild, pinnedCache)
        let pinned = try stage(pinnedRoot, pinnedCache), publishOnce = Once()
        let later = pinnedChild.appendingPathComponent("later")
        pinned.configureTestScanner(DiskScanner(testBatch: {
            if publishOnce.take() {
                try Data([1, 2, 3]).write(to: later)
                try Data(repeating: 4, count: 900).write(to: pinnedChild.appendingPathComponent("0000"))
                try FileManager.default.removeItem(at: pinnedChild.appendingPathComponent("0001"))
                _ = try DiskIndexStore(directory: pinnedCache).update(snapshot: pinnedSaved,
                    replacements: [IndexReplacement(path: pinnedChild.path, result: DiskScanner().scan(pinnedChild))],
                    eventID: 6, eventIdentity: "events")
            }
        }))
        try finish(pinned)
        let pinnedResult = try pinned.publish(elapsed: 0)
        let pinnedStore = DiskIndexStore(directory: pinnedCache)
        if try pinnedStore.node(path: later.path, snapshot: pinnedResult) == nil {
            let oldRoot = try pinnedStore.node(path: pinnedRoot.path, snapshot: pinnedResult)
            try require(oldRoot?.size == 700 * 25 && oldRoot?.fileCount == 700, "old source snapshot became mixed")
        } else {
            try verify(pinnedRoot, pinnedCache, pinnedResult)
        }
        try require(pinnedResult.eventID == 5, "pinned source lost older change replay floor")
        print("PASS: replacing a saved source retains a coherent version or fully reconciles, preserving event floor")

        let priorityRoot = try directory("priority/root"), priorityCache = base.appendingPathComponent("priority/cache")
        _ = try directory("priority/root/a/unrelated")
        let target = try directory("priority/root/z/target")
        let prioritized = try stage(priorityRoot, priorityCache, reuse: false)
        let preview = DiskScanPreview(); preview.prioritize(path: target.path); preview.attach(stage: prioritized)
        while try prioritized.crawler.store.directory(path: Data((priorityRoot.path + "/z").utf8)) == nil {
            _ = try prioritized.advance(progress: { _ in })
        }
        while try prioritized.crawler.store.hasTrim() { _ = try prioritized.advance(progress: { _ in }) }
        let next = try prioritized.crawler.store.nextDirectory(preferred: Data(target.path.utf8))
        try require(next?.path == Data((priorityRoot.path + "/z").utf8), "unknown target did not prioritize its queued ancestor")
        try prioritized.crawler.store.markDirty(parent: Data((priorityRoot.path + "/a").utf8))
        let dirty = try prioritized.crawler.store.nextDirectory(preferred: Data(target.path.utf8))
        try require(dirty?.path == Data((priorityRoot.path + "/a").utf8), "priority overtook removal reconciliation")
        try finish(prioritized)
        print("PASS: requested pending descendant priority preserves dirty-work precedence")

        // Foundation resolves TMPDIR aliases to /var, which uses the host event stream.
        let eventsBase = URL(fileURLWithPath: files.currentDirectoryPath, isDirectory: true)
            .appendingPathComponent(".orbit-reuse-events-\(UUID().uuidString)", isDirectory: true)
        defer { try? files.removeItem(at: eventsBase) }
        let actualChild = eventsBase.appendingPathComponent("root/child"), actualRoot = actualChild.deletingLastPathComponent()
        let actualCache = eventsBase.appendingPathComponent("cache")
        try files.createDirectory(at: actualChild, withIntermediateDirectories: true)
        try write(actualChild.appendingPathComponent("file"))
        let sourceEvent = try DiskChangeMonitor.capture(root: actualChild), parentEvent = try DiskChangeMonitor.capture(root: actualRoot)
        let actualStage = try ResumableStage(cache: actualCache, root: actualChild, current: StageCheckpoint(rootPath: actualChild.path, identity: sourceEvent.identity, eventID: sourceEvent.eventID))
        try finish(actualStage); let actualSaved = try actualStage.publish(elapsed: 0)
        let candidate = try DiskChangeMonitor.reuseCandidate(actualSaved, for: StageCheckpoint(rootPath: actualRoot.path, identity: parentEvent.identity, eventID: parentEvent.eventID))
        try require(sourceEvent.identity.contains("\"mode\":\"device\""), "workspace fixture did not select device event history")
        if sourceEvent.identity.contains("\"eventsUUID\"") {
            try require(candidate != nil, "same-volume device history was rejected")
        } else {
            try require(candidate == nil, "unavailable event history was accepted")
            print("PASS: unavailable sandbox FSEvents history safely declines reuse")
        }
        let wrong = try DiskChangeMonitor.reuseCandidate(actualSaved, for: StageCheckpoint(rootPath: actualRoot.path, identity: "wrong", eventID: parentEvent.eventID))
        try require(wrong == nil, "changed parent history identity was accepted")
        let host = try DiskChangeMonitor.capture(root: URL(fileURLWithPath: "/"))
        let hostCheckpoint = StageCheckpoint(rootPath: "/", identity: host.identity, eventID: host.eventID)
        try require(!sourceEvent.identity.contains("bootTraversal"), "child checkpoint identity was unnecessarily changed")
        try checkBootTraversalUpgrade(base.appendingPathComponent("boot-upgrade"), current: hostCheckpoint)
        DiskChangeMonitor.testReuseHistoryValidator = { _, _ in false }
        try require(try DiskChangeMonitor.reuseCandidate(actualSaved, for: hostCheckpoint) == nil,
                    "unverified mixed-stream source was accepted")
        var validatedSource: (String, UInt64)?
        DiskChangeMonitor.testReuseHistoryValidator = { url, cursor in validatedSource = (url.path, cursor); return true }
        defer { DiskChangeMonitor.testReuseHistoryValidator = nil }
        let hostCandidate = try DiskChangeMonitor.reuseCandidate(actualSaved, for: hostCheckpoint)
        try require(try DiskChangeMonitor.reuseCandidate(actualSaved,
            for: StageCheckpoint(rootPath: "/", identity: host.identity, eventID: UInt64.max)) == nil,
            "future parent host checkpoint was accepted")
        if sourceEvent.identity.contains("\"eventsUUID\""), try MappingCoverage.canReach(root: URL(fileURLWithPath: "/"), target: actualChild,
            expectedRootIdentity: identity(URL(fileURLWithPath: "/"))) {
            try require(hostCandidate?.checkpoint == hostCheckpoint
                && hostCandidate?.historyValidation == .verifiedThroughParentCheckpoint
                && validatedSource?.0 == actualChild.path && validatedSource?.1 == actualSaved.eventID,
                "verified child did not retain the parent host checkpoint")
        } else {
            try require(hostCandidate == nil, "unreachable or unjournaled source bypassed history validation")
        }
        DiskChangeMonitor.testReuseHistoryValidator = nil
        try require(try !DiskChangeMonitor.testReuseHistory(root: actualChild, since: sourceEvent.eventID, timeout: 0),
                    "exhausted validation budget permitted reuse")
        let history = FSEventStreamEventFlags(kFSEventStreamEventFlagHistoryDone)
        try require(DiskChangeMonitor.testReuseHistoryDecision(flags: [history], flushed: true), "quiet flushed history was rejected")
        try require(!DiskChangeMonitor.testReuseHistoryDecision(flags: [], flushed: true)
            && !DiskChangeMonitor.testReuseHistoryDecision(flags: [history], flushed: false)
            && !DiskChangeMonitor.testReuseHistoryDecision(flags: [history], flushed: true, expired: true),
            "incomplete, unflushed or expired history was accepted")
        for change in [0, kFSEventStreamEventFlagMustScanSubDirs, kFSEventStreamEventFlagUserDropped,
            kFSEventStreamEventFlagKernelDropped, kFSEventStreamEventFlagEventIdsWrapped,
            kFSEventStreamEventFlagRootChanged, kFSEventStreamEventFlagMount, kFSEventStreamEventFlagUnmount] {
            try require(!DiskChangeMonitor.testReuseHistoryDecision(flags: [FSEventStreamEventFlags(change), history], flushed: true),
                        "changed or incomplete source history was accepted")
        }
        try require(!DiskChangeMonitor.testReuseHistoryDecision(flags: [history | FSEventStreamEventFlags(kFSEventStreamEventFlagRootChanged)], flushed: true),
                    "combined history flags hid invalidation")
        if candidate != nil {
            // Newly created fixtures remain in the conservative timestamp cursor's replay window.
            let quietRoot = URL(fileURLWithPath: files.currentDirectoryPath).appendingPathComponent("Sources/CSQLite")
            var quiet = false
            for _ in 0..<40 {
                let checkpoint = try DiskChangeMonitor.capture(root: quietRoot)
                if try DiskChangeMonitor.testReuseHistory(root: quietRoot, since: checkpoint.eventID) {
                    quiet = true; break
                }
                usleep(250_000)
            }
            try require(quiet, "native verifier never accepted a quiet, flushed source")
            try write(actualChild.appendingPathComponent("file"), 41)
            var changed = false
            for _ in 0..<40 {
                if try !DiskChangeMonitor.testReuseHistory(root: actualChild, since: sourceEvent.eventID) { changed = true; break }
                usleep(250_000)
            }
            try require(changed, "native verifier missed a journaled source change after its saved cursor")
        }
        if hostCandidate != nil {
            let physicalAlias = "/System/Volumes/Data" + actualChild.path + "/deleted"
            try require(try DiskChangeMonitor.testBatchDirectories(root: URL(fileURLWithPath: "/"), event: physicalAlias).contains(actualChild.path),
                        "physical Data alias update did not reach its native map location")
            let physicalOnly = "/System/Volumes/Data/OrbitMissingFixture/deleted"
            let normalized = try DiskChangeMonitor.testNormalize(root: URL(fileURLWithPath: "/"), event: physicalOnly)
            try require(normalized == physicalOnly && (try DiskChangeMonitor.testBatchDirectories(root: URL(fileURLWithPath: "/"), event: physicalOnly))
                .contains(URL(fileURLWithPath: physicalOnly).deletingLastPathComponent().path),
                "tracked physical Data update was discarded as a nested mount")
        }
        print("PASS: real FSEvents identities; cross-stream reuse needs verified source history and preserves host cursor")
    }
}
