import CSQLite
import Darwin
import Foundation

enum NoStaleTrimChecks {
    final class Fixture {
        let crawler: DurableCrawler
        var store: ListingStage { crawler.store }
        var parent: Data { crawler.rootPath }
        var records: [FileMetadata] {
            [file("a", 10, 7), file("b", 11, 9),
             FileMetadata(name: "directory", device: crawler.root.identity.device, inode: 20,
                isDirectory: true, isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0)]
        }
        init(_ base: URL) throws {
            let root = base.appendingPathComponent("root", isDirectory: true)
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            crawler = try DurableCrawler(rootURL: root, database: base.appendingPathComponent("stage.sqlite").path,
                checkpoint: StageCheckpoint(rootPath: root.path, identity: "no-stale-trim", eventID: 0), mirror: true)
            let completed = try store.completeFreshListing(parent: parent, records: records,
                rootDevice: crawler.root.identity.device, expectedRevision: 0, expectedFullGeneration: 0)
            var child = parent; child.append(contentsOf: "/directory".utf8)
            let childDone = try store.completeFreshListing(parent: child, records: [file("nested", 21, 5)],
                rootDevice: crawler.root.identity.device, expectedRevision: 0, expectedFullGeneration: 0)
            precondition(completed && childDone)
        }
        func file(_ name: String, _ inode: UInt64, _ size: Int64) -> FileMetadata {
            FileMetadata(name: name, device: crawler.root.identity.device, inode: inode, isDirectory: false,
                isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: size, allocatedSize: 4_096)
        }
        @discardableResult func query(_ sql: String) -> Int64 {
            Self.query(store.databasePath, sql)
        }
        static func query(_ path: String, _ sql: String) -> Int64 {
            var db: OpaquePointer?, statement: OpaquePointer?
            precondition(sqlite3_open_v2(path, &db, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK)
            defer { sqlite3_finalize(statement); sqlite3_close(db) }
            precondition(sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK)
            let status = sqlite3_step(statement)
            precondition(status == SQLITE_ROW || status == SQLITE_DONE)
            return status == SQLITE_ROW ? sqlite3_column_int64(statement, 0) : 0
        }
        func relist(_ records: [FileMetadata]? = nil) throws {
            try store.markDirty(parent: parent)
            let epoch = try store.begin(parent: parent)
            precondition(epoch > 1)
            try store.append(parent: parent, epoch: epoch, records: records ?? self.records, rootDevice: crawler.root.identity.device)
            let done = try store.finish(parent: parent, epoch: epoch, expectedRevision: store.dirtyRevision(parent: parent),
                expectedFullGeneration: store.fullRefreshState().generation)
            precondition(done && query("SELECT COUNT(*) FROM trim") == 1)
        }
        func check(_ count: Int64, _ logical: Int64) throws {
            while try store.reconcileLinksChunk() {}
            let totals = try store.folderTotals(path: parent)
            precondition(totals?.count == count && totals?.logical == logical && totals?.allocated == count * 4_096)
        }
    }

    static func crashChild(_ base: URL, boundary: String) throws {
        let fixture = try Fixture(base)
        try fixture.relist()
        ListingStage.testListingCommit = { if $0 == boundary { kill(getpid(), SIGKILL) } }
        _ = try fixture.store.trimOneChunk()
        preconditionFailure("Missed no-stale trim crash boundary")
    }

    static func run(_ base: URL) throws {
        defer { ListingStage.testPreparedStatement = nil; ListingStage.testListingCommit = nil }
        let empty = try Fixture(base.appendingPathComponent("no-stale"))
        try empty.relist()
        var removalQueries = 0
        ListingStage.testPreparedStatement = { sql in
            if sql.hasPrefix("INSERT OR IGNORE INTO dirty_links SELECT") || sql.hasPrefix("DELETE FROM nodes WHERE id IN") {
                removalQueries += 1
            }
        }
        let acknowledged = try empty.store.trimOneChunk()
        ListingStage.testPreparedStatement = nil
        precondition(acknowledged && removalQueries == 0 && empty.query("SELECT COUNT(*) FROM trim") == 0)
        precondition(empty.query("SELECT COUNT(*) FROM directories WHERE pending=1") == 0)
        precondition(empty.query("SELECT COUNT(*) FROM dirty WHERE pending=1") == 0)
        try empty.check(3, 21)

        for kind in ["file", "directory"] {
            let fixture = try Fixture(base.appendingPathComponent("stale-" + kind))
            try fixture.relist(fixture.records.filter { $0.name != (kind == "file" ? "b" : "directory") })
            while try fixture.store.trimOneChunk() {
                while try fixture.store.pruneOneChunk() {}
            }
            try fixture.check(2, kind == "file" ? 12 : 16)
            precondition(fixture.query("SELECT COUNT(*) FROM trim") == 0)
        }
        for kind in ["revision", "generation"] {
            let fixture = try Fixture(base.appendingPathComponent(kind))
            try fixture.relist([])
            if kind == "revision" { try fixture.store.markDirty(parent: fixture.parent) }
            else { try fixture.store.markFullRefresh() }
            _ = try fixture.store.trimOneChunk()
            try fixture.check(3, 21)
            precondition(fixture.query("SELECT COUNT(*) FROM trim") == 0)
            precondition(fixture.query("SELECT COUNT(*) FROM directories WHERE pending=1") > 0)
        }
        let rollback = try Fixture(base.appendingPathComponent("rollback"))
        try rollback.relist()
        rollback.query("CREATE TRIGGER fail_trim_ack BEFORE DELETE ON trim BEGIN SELECT RAISE(ABORT,'trim rollback'); END")
        do { _ = try rollback.store.trimOneChunk(); preconditionFailure("Trim acknowledgement did not roll back") }
        catch ListingStage.Failure.sqlite {}
        precondition(rollback.query("SELECT COUNT(*) FROM trim") == 1)
        precondition(rollback.query("SELECT COUNT(*) FROM dirty WHERE pending=1") == 1)
        precondition(rollback.query("SELECT COUNT(*) FROM directories WHERE pending=1") == 1)
        rollback.query("DROP TRIGGER fail_trim_ack")
        _ = try rollback.store.trimOneChunk()
        try rollback.check(3, 21)

        let cancelled = try Fixture(base.appendingPathComponent("cancelled"))
        try cancelled.relist(); cancelled.crawler.cancellation.cancel()
        do { _ = try cancelled.crawler.step(); preconditionFailure("Cancelled writer consumed trim work") }
        catch is CancellationError {}
        precondition(cancelled.query("SELECT COUNT(*) FROM trim") == 1)

        for boundary in ["trim-empty-written", "trim-empty-committed"] {
            let path = base.appendingPathComponent(boundary)
            let child = Process()
            child.executableURL = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
            child.arguments = ["no-stale-trim-child", path.path, boundary]
            try child.run(); child.waitUntilExit()
            precondition(child.terminationReason == .uncaughtSignal && child.terminationStatus == SIGKILL)
            let db = path.appendingPathComponent("stage.sqlite").path
            let pending: Int64 = boundary == "trim-empty-written" ? 1 : 0
            precondition(Fixture.query(db, "SELECT COUNT(*) FROM trim") == pending)
            precondition(Fixture.query(db, "SELECT COUNT(*) FROM dirty WHERE pending=1") == pending)
            precondition(Fixture.query(db, "SELECT COUNT(*) FROM directories WHERE pending=1") == pending)
            precondition(Fixture.query(db, "SELECT COUNT(*) FROM nodes WHERE is_dir=0") == 3)
        }
        print("PASS: no-stale trim skips removal scans after any epoch while preserving stale work, generation guards, rollback and crash recovery")
    }
}
