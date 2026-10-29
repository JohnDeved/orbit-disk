import Darwin
import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

extension AsyncMappingChecks {
    private static func freshLeafMailboxBounds() throws {
        let identity = DirectoryIdentity(device: 1, inode: 1)
        func mailbox() -> PendingInventoryMailbox {
            PendingInventoryMailbox(rootPath: Data("/fixture".utf8), rootIdentity: identity,
                                    cancellation: ScanCancellation(), onAccepted: {})
        }
        func batch(_ index: Int, count: Int = 0, directory: Bool = false, complete: Bool = true,
                   revision: Int64 = 0, generation: Int64 = 0) -> PendingInventoryBatch {
            let records = (0..<count).map { record in
                FileMetadata(name: "file-\(record)", device: 1, inode: UInt64(record + 1), isDirectory: directory,
                    isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 1, allocatedSize: 512)
            }
            return PendingInventoryBatch(sequence: 0, parentPath: Data("/fixture/\(index)".utf8), parentIdentity: identity,
                components: [], records: records,
                completion: complete ? PendingListingCompletion(issueCount: 0, issues: []) : nil,
                failure: nil, expectedRevision: revision, expectedFullGeneration: generation)
        }
        let empty = mailbox()
        for index in 0..<40 { try empty.enqueue(batch(index)) }
        try require(empty.nextFreshLeaves().count == 32 && empty.lastCommittedSequence == 0,
                    "Group peek exceeded its parent bound or retired unsaved memory")
        let records = mailbox()
        for index in 0..<32 { try records.enqueue(batch(index, count: 127)) }
        let bounded = records.nextFreshLeaves()
        try require(bounded.count > 1 && bounded.reduce(0) { $0 + $1.records.count } <= 1024
            && bounded.reduce(0) { $0 + $1.byteCount } <= 256 * 1024, "Group peek exceeded its record or byte budget")
        for barrier in [batch(2, count: 1, directory: true), batch(2, complete: false),
                        batch(2, revision: 1), batch(2, generation: 1), batch(2, count: 128)] {
            let queue = mailbox()
            try queue.enqueue(batch(0)); try queue.enqueue(batch(1)); try queue.enqueue(barrier); try queue.enqueue(batch(3))
            try require(queue.nextFreshLeaves().map(\.sequence) == [1, 2], "Group peek skipped an unsuitable mailbox entry")
        }
    }

    static func freshLeafGroups(_ base: URL) throws {
        try freshLeafMailboxBounds()
        let fixture = try AsyncFixture(base, "fresh-leaf-groups")
        for index in 0..<40 { try fixture.file("leaf-\(index)/file", size: index + 1) }
        let owner = try fixture.file("leaf-0/owner", size: 8192)
        try fm.linkItem(at: owner, to: fixture.root.appendingPathComponent("leaf-39/link"))
        try fixture.directory("empty-0"); try fixture.directory("empty-1")
        let expected = try DiskScanner().scan(fixture.root).root
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { ListingStage.testListingCommit = nil; stage.crawler.cancelAsyncDiscovery() }
        try signals.until("group discovery") { mailbox.discoveryFinished }
        let written = HookOutcome(), committed = HookOutcome()
        var groupAwaitingAccounting = false, groups = 0
        ListingStage.testListingCommit = { boundary in
            if boundary == "fresh-group-written" {
                groupAwaitingAccounting = true
                written.run {
                    let durable = try scalar(stage.crawler.store.databasePath, "SELECT COALESCE(MAX(sequence),0) FROM orbit_async_progress")
                    try require(durable == mailbox.lastCommittedSequence, "Uncommitted group exposed its durable watermark")
                    let view = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())!.folder
                    try require(view.fileCount == expected.fileCount && view.allocatedSize == expected.allocatedSize,
                                "Group transaction blocked or changed the live map")
                }
            } else if boundary == "fresh-group-committed" {
                groups += 1
                committed.run {
                    let durable = try scalar(stage.crawler.store.databasePath, "SELECT sequence FROM orbit_async_progress")
                    try require(durable > mailbox.lastCommittedSequence, "Group acknowledged memory before durable commit")
                }
            }
        }
        var callbacks: Set<Data> = []
        while try stage.crawler.stepAsync(beforeAccounting: {
            if groupAwaitingAccounting {
                let durable = try scalar(stage.crawler.store.databasePath, "SELECT sequence FROM orbit_async_progress")
                try require(durable > mailbox.lastCommittedSequence, "Group retired before hardlink accounting")
                let view = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())!.folder
                try require(view.allocatedSize == expected.allocatedSize, "Group commit exposed unsettled hardlink totals")
                groupAwaitingAccounting = false
            }
        }, onBatch: { path, _ in
            callbacks.insert(path)
            let durable = try scalar(stage.crawler.store.databasePath, "SELECT sequence FROM orbit_async_progress")
            try require(durable == mailbox.lastCommittedSequence, "Batch callback ran before committed memory retirement")
        }) {}
        try written.verify(); try committed.verify()
        try require(groups > 0 && callbacks.count == 43, "Group did not report every completed parent")
        ListingStage.testListingCommit = nil
        try finish(stage)
        try verifyTree(stage, fixture: fixture)
        print("PASS: fresh leaf groups preserve live reads, commit watermarks, hardlink accounting and per-parent callbacks")
    }

    static func cancelledLeafGroup(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "cancelled-leaf-group")
        for index in 0..<8 { try fixture.file("leaf-\(index)/file") }
        do {
            let token = ScanCancellation(), stage = try fixture.stage(token), signals = Signals()
            let mailbox = try start(stage, signals: signals)
            defer { ListingStage.testListingCommit = nil; stage.crawler.cancelAsyncDiscovery() }
            try signals.until("cancelled group discovery") { mailbox.discoveryFinished }
            var interrupted = false
            ListingStage.testListingCommit = { boundary in
                if boundary == "fresh-group-written" { interrupted = true; token.cancel() }
            }
            do { while try stage.crawler.stepAsync() {}; throw AsyncCheckFailure(description: "Group ignored cancellation") }
            catch is CancellationError {}
            try require(interrupted, "Cancellation did not reach a grouped transaction")
            try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes WHERE is_dir=0") == 0,
                        "Cancelled group retained uncommitted files")
            try require(try scalar(stage.crawler.store.databasePath, "SELECT sequence FROM orbit_async_progress") == 1,
                        "Cancelled group advanced the durable watermark")
        }
        let resumed = try fixture.stage()
        try finish(resumed); try verifyTree(resumed, fixture: fixture)
        print("PASS: cancellation rolls back the whole leaf group and reopening resumes its pending parents")
    }

    static func removalDuringLeafGroup(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "removed-during-leaf-group")
        for index in 0..<8 { try fixture.file("leaf-\(index)/retained", size: index + 1) }
        let victim = try fixture.file("leaf-0/victim", size: 19)
        let removal = ScanRemoval(path: victim.path, identity: try identity(victim))
        let journal = try RemovalJournal(cache: fixture.cache)
        let stage = try fixture.stage(), signals = Signals()
        let mailbox = try start(stage, signals: signals)
        defer { ListingStage.testListingCommit = nil; stage.crawler.cancelAsyncDiscovery() }
        try signals.until("removal group discovery") { mailbox.discoveryFinished }
        let removed = HookOutcome()
        var replayPending = false
        ListingStage.testListingCommit = { boundary in
            if boundary == "fresh-group-written" {
                removed.run {
                    let operation = try journal.begin(root: fixture.root, rootIdentity: identity(fixture.root), items: [removal])
                    try fm.removeItem(at: victim)
                    try operation.complete()
                    replayPending = true
                    let view = try stage.view(path: victim.deletingLastPathComponent().path, removals: [removal], cancellation: ScanCancellation())
                    try require(view?.folder.children.count == 1, "Pending group prevented immediate removal projection")
                }
            }
        }
        while try stage.crawler.stepAsync(onBatch: { _, _ in
            if replayPending { replayPending = false; try stage.prepareRemovalReplay() }
        }) {}
        try removed.verify()
        ListingStage.testListingCommit = nil
        try finish(stage); try verifyTree(stage, fixture: fixture)
        try require(try scalar(stage.crawler.store.databasePath, "SELECT COUNT(*) FROM nodes WHERE name='victim'") == 0,
                    "Group commit resurrected a journaled removal")
        print("PASS: removal while a leaf group is uncommitted stays usable and replay removes its stale queued observation")
    }
}
