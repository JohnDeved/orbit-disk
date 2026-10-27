@testable import OrbitCore
import CSQLite
import Darwin
import Foundation

private struct TestFailure: Error, CustomStringConvertible {
    let description: String
}

private func require(_ condition: Bool, _ message: String) throws {
    guard condition else { throw TestFailure(description: message) }
}

private func fileIdentity(_ url: URL) throws -> String {
    var info = stat()
    guard lstat(url.path, &info) == 0 else {
        throw TestFailure(description: "Cannot read identity of \(url.path): errno \(errno)")
    }
    return "\(info.st_dev):\(info.st_ino)"
}

private func withDatabase(at url: URL, _ body: (OpaquePointer) throws -> Void) throws {
    var database: OpaquePointer?
    let status = sqlite3_open_v2(url.path, &database, SQLITE_OPEN_READWRITE, nil)
    guard status == SQLITE_OK, let database else {
        let message = database.map { String(cString: sqlite3_errmsg($0)) } ?? "no database handle"
        if let database { sqlite3_close(database) }
        throw TestFailure(description: "Cannot open journal: \(message)")
    }
    defer { sqlite3_close(database) }
    try body(database)
}

private func execute(_ sql: String, on database: OpaquePointer) throws {
    guard sqlite3_exec(database, sql, nil, nil, nil) == SQLITE_OK else {
        throw TestFailure(description: "Journal fixture SQL failed: \(String(cString: sqlite3_errmsg(database)))")
    }
}

private func transaction(on database: OpaquePointer, _ body: () throws -> Void) throws {
    try execute("BEGIN IMMEDIATE", on: database)
    do {
        try body()
        try execute("COMMIT", on: database)
    } catch {
        try? execute("ROLLBACK", on: database)
        throw error
    }
}

private struct Fixture {
    let root: URL
    let cache: URL
    let victim: URL
    let journalFile: URL

    init(_ base: URL, _ name: String, unrelatedCount: Int = 256) throws {
        let directory = base.appendingPathComponent(name)
        root = directory.appendingPathComponent("holder/root")
        cache = directory.appendingPathComponent("cache")
        victim = root.appendingPathComponent("victim")
        journalFile = directory.appendingPathComponent("cache.staging/.orbit-removals/journal.sqlite")
        try FileManager.default.createDirectory(at: root.appendingPathComponent("pending"), withIntermediateDirectories: true)
        try Data([1]).write(to: victim)
        try Data([2]).write(to: root.appendingPathComponent("pending/leaf"))
        do {
            let stage = try open()
            _ = try stage.advance(progress: { _ in })
        }

        // Seed completed, unrelated history after the stage has acknowledged cursor zero.
        try withDatabase(at: journalFile) { database in
            try transaction(on: database) {
                for offset in 0..<unrelatedCount {
                    let sequence = offset + 1
                    let lockName = String(format: "%08x-0000-0000-0000-000000000000.operation", sequence)
                    try execute("""
                        INSERT INTO operations(seq, root, root_identity, lock_name, complete)
                        VALUES(\(sequence), CAST('/other' AS BLOB), '1:1', '\(lockName)', 1)
                        """, on: database)
                    try execute("""
                        INSERT INTO items(op, path, identity, canonical, parent_identity)
                        VALUES(\(sequence), CAST('/other/item\(sequence)' AS BLOB), '1:1',
                               CAST('/other/item\(sequence)' AS BLOB), '1:1')
                        """, on: database)
                }
            }
        }
    }

    func open(cancellation: ScanCancellation = ScanCancellation()) throws -> ResumableStage {
        try ResumableStage(cache: cache, root: root,
                           current: StageCheckpoint(rootPath: root.path, identity: "replay-batch-check", eventID: 1),
                           cancellation: cancellation)
    }

    func beginRemoval(in journal: RemovalJournal) throws -> RemovalJournal.Operation {
        try journal.begin(root: root, rootIdentity: fileIdentity(root),
                          items: [ScanRemoval(path: victim.path, identity: fileIdentity(victim))])
    }
}

private final class OnceHook: @unchecked Sendable {
    private let lock = NSLock()
    private var didRun = false
    private var failure: Error?

    func run(_ body: () throws -> Void) {
        lock.lock()
        guard !didRun else { lock.unlock(); return }
        didRun = true
        lock.unlock()
        do { try body() }
        catch {
            lock.lock()
            failure = error
            lock.unlock()
        }
    }

    func verifyRan() throws {
        lock.lock()
        defer { lock.unlock() }
        if let failure { throw failure }
        try require(didRun, "The intended replay boundary was never reached")
    }
}

@main struct Check {
    static func requireCursor(_ stage: ResumableStage, equals expected: Int64) throws {
        let persisted = try stage.crawler.store.removalCursor()?.sequence
        try require(persisted == expected, "Expected durable cursor \(expected), got \(String(describing: persisted))")
    }

    static func settle(_ stage: ResumableStage, at target: Int64) throws {
        for _ in 0..<30 {
            _ = try stage.advance(progress: { _ in })
            if stage.removalReplayReadiness().cursor?.sequence == target {
                try requireCursor(stage, equals: target)
                return
            }
        }
        throw TestFailure(description: "Replay did not settle at cursor \(target) after 30 advances")
    }

    private static func requireJournalChange(_ stage: ResumableStage, hook: OnceHook) throws {
        var observed: Error?
        do { try stage.prepareRemovalReplay() }
        catch { observed = error }
        try hook.verifyRan()
        guard let observed else { throw TestFailure(description: "Replay accepted a changed root binding") }
        guard let retry = observed as? StageRemovalReplay.Retry, case .journalChanged = retry else { throw observed }
        try requireCursor(stage, equals: 0)
    }

    static func appendedOperation(_ base: URL) throws {
        let fixture = try Fixture(base, "append")
        let stage = try fixture.open()
        let journal = try RemovalJournal(cache: fixture.cache)
        let hook = OnceHook()
        StageRemovalReplay.testAfterReplayHead = { _ in
            hook.run {
                let operation = try fixture.beginRemoval(in: journal)
                try operation.complete()
            }
        }
        defer { StageRemovalReplay.testAfterReplayHead = nil }
        try stage.prepareRemovalReplay()
        StageRemovalReplay.testAfterReplayHead = nil
        try hook.verifyRan()
        try requireCursor(stage, equals: 0)
        let view = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())
        try require(view?.folder.children.count == 2, "Preparation applied an operation beyond its captured highwater")
        try settle(stage, at: 257)
    }

    static func completionAfterHeader(_ base: URL) throws {
        let fixture = try Fixture(base, "complete")
        let stage = try fixture.open()
        let journal = try RemovalJournal(cache: fixture.cache)
        let operation = try fixture.beginRemoval(in: journal)
        let hook = OnceHook()
        StageRemovalReplay.testAfterReplayHead = { sequence in
            guard sequence == operation.sequence else { return }
            hook.run { try operation.complete() }
        }
        defer { StageRemovalReplay.testAfterReplayHead = nil }
        try stage.prepareRemovalReplay()
        StageRemovalReplay.testAfterReplayHead = nil
        try hook.verifyRan()
        try withExtendedLifetime(operation) { try settle(stage, at: 257) }
    }

    static func laterCachedCompletion(_ base: URL) throws {
        let fixture = try Fixture(base, "cached-complete", unrelatedCount: 1)
        let stage = try fixture.open()
        let journal = try RemovalJournal(cache: fixture.cache)
        let first = try fixture.beginRemoval(in: journal)
        try first.complete()
        let second = try fixture.beginRemoval(in: journal)
        let hook = OnceHook()
        StageRemovalReplay.testAfterReplayHead = { sequence in
            guard sequence == first.sequence else { return }
            hook.run { try second.complete() }
        }
        defer { StageRemovalReplay.testAfterReplayHead = nil }
        try stage.prepareRemovalReplay()
        StageRemovalReplay.testAfterReplayHead = nil
        try hook.verifyRan()
        try withExtendedLifetime(second) { try settle(stage, at: 3) }
    }

    static func liveOperation(_ base: URL) throws {
        let fixture = try Fixture(base, "live")
        let stage = try fixture.open()
        let journal = try RemovalJournal(cache: fixture.cache)
        let operation = try fixture.beginRemoval(in: journal)
        try stage.prepareRemovalReplay()
        _ = try stage.advance(progress: { _ in })
        try requireCursor(stage, equals: 0)
        try require(!stage.removalReplayReadiness().blocked, "A live intent hid the unrelated staged map")
        try operation.complete()
        try stage.prepareRemovalReplay()
        try settle(stage, at: 257)
    }

    static func cancellation(_ base: URL) throws {
        let fixture = try Fixture(base, "cancel")
        let cancellation = ScanCancellation()
        let stage = try fixture.open(cancellation: cancellation)
        let hook = OnceHook()
        RemovalJournal.testAfterRecoveryProbe = { hook.run { cancellation.cancel() } }
        defer { RemovalJournal.testAfterRecoveryProbe = nil }
        do {
            try stage.prepareRemovalReplay()
            throw TestFailure(description: "Replay ignored cancellation during a header probe")
        } catch is CancellationError {}
        RemovalJournal.testAfterRecoveryProbe = nil
        try hook.verifyRan()
        try requireCursor(stage, equals: 0)
    }

    static func replacedRoot(_ base: URL) throws {
        let fixture = try Fixture(base, "replace")
        let stage = try fixture.open()
        let hook = OnceHook()
        let moved = fixture.root.deletingLastPathComponent().appendingPathComponent("moved")
        RemovalJournal.testAfterRecoveryProbe = {
            hook.run {
                try FileManager.default.moveItem(at: fixture.root, to: moved)
                try FileManager.default.createDirectory(at: fixture.root, withIntermediateDirectories: false)
            }
        }
        defer { RemovalJournal.testAfterRecoveryProbe = nil }
        try requireJournalChange(stage, hook: hook)
    }

    static func changedCanonicalRoot(_ base: URL, beforeRelevantPage: Bool) throws {
        let fixture = try Fixture(base, beforeRelevantPage ? "alias-page" : "alias-final", unrelatedCount: 1)
        let stage = try fixture.open()
        if beforeRelevantPage {
            let journal = try RemovalJournal(cache: fixture.cache)
            let operation = try fixture.beginRemoval(in: journal)
            try operation.complete()
        }
        let parent = fixture.root.deletingLastPathComponent()
        let moved = parent.deletingLastPathComponent().appendingPathComponent("moved-holder")
        let hook = OnceHook()
        StageRemovalReplay.testAfterReplayHead = { sequence in
            guard (beforeRelevantPage ? sequence == 2 : sequence == nil) else { return }
            hook.run {
                try FileManager.default.moveItem(at: parent, to: moved)
                try FileManager.default.createSymbolicLink(at: parent, withDestinationURL: moved)
            }
        }
        defer { StageRemovalReplay.testAfterReplayHead = nil }
        try requireJournalChange(stage, hook: hook)
        try stage.crawler.root.validate(url: fixture.root)
    }

    static func missingCapturedTail(_ base: URL) throws {
        let fixture = try Fixture(base, "missing-tail")
        let stage = try fixture.open()
        let hook = OnceHook()
        StageRemovalReplay.testAfterReplayHead = { _ in
            hook.run {
                try withDatabase(at: fixture.journalFile) { database in
                    try transaction(on: database) {
                        try execute("DELETE FROM items WHERE op > 128", on: database)
                        try execute("DELETE FROM operations WHERE seq > 128", on: database)
                        try execute("""
                            INSERT INTO operations(seq, root, root_identity, lock_name, complete)
                            VALUES(257, CAST('/other' AS BLOB), '1:1',
                                   '00000101-0000-0000-0000-000000000000.operation', 1)
                            """, on: database)
                    }
                }
            }
        }
        defer { StageRemovalReplay.testAfterReplayHead = nil }
        var observed: Error?
        do { try stage.prepareRemovalReplay() }
        catch { observed = error }
        try hook.verifyRan()
        guard let observed else { throw TestFailure(description: "Replay accepted a missing captured tail") }
        guard let failure = observed as? RemovalJournal.Failure, case .invalid(let reason) = failure else { throw observed }
        try require(reason == "Missing removal journal entries", "Unexpected missing-tail error: \(reason)")
        try requireCursor(stage, equals: 0)
    }

    static func emptyReplayRange(_ base: URL) throws {
        let fixture = try Fixture(base, "idle", unrelatedCount: 0)
        let stage = try fixture.open()
        let journal = try RemovalJournal(cache: fixture.cache)
        RemovalJournal.testAfterRecoveryProbe = { preconditionFailure("Empty replay range probed an operation") }
        defer { RemovalJournal.testAfterRecoveryProbe = nil }
        try stage.prepareRemovalReplay()
        try requireCursor(stage, equals: 0)
        try require(!stage.removalReplayReadiness().blocked, "Empty history blocked the staged map")
        let initialView = try stage.view(path: nil, removals: [], cancellation: ScanCancellation())
        try require(initialView?.folder.children.count == 2, "Empty replay range lost committed root children")

        RemovalJournal.testAfterRecoveryProbe = nil
        let operation = try fixture.beginRemoval(in: journal)
        try operation.complete()
        try requireCursor(stage, equals: 0)
        try settle(stage, at: 1)

        RemovalJournal.testAfterRecoveryProbe = { preconditionFailure("Acknowledged history was probed again") }
        try stage.prepareRemovalReplay()
        try requireCursor(stage, equals: 1)
        try require(!stage.removalReplayReadiness().blocked, "Acknowledged history blocked the staged map")
    }

    static func main() throws {
        guard CommandLine.arguments.count == 2 else { throw TestFailure(description: "Expected a fixture directory") }
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).resolvingSymlinksInPath()
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        let deadline = DispatchWorkItem {
            fputs("FAIL: replay batch checks stalled; see the last CHECK line\n", stderr)
            exit(EXIT_FAILURE)
        }
        DispatchQueue(label: "OrbitStageRemovalChecks.ReplayBatch.timeout").asyncAfter(deadline: .now() + 60, execute: deadline)
        defer { deadline.cancel() }
        let checks: [(String, (URL) throws -> Void)] = [
            ("append beyond captured highwater", appendedOperation),
            ("completion between header and item page", completionAfterHeader),
            ("completion of a later cached header", laterCachedCompletion),
            ("live operation delays cursor but preserves browsing", liveOperation),
            ("cancellation preserves the durable cursor", cancellation),
            ("root replacement between probes", replacedRoot),
            ("canonical root change after the last unrelated probe", { try changedCanonicalRoot($0, beforeRelevantPage: false) }),
            ("canonical root change before a relevant page", { try changedCanonicalRoot($0, beforeRelevantPage: true) }),
            ("missing captured tail rejects newer-only headers", missingCapturedTail),
            ("empty and acknowledged replay ranges preserve browsing and detect a late append", emptyReplayRange)
        ]
        for (name, check) in checks {
            fputs("CHECK: \(name)\n", stderr)
            try check(base)
            print("PASS: \(name)")
        }
    }
}
