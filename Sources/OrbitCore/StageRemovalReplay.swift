import Darwin
import Foundation

// Scratch prototype: prepare/advance run on the stage's single writer worker.
final class StageRemovalReplay {
    enum Retry: Error { case journalChanged, cannotExpose }
    let journal: RemovalJournal
    let crawler: DurableCrawler
    let cancellation: ScanCancellation
    private let stateLock = NSLock()
    private var overlay: [ScanRemoval] = []
    private var blocksPreview = true
    private var stateGeneration: UInt64 = 0
    private var fullReconciliation = false
    private(set) var covered: RemovalCursor?
    private var target: RemovalCursor?
    private var needsSettle = true
    private var liveBarrier: Int64?
    private var nextProbe: TimeInterval = 0
    private let overlayLimit = 10_000
    #if ORBIT_TESTING
    static var testAfterFreshCapture: (@Sendable () -> Void)?
    #endif
    func viewState() -> (overlay: [ScanRemoval], blocked: Bool, cursor: RemovalCursor?, generation: UInt64) {
        stateLock.lock(); defer { stateLock.unlock() }; return (overlay, blocksPreview, covered, stateGeneration)
    }
    init(cache: URL, crawler: DurableCrawler, cancellation: ScanCancellation, fresh: Bool = false) throws {
        journal = try RemovalJournal(cache: cache); self.crawler = crawler; self.cancellation = cancellation
        covered = try crawler.store.removalCursor()
        if try fresh && covered == nil && crawler.store.isFreshForRemovalAnchor() {
            let end = try journal.withFence(cancellation: cancellation) { RemovalCursor(identity: $0.identity, sequence: $0.highWater) }
            var after: Int64 = 0, anchor = end.sequence
            while after < end.sequence {
                try check()
                let (page, firstLive) = try journal.withFence(cancellation: cancellation) { fence -> ([RemovalJournal.Entry], Int64?) in
                    guard fence.identity == end.identity, fence.highWater >= end.sequence else { throw Retry.journalChanged }
                    let page = try fence.pendingOperations(after: after, limit: 512).filter { $0.sequence <= end.sequence }
                    for entry in page { if try fence.isLive(sequence: entry.sequence) { return (page, entry.sequence) } }
                    return (page, nil)
                }
                if let firstLive { anchor = firstLive - 1; break }
                guard page.count == 512, let last = page.last else { break }
                after = last.sequence
            }
            #if ORBIT_TESTING
            Self.testAfterFreshCapture?()
            #endif
            let cursor = RemovalCursor(identity: end.identity, sequence: anchor)
            try crawler.store.writeRemovalCursor(cursor)
            covered = cursor
        }
    }
    private func check() throws {
        if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
    }
    func prepare() throws {
        stateLock.lock(); blocksPreview = true; stateGeneration &+= 1; stateLock.unlock()
        try crawler.store.resetReplayParents()
        let end = try journal.withFence(cancellation: cancellation) { RemovalCursor(identity: $0.identity, sequence: $0.highWater) }
        let mismatch = covered.map { $0.identity != end.identity || $0.sequence > end.sequence } ?? false
        var sequence = mismatch ? 0 : (covered?.sequence ?? 0)
        var kept: [ScanRemoval] = [], overflow = false, full = mismatch
        var pendingLive: Int64?
        while sequence < end.sequence {
            try check()
            let entry = try journal.withFence(cancellation: cancellation) { fence -> RemovalJournal.Entry in
                guard fence.identity == end.identity, fence.highWater >= end.sequence else { throw Retry.journalChanged }
                guard let next = try fence.operations(after: sequence, limit: 1).first, next.sequence <= end.sequence else { throw RemovalJournal.Failure.invalid("Missing removal journal entries") }
                return next
            }
            var after: String?
            while true {
                try check()
                let (page, live) = try journal.withFence(cancellation: cancellation) { fence -> ([RemovalJournal.Intersection], Bool) in
                    guard fence.identity == end.identity, fence.highWater >= end.sequence else { throw Retry.journalChanged }
                    let page = try fence.intersections(sequence: entry.sequence, root: crawler.rootURL, afterPath: after, limit: 512)
                    guard let current = try fence.operations(after: entry.sequence - 1, limit: 1).first, current.sequence == entry.sequence else { throw RemovalJournal.Failure.invalid("Missing removal operation") }
                    let live = try !page.isEmpty && !current.completed && fence.isLive(sequence: entry.sequence)
                    return (page, live)
                }
                if live, pendingLive == nil { pendingLive = entry.sequence }
                var parents: [Data] = []
                func process(_ relation: RemovalJournal.Relation, _ removal: ScanRemoval) {
                    switch relation {
                    case .inside:
                        let bytes = Data(removal.path.utf8)
                        if let slash = bytes.lastIndex(of: 47) {
                            parents.append(slash == bytes.startIndex ? Data([47]) : Data(bytes.prefix(upTo: slash)))
                        } else { full = true }
                        if !overflow {
                            if kept.count < overlayLimit { kept.append(removal) }
                            else { kept.removeAll(keepingCapacity: false); overflow = true }
                        }
                    case .coversRoot, .unresolved: full = true
                    }
                }
                for match in page {
                    process(match.relation, match.removal)
                    if let original = match.originalRelation {
                        process(original, ScanRemoval(path: match.originalPath, identity: match.removal.identity))
                    }
                }
                try crawler.store.addReplayParents(parents)
                guard page.count == 512, let last = page.last else { break }
                after = last.originalPath
            }
            sequence = entry.sequence
        }
        try crawler.root.validate(url: crawler.rootURL)
        if pendingLive != nil {
            // The owner may still change the filesystem; defer dirtying until it finishes.
        } else if full { try crawler.store.markFullRefresh() }
        else {
            var after: Data?
            while true {
                try check()
                let page = try crawler.store.replayParents(after: after, limit: 128)
                for path in page {
                    guard let path = String(data: path, encoding: .utf8) else { throw RemovalJournal.Failure.invalid("invalid replay parent") }
                    try crawler.markChanged(directory: path)
                }
                guard page.count == 128, let last = page.last else { break }
                after = last
            }
        }
        fullReconciliation = full
        liveBarrier = pendingLive
        nextProbe = ProcessInfo.processInfo.systemUptime + 1
        target = end
        needsSettle = true
        stateLock.lock(); overlay = kept; blocksPreview = full || overflow; stateGeneration &+= 1; stateLock.unlock()
    }
    // Once affected parents are reconciled, release the temporary overlay even if
    // other unrelated directories remain queued for the initial scan.
    func settleIfPossible() throws -> Bool {
        guard let target else { throw Retry.journalChanged }
        if liveBarrier != nil { return false }
        if !needsSettle { return true }
        if try crawler.store.hasReplayDirtyWork() { return false }
        if try fullReconciliation && crawler.store.hasPendingDirectories() { return false }
        let stable = try journal.withFence(cancellation: cancellation) { $0.identity == target.identity && $0.highWater == target.sequence }
        guard stable else { try prepare(); return false }
        try crawler.store.writeRemovalCursor(target)
        needsSettle = false
        stateLock.lock(); covered = target; overlay.removeAll(keepingCapacity: false); blocksPreview = false; stateGeneration &+= 1; stateLock.unlock()
        return true
    }
    // Invoked off the main actor between committed batches.
    func refreshIfNeeded() throws {
        let now = ProcessInfo.processInfo.systemUptime
        guard now >= nextProbe, let target else { return }
        nextProbe = now + 1
        let changed = try journal.withFence(cancellation: cancellation) { fence -> Bool in
            if fence.identity != target.identity || fence.highWater != target.sequence { return true }
            guard let liveBarrier else { return false }
            guard let current = try fence.operations(after: liveBarrier - 1, limit: 1).first,
                  current.sequence == liveBarrier else { throw RemovalJournal.Failure.invalid("Missing live removal") }
            return try current.completed || !fence.isLive(sequence: liveBarrier)
        }
        if changed { try prepare() }
    }
    func fencePublication(_ action: () throws -> Void) throws {
        guard let covered else { throw Retry.journalChanged }
        try journal.withFence(cancellation: cancellation) { fence in
            guard fence.identity == covered.identity && fence.highWater == covered.sequence else { throw Retry.journalChanged }
            try fence.validate()
            try action()
        }
    }
}
