import Darwin
import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

extension AsyncMappingChecks {
    static func liveCacheInvalidation(_ base: URL) throws {
        let root = base.appendingPathComponent("live-cache-invalidation", isDirectory: true)
        let rootPath = Data(root.path.utf8)
        let folderPath = HardLinkPathOrder.path(rootPath, Data("folder".utf8))
        let nestedPath = HardLinkPathOrder.path(folderPath, Data("nested".utf8))
        let rootIdentity = DirectoryIdentity(device: 1, inode: 1)
        let oldIdentity = DirectoryIdentity(device: 1, inode: 2)
        let nestedIdentity = DirectoryIdentity(device: 1, inode: 3)
        let newIdentity = DirectoryIdentity(device: 1, inode: 4)
        let token = ScanCancellation()
        let inventory = LiveMappingInventory(root: root, identity: rootIdentity, cancellation: token)
        var sequence: Int64 = 0
        func record(_ name: String, size: Int64 = 0, inode: UInt64 = 10, directory: Bool = false) -> FileMetadata {
            FileMetadata(name: name, device: 1, inode: inode, isDirectory: directory, isSymbolicLink: false,
                isMountPoint: false, linkCount: 1, size: size, allocatedSize: directory ? 0 : size + 1)
        }
        func append(_ path: Data, _ identity: DirectoryIdentity, _ records: [FileMetadata], complete: Bool = false) {
            sequence += 1
            inventory.append(PendingInventoryBatch(sequence: sequence, parentPath: path, parentIdentity: identity,
                components: [], records: records,
                completion: complete ? PendingListingCompletion(issueCount: 0, issues: []) : nil, failure: nil))
        }
        func view() throws -> ScanPreviewView {
            let count = try { () throws -> Int? in
                let previous = PendingInventoryRead.testBeforeBuild
                PendingInventoryRead.testBeforeBuild = { throw AsyncCheckFailure(description: "Count-only read built full metadata/hard-link aggregates") }
                defer { PendingInventoryRead.testBeforeBuild = previous }
                return try inventory.fileCount(removals: [], cancellation: token)
            }()
            guard let result = try inventory.view(path: nil, removals: [], cancellation: token) else {
                throw AsyncCheckFailure(description: "Live cache lost its root")
            }
            try require(count == result.root.fileCount,
                        "Count-only inventory disagreed after a listing, identity or reachability change")
            return result
        }
        func matching(_ query: String) throws -> Int {
            try inventory.search(query: query, removals: [], cancellation: token, limit: 10).count
        }
        inventory.begin(rootPath, identity: rootIdentity)
        append(rootPath, rootIdentity, [record("folder", inode: 2, directory: true)], complete: true)
        inventory.begin(folderPath, identity: oldIdentity)
        append(folderPath, oldIdentity, [record("old-direct", size: 10), record("nested", inode: 3, directory: true)])
        inventory.begin(nestedPath, identity: nestedIdentity)
        append(nestedPath, nestedIdentity, [record("old-nested", size: 50)], complete: true)
        let first = try view()
        try require(first.root.size == 60 && first.folder.children.first?.size == 60, "Live cache fixture totals differ")
        let folderRemoval = ScanRemoval(path: String(decoding: folderPath, as: UTF8.self), identity: "1:2")
        let nestedRemoval = ScanRemoval(path: String(decoding: nestedPath, as: UTF8.self), identity: "1:3")
        let fileRemoval = ScanRemoval(path: String(decoding: folderPath, as: UTF8.self) + "/old-direct", identity: "1:10")
        let staleRemoval = ScanRemoval(path: folderRemoval.path, identity: "1:4")
        for removals in [[folderRemoval], [nestedRemoval], [fileRemoval, fileRemoval],
                         [folderRemoval, nestedRemoval, fileRemoval], [staleRemoval, nestedRemoval]] {
            let previous = PendingInventoryRead.testBeforeBuild
            PendingInventoryRead.testBeforeBuild = { throw AsyncCheckFailure(description: "Removal-projected count built metadata/hard-link aggregates") }
            let count: Int?
            do { count = try inventory.fileCount(removals: removals, cancellation: token) }
            catch { PendingInventoryRead.testBeforeBuild = previous; throw error }
            PendingInventoryRead.testBeforeBuild = previous
            let projected = try inventory.view(path: nil, removals: removals, cancellation: token)
            try require(count == projected?.root.fileCount, "Count-only directory/file removals disagreed with the projected view")
        }
        try require(try inventory.fileCount(removals: [ScanRemoval(path: root.path, identity: "1:1")], cancellation: token) == nil,
                    "Count-only root removal retained its tree")
        try require(try inventory.fileCount(removals: [ScanRemoval(path: root.path, identity: "1:99")], cancellation: token) == 2,
                    "Stale root identity removed the current count")
        try require(try matching("old-") == 2, "Live cache fixture must find both old descendants before restarting")
        try require(try view().revision == first.revision, "Unchanged live read changed its version")
        append(folderPath, oldIdentity, [record("partial", size: 7)])
        let partial = try view()
        try require(partial.revision > first.revision && partial.root.size == 67 && partial.root.fileCount == 3,
                    "Appending a partial listing reused stale cached totals")
        try require(try matching("partial") == 1, "Appending a partial listing left cached search stale")

        inventory.begin(folderPath, identity: oldIdentity)
        let restarted = try view()
        try require(restarted.revision > partial.revision && restarted.root.size == 0 && restarted.root.fileCount == 0,
                    "Restarting a listing retained its old child contributions")
        try require(try matching("old-") == 0, "Restarting a listing retained unreachable cached descendants")
        try require(try matching("folder") == 1, "Restarting a child listing removed its still-reachable parent folder")
        append(folderPath, oldIdentity, [record("old-again", size: 7), record("nested", inode: 3, directory: true)])
        let restored = try view()
        try require(restored.root.size == 57, "Live fixture did not restore the old descendant before replacement")
        inventory.begin(rootPath, identity: rootIdentity)
        let empty = try view()
        try require(empty.revision > restored.revision && empty.root.size == 0 && empty.folder.children.isEmpty,
                    "Restarting the root reused an earlier cached tree")
        append(rootPath, rootIdentity, [record("folder", inode: 4, directory: true)], complete: true)
        let replacement = try view()
        try require(replacement.root.size == 0 && replacement.folder.children.first?.fileIdentity == "1:4",
                    "A replacement directory inherited cached descendants from its old identity")
        append(folderPath, newIdentity, [record("new-visible", size: 23)], complete: true)
        let fresh = try view()
        try require(fresh.revision > replacement.revision && fresh.root.size == 23 && fresh.root.fileCount == 1,
                    "Appending a replacement listing reused an earlier cache version")
        try require(try matching("old-") == 0 && matching("new-visible") == 1,
                    "Replacement search retained the previous directory identity")

        inventory.unavailable(folderPath, identity: newIdentity, missing: false)
        let denied = try view()
        try require(denied.revision > fresh.revision && denied.root.size == 0 && denied.root.fileCount == 0
            && denied.folder.children.first?.isAccessible == false && denied.folder.children.first?.scanState == .complete,
            "Permission failure reused cached sizes or accessibility")
        try require(try matching("new-visible") == 0, "Permission failure retained cached file search results")
        inventory.begin(folderPath, identity: newIdentity)
        append(folderPath, newIdentity, [record("new-restored", size: 29)], complete: true)
        let accessible = try view()
        try require(accessible.root.size == 29 && accessible.folder.children.first?.isAccessible == true,
                    "Restarting a denied directory retained cached inaccessibility")
        inventory.unavailable(folderPath, identity: newIdentity, missing: true)
        let missing = try view()
        try require(missing.revision > accessible.revision && missing.root.size == 0 && missing.folder.children.isEmpty,
                    "A removed directory remained in the cached view")
        try require(try matching("new") == 0 && inventory.confirmsMissingDirectory(String(decoding: folderPath, as: UTF8.self)),
                    "A removed directory retained cached search or navigation state")
        try require(first.root.size == 60 && partial.root.size == 67,
                    "Later live mutations changed already delivered views")
        print("PASS: cached live views update after partial append, restart, replacement, denial and removal")
    }

    static func liveViewsWithoutDatabase(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "live-without-database")
        let folder = try fixture.directory("folder")
        for index in 0..<270 { try fixture.file("folder/visible-\(index)", size: index + 1) }
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { DiskIndexStore.testBeforeDatabaseOpen = nil; stage.crawler.cancelAsyncDiscovery() }
        try signals.until("live discovery before any save") { mailbox.discoveryFinished }
        for saved in [false, true] {
            if saved { try finish(stage) }
            DiskIndexStore.testBeforeDatabaseOpen = {
                throw AsyncCheckFailure(description: "Live UI attempted to read the database")
            }
            let view = try stage.view(path: folder.path, removals: [], cancellation: ScanCancellation())
            try require(view?.folder.children.count == 270, "Live listing depends on database availability")
            let search = try stage.search(query: "visible-", cancellation: ScanCancellation(), removals: [], limit: 20)
            try require(search.count == 270 && search.nodes.first?.size == 270,
                        "Live search depends on database availability")
            let victim = search.nodes[0]
            let removal = ScanRemoval(path: victim.id, identity: victim.fileIdentity!)
            let projected = try stage.view(path: folder.path, removals: [removal], cancellation: ScanCancellation())
            try require(projected?.folder.children.count == 269, "Live removal projection depends on saving")
            try require(!stage.confirmsMissingDirectory(folder.path), "Live navigation depends on saving")
            try require(stage.confirmsMissingDirectory(fixture.root.appendingPathComponent("absent").path),
                        "Completed live root could not confirm a missing child")
            DiskIndexStore.testBeforeDatabaseOpen = nil
        }
        try require(mailbox.isDrained, "Test did not retire saved batches")
        print("PASS: browsing, search and removal projection open no database before writes or after queue retirement")
    }

    static func refreshedLiveDiscovery(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "full-refresh-live")
        try fixture.file("existing/old", size: 3)
        for index in 0..<130 { try fixture.directory("existing/empty-\(index)") }
        do {
            let stage = try fixture.stage()
            try finish(stage)
            try stage.crawler.store.markFullRefresh()
        }
        let folder = try fixture.directory("new/nested")
        try fixture.file("new/nested/visible", size: 7)
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { stage.crawler.cancelAsyncDiscovery() }
        var serialListings = 0
        stage.crawler.testBeforeListing = { serialListings += 1 }
        try signals.until("resumed full refresh with writer held") { mailbox.discoveryFinished }
        try require(try stage.view(path: folder.path, removals: [], cancellation: ScanCancellation())?.folder.children.count == 1,
                    "Full refresh waited for durable directory registration")
        try require(try stage.crawler.store.fullRefreshState().pending, "Fixture lost pending full-refresh marking")
        let savedSequence = try scalar(stage.crawler.store.databasePath, "SELECT COALESCE(MAX(sequence),0) FROM orbit_async_progress")
        _ = try stage.advance(progress: { _ in })
        try require(try stage.crawler.store.fullRefreshState().pending,
                    "Full-refresh preparation did not stay within its bounded directory chunk")
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COALESCE(MAX(sequence),0) FROM orbit_async_progress") == savedSequence,
                    "Writer consumed discovered listings before marking the old directory queue")
        try finish(stage)
        try require(serialListings == 0, "Full refresh repeated asynchronous discovery in the serial crawler")
        try require(stage.crawler.asyncInventory != nil, "A newly discovered directory invalidated the refresh generation")
        try verifyTree(stage, fixture: fixture)
        print("PASS: bounded full-refresh preparation preserves live discovery without enumerating directories twice")
    }

    static func sharedLiveDiscovery(_ base: URL) throws {
        for busy in [false, true] {
            let fixture = try AsyncFixture(base, busy ? "busy-shared-live" : "shared-live")
            let child = try fixture.directory("child")
            for index in 0..<150 { try fixture.file("child/cached-\(index)", size: index + 1) }
            var branches: [URL] = [], leaves: [URL] = []
            for branch in 0..<6 {
                branches.append(try fixture.directory("child/branch-\(branch)"))
                leaves.append(try fixture.directory("child/branch-\(branch)/nested"))
                for index in 0..<30 { try fixture.file("child/branch-\(branch)/direct-\(index)", size: index + 1) }
                for index in 0..<20 { try fixture.file("child/branch-\(branch)/nested/leaf-file-\(index)", size: index + 1) }
            }
            let saved: IndexSnapshot
            do {
                let stage = try ResumableStage(cache: fixture.cache, root: child,
                    current: StageCheckpoint(rootPath: child.path, identity: "async-check", eventID: 0))
                try finish(stage); saved = try stage.publish(elapsed: 0)
            }
            let parent = try fixture.stage(), signals = Signals(), seen = DiscoveredPaths()
            let workers = SharedReuseWorkers(branches: branches)
            parent.configureReuse { directory, checkpoint in
                directory.path == child.path ? StageReuseCandidate(snapshot: saved, checkpoint: checkpoint) : nil
            }
            try parent.prepareRemovalReplay()
            AsyncListingProducer.testRecords = { path, records in seen.insert(path); return records }
            AsyncListingProducer.testBeforeDirectory = { path, _ in workers.enter(path) }
            defer {
                workers.release()
                AsyncListingProducer.testRecords = nil; AsyncListingProducer.testBeforeDirectory = nil
                parent.crawler.cancelAsyncDiscovery()
            }
            func discover() throws {
                try parent.startDiscovery(progress: { _ in signals.signal() })
                try signals.until("shared live discovery without writer", timeout: 10) {
                    parent.crawler.asyncInventory?.discoveryFinished == true
                }
                try workers.verify()
                let view = try parent.view(path: child.path, removals: [], cancellation: ScanCancellation())
                try require(view?.folder.children.count == 156 && view?.folder.fileCount == 450 && view?.folder.size == 15_375,
                            "Shared descendant totals waited for durable ingestion or lost records across workers")
                for branch in branches {
                    let listing = try parent.view(path: branch.path, removals: [], cancellation: ScanCancellation())
                    try require(listing?.folder.children.count == 31, "A shared branch was absent from the live map")
                }
                for leaf in leaves {
                    let listing = try parent.view(path: leaf.path, removals: [], cancellation: ScanCancellation())
                    try require(listing?.folder.children.count == 20, "A shared nested listing was absent from the live map")
                }
                let search = try parent.search(query: "leaf-file-", cancellation: ScanCancellation(), removals: [], limit: 10)
                try require(search.count == 120, "Shared nested files were absent from live search")
                if busy {
                    for directory in [child] + branches + leaves {
                        try require(seen.contains(Data(directory.path.utf8)), "Busy shared cache skipped metadata fallback for \(directory.lastPathComponent)")
                    }
                } else {
                    try require(!seen.containsSubtree(Data(child.path.utf8)), "A worker re-enumerated part of the shared subtree")
                }
                try require(try scalar(parent.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes") == 1,
                            "Shared discovery needed writer commits")
            }
            if busy {
                try RemovalJournal(cache: fixture.cache).withFence { _ in try discover() }
            } else { try discover() }
            try finish(parent)
            try verifyTree(parent, fixture: fixture)
        }
        print("PASS: four workers reuse shared descendants before ingestion; a held journal fence falls back without waiting")
    }

    static func resumedLiveDiscovery(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "resumed-live")
        let complete = try fixture.directory("complete"), pending = try fixture.directory("pending")
        for index in 0..<40 { try fixture.file("complete/cached-\(index)", size: index + 1) }
        try fixture.file("pending/old", size: 3)
        do {
            let stage = try fixture.stage()
            try finish(stage)
            try stage.crawler.markChanged(directory: pending.path)
        }
        for index in 0..<60 { try fixture.file("pending/new-\(index)", size: index + 1) }
        let stage = try fixture.stage(), signals = Signals(), seen = DiscoveredPaths()
        for _ in 0..<16 {
            let rejected = try? DiscoveryCacheReader(path: stage.crawler.store.databasePath,
                rootIdentity: DirectoryIdentity(device: 0, inode: 0), cancellation: ScanCancellation())
            try require(rejected == nil, "A seed for a different directory identity was accepted")
        }
        AsyncListingProducer.testRecords = { path, records in seen.insert(path); return records }
        defer { AsyncListingProducer.testRecords = nil; stage.crawler.cancelAsyncDiscovery() }
        let mailbox = try start(stage, signals: signals)
        try signals.until("resumed discovery with writer stopped") { mailbox.discoveryFinished }
        let cached = try stage.view(path: complete.path, removals: [], cancellation: ScanCancellation())
        let fresh = try stage.view(path: pending.path, removals: [], cancellation: ScanCancellation())
        try require(cached?.folder.children.count == 40, "Completed coverage was absent from live resume")
        try require(fresh?.folder.children.count == 61, "Pending directory waited for resumed writer")
        try require(!seen.contains(Data(complete.path.utf8)), "Resume re-enumerated a completed directory")
        try require(seen.contains(Data(pending.path.utf8)), "Resume failed to enumerate dirty directory")
        try finish(stage)
        try verifyTree(stage, fixture: fixture)
        print("PASS: resume reuses completed listings and discovers pending files before writer catch-up")
    }
}

private final class DiscoveredPaths: @unchecked Sendable {
    private let lock = NSLock()
    private var paths = Set<Data>()
    func insert(_ path: Data) { lock.lock(); paths.insert(path); lock.unlock() }
    func contains(_ path: Data) -> Bool { lock.lock(); defer { lock.unlock() }; return paths.contains(path) }
    func containsSubtree(_ root: Data) -> Bool {
        lock.lock(); defer { lock.unlock() }
        let prefix = root + Data([47])
        return paths.contains { $0 == root || $0.starts(with: prefix) }
    }
}

private final class SharedReuseWorkers: @unchecked Sendable {
    private let condition = NSCondition()
    private let branches: Set<Data>
    private var threads = Set<mach_port_t>()
    private var released = false
    private var timedOut = false

    init(branches: [URL]) { self.branches = Set(branches.map { Data($0.path.utf8) }) }

    func enter(_ path: Data) {
        guard branches.contains(path) else { return }
        condition.lock(); defer { condition.unlock() }
        threads.insert(pthread_mach_thread_np(pthread_self()))
        if threads.count == 4 { released = true; condition.broadcast() }
        let deadline = Date(timeIntervalSinceNow: 10)
        while !released {
            if !condition.wait(until: deadline) {
                timedOut = true; released = true; condition.broadcast()
            }
        }
    }

    func release() { condition.lock(); released = true; condition.broadcast(); condition.unlock() }

    func verify() throws {
        condition.lock(); defer { condition.unlock() }
        try require(!timedOut && threads.count == 4, "Shared descendant coverage did not run on four independent workers")
    }
}
