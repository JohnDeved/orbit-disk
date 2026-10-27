import Darwin
import Foundation

public enum RemovalRecoveryError: Error {
    case changedDuringReconciliation
    case journalChanged
}

public struct RemovalCursor: Sendable, Equatable {
    public let identity: String
    public let sequence: Int64
    public init(identity: String, sequence: Int64) { self.identity = identity; self.sequence = sequence }
}

public struct RemovalRecovery: Sendable {
    public let cursor: RemovalCursor
    public let removals: [ScanRemoval]
    public let affectedParents: Set<String>
    public let overlayOverflow: Bool
    public let hasLiveOperation: Bool
    public let requiresFullRefresh: Bool

    public init(cursor: RemovalCursor, removals: [ScanRemoval], affectedParents: Set<String>, overlayOverflow: Bool,
                hasLiveOperation: Bool, requiresFullRefresh: Bool) {
        self.cursor = cursor; self.removals = removals; self.affectedParents = affectedParents
        self.overlayOverflow = overlayOverflow; self.hasLiveOperation = hasLiveOperation
        self.requiresFullRefresh = requiresFullRefresh
    }
}

extension RemovalJournal {
    // Entries are immutable after insertion except for completion; process each page under a
    // separate fence so long histories do not prevent another process from starting a removal.
    public func recover(root: URL, cursor: RemovalCursor?, cancellation: ScanCancellation) throws -> RemovalRecovery {
        let (captured, expectedRoot) = try withFence(cancellation: cancellation) { fence -> (RemovalCursor, Data?) in
            if let cursor, cursor.identity != fence.identity || cursor.sequence < fence.floor || cursor.sequence > fence.highWater {
                throw RemovalRecoveryError.journalChanged
            }
            let end = RemovalCursor(identity: fence.identity, sequence: fence.highWater)
            let rootIdentity = (cursor?.sequence ?? 0) == end.sequence ? nil : fence.recoveryRootIdentity(root)
            return (end, rootIdentity)
        }
        if (cursor?.sequence ?? 0) == captured.sequence {
            return RemovalRecovery(cursor: captured, removals: [], affectedParents: [], overlayOverflow: false,
                                   hasLiveOperation: false, requiresFullRefresh: false)
        }
        var sequence = cursor?.sequence ?? 0
        var result: [ScanRemoval] = []
        var parents = Set<String>()
        var parentBytes: [String: Data] = [:]
        var overflow = false, live = false, full = false
        func accept(_ item: ScanRemoval) {
            if result.count < 10_000 { result.append(item) } else { overflow = true }
            let bytes = Array(item.path.utf8)
            guard let slash = bytes.lastIndex(of: 47) else { full = true; return }
            let parent = slash == 0 ? "/" : String(decoding: bytes[..<slash], as: UTF8.self)
            let raw = Data(parent.utf8)
            if let existing = parentBytes[parent], existing != raw { full = true; return }
            if parents.count < 10_000 {
                parentBytes[parent] = raw
                parents.insert(parent)
            } else if !parents.contains(parent) {
                full = true
            }
        }
        while sequence < captured.sequence {
            let (nextSequence, operation) = try withFence(cancellation: cancellation) { fence -> (Int64, RemovalJournal.Entry?) in
                guard fence.identity == captured.identity, fence.highWater >= captured.sequence else { throw RemovalRecoveryError.journalChanged }
                let operations = try fence.operations(after: sequence, limit: 128).filter { $0.sequence <= captured.sequence }
                guard !operations.isEmpty else { throw RemovalJournal.Failure.invalid("Missing removal journal entries") }
                var scanned = sequence
                guard fence.recoveryRootIdentity(root) == expectedRoot else { throw RemovalRecoveryError.journalChanged }
                // The budget applies between probes; one filesystem lookup or SQLite step can take longer.
                let deadline = ProcessInfo.processInfo.systemUptime + 0.005
                for operation in operations {
                    let relevant = try !fence.intersections(sequence: operation.sequence, root: root, limit: 1).isEmpty
                    #if ORBIT_TESTING
                    RemovalJournal.testAfterRecoveryProbe?()
                    #endif
                    guard fence.recoveryRootIdentity(root) == expectedRoot else { throw RemovalRecoveryError.journalChanged }
                    if relevant { return (scanned, operation) }
                    scanned = operation.sequence
                    if expectedRoot == nil || ProcessInfo.processInfo.systemUptime >= deadline { break }
                }
                return (scanned, nil)
            }
            sequence = nextSequence
            if operation == nil && sequence < captured.sequence { usleep(2_000) }
            if let operation {
                var after: String?
                var relevant = false
                while true {
                    let page = try withFence(cancellation: cancellation) { fence in
                        guard fence.identity == captured.identity, fence.highWater >= captured.sequence,
                              fence.recoveryRootIdentity(root) == expectedRoot else { throw RemovalRecoveryError.journalChanged }
                        return try fence.intersections(sequence: operation.sequence, root: root, afterPath: after, limit: 256)
                    }
                    for item in page {
                        switch item.relation {
                        case .inside: accept(item.removal); relevant = true
                        case .coversRoot, .unresolved: full = true; relevant = true
                        }
                        if let original = item.originalRelation {
                            switch original {
                            case .inside: accept(ScanRemoval(path: item.originalPath, identity: item.removal.identity)); relevant = true
                            case .coversRoot, .unresolved: full = true; relevant = true
                            }
                        }
                    }
                    guard page.count == 256, let last = page.last else { break }
                    after = last.originalPath
                }
                if relevant && !operation.completed {
                    live = try withFence(cancellation: cancellation) { fence in
                        guard fence.identity == captured.identity, fence.highWater >= captured.sequence else { throw RemovalRecoveryError.journalChanged }
                        return try fence.isLive(sequence: operation.sequence)
                    } || live
                }
                sequence = operation.sequence
            }
        }
        try withFence(cancellation: cancellation) { fence in
            guard fence.identity == captured.identity, fence.highWater >= captured.sequence,
                  fence.recoveryRootIdentity(root) == expectedRoot else { throw RemovalRecoveryError.journalChanged }
        }
        return RemovalRecovery(cursor: captured, removals: result, affectedParents: parents, overlayOverflow: overflow,
                               hasLiveOperation: live, requiresFullRefresh: full)
    }
}
