import CSQLite
import Foundation

enum FreshFileInsertChecks {
    static func run(_ base: URL) throws {
        let root = base.appendingPathComponent("root", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let crawler = try DurableCrawler(rootURL: root, database: base.appendingPathComponent("stage.sqlite").path,
            checkpoint: StageCheckpoint(rootPath: root.path, identity: "fresh-file-insert", eventID: 0), mirror: true)
        let store = crawler.store, parent = crawler.rootPath, device = crawler.root.identity.device
        func file(_ name: String, _ inode: UInt64, _ size: Int64, allocation: Int64 = 4_096) -> FileMetadata {
            FileMetadata(name: name, device: device, inode: inode, isDirectory: false,
                isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: size, allocatedSize: allocation)
        }
        func query(_ sql: String) -> Int64 {
            var db: OpaquePointer?, statement: OpaquePointer?
            precondition(sqlite3_open_v2(store.databasePath, &db, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK)
            defer { sqlite3_finalize(statement); sqlite3_close(db) }
            precondition(sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK)
            let status = sqlite3_step(statement)
            precondition(status == SQLITE_ROW || status == SQLITE_DONE)
            return status == SQLITE_ROW ? sqlite3_column_int64(statement, 0) : 0
        }
        func check(_ count: Int64, _ logical: Int64, _ allocated: Int64) throws {
            while try store.reconcileLinksChunk() {}
            let totals = try store.folderTotals(path: parent)
            precondition(totals?.count == count && totals?.logical == logical && totals?.allocated == allocated)
            precondition(query("SELECT COUNT(*) FROM nodes WHERE is_dir=0") == count)
        }
        var inserts = 0, upserts = 0
        ListingStage.testPreparedStatement = { sql in
            guard sql.contains("INSERT INTO nodes(id,parent,name,search_name") else { return }
            if sql.contains("ON CONFLICT") { upserts += 1 } else { inserts += 1 }
        }
        defer { ListingStage.testPreparedStatement = nil }
        let fresh = try store.completeFreshListing(parent: parent, records: [
            file("keep", 10, 7), file("repeat", 11, 10), file("repeat", 11, 20),
            file("replace", 12, 4), file("replace", 13, 8, allocation: 8_192)
        ], rootDevice: device, expectedRevision: 0, expectedFullGeneration: 0)
        ListingStage.testPreparedStatement = nil
        precondition(fresh && inserts == 1 && upserts == 1)
        try check(3, 35, 16_384)
        let repeatedID = query("SELECT id FROM nodes WHERE name='repeat'")
        let epoch = try store.begin(parent: parent)
        try store.append(parent: parent, epoch: epoch, records: [file("repeat", 11, 21)], rootDevice: device)
        precondition(query("SELECT id FROM nodes WHERE name='repeat'") == repeatedID)
        try check(3, 36, 16_384)

        let directory = FileMetadata(name: "replace", device: device, inode: 500, isDirectory: true,
            isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0)
        try store.append(parent: parent, epoch: epoch, records: [directory], rootDevice: device)
        var child = parent; child.append(contentsOf: "/replace".utf8)
        let childDone = try store.completeFreshListing(parent: child, records: [file("child", 501, 5)],
            rootDevice: device, expectedRevision: 0, expectedFullGeneration: 0)
        precondition(childDone)
        try check(3, 33, 12_288)
        do {
            try store.append(parent: parent, epoch: epoch, records: [file("replace", 600, 9)], rootDevice: device)
            preconditionFailure("A directory replacement bypassed subtree pruning")
        } catch ListingStage.Reconcile.pruneRequired {}
        while try store.pruneOneChunk() {}
        try store.append(parent: parent, epoch: epoch, records: [file("replace", 600, 9)], rootDevice: device)
        try check(3, 37, 12_288)
        precondition(query("SELECT COUNT(*) FROM directory_paths WHERE path LIKE '%/replace%'") == 0)

        _ = query("CREATE TRIGGER fail_fresh_file BEFORE INSERT ON nodes WHEN NEW.name='fault' BEGIN SELECT RAISE(ABORT,'fresh insert rollback'); END")
        do {
            try store.append(parent: parent, epoch: epoch, records: [file("first", 700, 1), file("fault", 701, 2)], rootDevice: device)
            preconditionFailure("Failed compact insert did not abort the batch")
        } catch ListingStage.Failure.sqlite {}
        _ = query("DROP TRIGGER fail_fresh_file")
        try check(3, 37, 12_288)
        precondition(query("SELECT COUNT(*) FROM nodes WHERE name IN ('first','fault')") == 0)
        precondition(query("SELECT COUNT(*) FROM owners WHERE inode IN (700,701)") == 0)
        print("PASS: compact fresh-file inserts preserve duplicate names, replacement identities, directory transitions and rollback")
    }
}
