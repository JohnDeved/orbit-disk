import CSQLite
import Darwin
import Foundation

enum FreshLeafGroupChecks {
    final class Fixture {
        let crawler: DurableCrawler
        let count: Int
        var store: ListingStage { crawler.store }
        var device: Int32 { crawler.root.identity.device }
        init(_ base: URL, count: Int = 4) throws {
            self.count = count
            let root = base.appendingPathComponent("root", isDirectory: true)
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            crawler = try DurableCrawler(rootURL: root, database: base.appendingPathComponent("stage.sqlite").path,
                checkpoint: StageCheckpoint(rootPath: root.path, identity: "leaf-groups", eventID: 0), mirror: true)
            var directories: [FileMetadata] = []
            for index in 0..<count {
                let url = root.appendingPathComponent("d-\(index)", isDirectory: true)
                try FileManager.default.createDirectory(at: url, withIntermediateDirectories: false)
                directories.append(try FileMetadata.read(url))
            }
            let completed = try store.completeFreshListing(parent: crawler.rootPath, records: directories,
                rootDevice: device, expectedRevision: 0, expectedFullGeneration: 0)
            precondition(completed)
        }
        func path(_ index: Int) -> Data { Data(crawler.rootURL.appendingPathComponent("d-\(index)").path.utf8) }
        func records(_ index: Int, shared: Bool = false) -> [FileMetadata] {
            [FileMetadata(name: "file", device: device, inode: shared ? 100_000 : UInt64(100_000 + index),
                isDirectory: false, isSymbolicLink: false, isMountPoint: false, linkCount: shared ? 2 : 1,
                size: 7, allocatedSize: 4_096)]
        }
        func input(_ index: Int, records: [FileMetadata]? = nil, revision: Int64 = 0,
                   generation: Int64 = 0, sequence: Int64? = nil, replacedIdentity: Bool = false) throws -> ListingStage.FreshLeafListing {
            let actual = try store.directory(path: path(index))!
            let directory = ListingStage.QueuedDirectory(path: actual.path, device: actual.device,
                inode: actual.inode + (replacedIdentity ? 1 : 0))
            return ListingStage.FreshLeafListing(directory: directory, records: records ?? self.records(index),
                expectedRevision: revision, expectedFullGeneration: generation,
                asyncCommit: AsyncInventoryCommit(generation: "leaf-group-fixture", sequence: sequence ?? Int64(index + 1)))
        }
        func group(shared: Bool = false) throws -> [ListingStage.FreshLeafListing] {
            try (0..<count).map { try input($0, records: records($0, shared: shared)) }
        }
        @discardableResult func query(_ sql: String) -> Int64 {
            var db: OpaquePointer?, statement: OpaquePointer?
            precondition(sqlite3_open_v2(store.databasePath, &db, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK)
            defer { sqlite3_finalize(statement); sqlite3_close(db) }
            precondition(sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK)
            let status = sqlite3_step(statement)
            precondition(status == SQLITE_ROW || status == SQLITE_DONE)
            return status == SQLITE_ROW ? sqlite3_column_int64(statement, 0) : 0
        }
        func commit(_ input: [ListingStage.FreshLeafListing], token: ScanCancellation = ScanCancellation()) throws -> Int {
            try store.completeFreshLeafListings(input, rootDevice: device, cancellation: token)
        }
        func untouched() {
            precondition(query("SELECT COUNT(*) FROM nodes WHERE is_dir=0") == 0)
            precondition(query("SELECT COUNT(*) FROM orbit_async_progress") == 0)
            precondition(query("SELECT COUNT(*) FROM prune") == 0)
        }
    }

    static func crashChild(_ base: URL, boundary: String) throws {
        let fixture = try Fixture(base)
        ListingStage.testFreshLeafGroupTime = { 0 }
        ListingStage.testListingCommit = { if $0 == boundary { kill(getpid(), SIGKILL) } }
        _ = try fixture.commit(fixture.group())
        preconditionFailure("Missed grouped commit crash boundary")
    }

    static func run(_ base: URL) throws {
        ListingStage.testFreshLeafGroupTime = { 0 }
        defer { ListingStage.testFreshLeafGroupTime = nil; ListingStage.testListingCommit = nil; ListingStage.testCommitReaders = nil; ListingStage.testPreparedStatement = nil }
        let complete = try Fixture(base.appendingPathComponent("complete"))
        var readers: [String] = []
        var upsertPreparations = 0
        ListingStage.testPreparedStatement = { if $0.contains("INSERT INTO nodes(id,parent,name,search_name") { upsertPreparations += 1 } }
        ListingStage.testCommitReaders = { readers += $0 }
        let completed = try complete.commit(complete.group(shared: true))
        ListingStage.testCommitReaders = nil
        ListingStage.testPreparedStatement = nil
        precondition(completed == 4 && readers.isEmpty && upsertPreparations == 1)
        precondition(complete.query("SELECT sequence FROM orbit_async_progress") == 4)
        precondition(complete.query("SELECT COUNT(*) FROM directories WHERE pending=1") == 0)
        while try complete.store.reconcileLinksChunk() {}
        let totals = try complete.store.folderTotals(path: complete.crawler.rootPath)
        precondition(totals?.count == 4 && totals?.logical == 28 && totals?.allocated == 4_096)

        let prefix = try Fixture(base.appendingPathComponent("prefix"))
        var times = 0
        ListingStage.testFreshLeafGroupTime = { times += 1; return times == 1 ? 0 : 0.006 }
        let prefixCount = try prefix.commit(prefix.group())
        precondition(prefixCount == 1 && prefix.query("SELECT sequence FROM orbit_async_progress") == 1)
        precondition(prefix.query("SELECT COUNT(*) FROM nodes WHERE is_dir=0") == 1)
        precondition(prefix.query("SELECT COUNT(*) FROM directories WHERE pending=1") == 3)
        ListingStage.testFreshLeafGroupTime = { 0 }
        let remaining = try prefix.commit(Array(prefix.group().dropFirst()))
        precondition(remaining == 3 && prefix.query("SELECT sequence FROM orbit_async_progress") == 4)

        for mode in ["dirty", "generation", "identity", "epoch", "prune", "failure", "cancel"] {
            let fixture = try Fixture(base.appendingPathComponent(mode)), token = ScanCancellation()
            var input = try fixture.group()
            switch mode {
            case "dirty": try fixture.store.markDirty(parent: fixture.path(1))
            case "generation": input[1] = try fixture.input(1, generation: 1)
            case "identity": input[1] = try fixture.input(1, replacedIdentity: true)
            case "epoch": _ = try fixture.store.begin(parent: fixture.path(1))
            case "prune":
                var path = fixture.path(1); path.append(contentsOf: "/file".utf8)
                try fixture.store.registerDirectory(path: path, device: fixture.device, inode: 999)
            case "failure":
                fixture.query("CREATE TRIGGER fail_leaf BEFORE INSERT ON nodes WHEN NEW.inode=100001 BEGIN SELECT RAISE(ABORT,'group rollback'); END")
            case "cancel": ListingStage.testListingCommit = { if $0 == "batch-epoch" { token.cancel() } }
            default: break
            }
            let epochs = fixture.query("SELECT COUNT(*) FROM epochs")
            do {
                let committed = try fixture.commit(input, token: token)
                precondition(committed == 0 && mode != "failure" && mode != "cancel")
            } catch is CancellationError { precondition(mode == "cancel") }
            catch ListingStage.Failure.sqlite { precondition(mode == "failure") }
            ListingStage.testListingCommit = nil
            fixture.untouched()
            precondition(fixture.query("SELECT COUNT(*) FROM epochs") == epochs)
            precondition(fixture.query("SELECT COUNT(*) FROM directories WHERE pending=1") >= 4)
        }

        let bounds = try Fixture(base.appendingPathComponent("bounds"), count: 33)
        let oversized = try bounds.commit(bounds.group())
        precondition(oversized == 0)
        var invalid = try Array(bounds.group().prefix(2))
        invalid[1] = try bounds.input(1, sequence: 3)
        let gap = try bounds.commit(invalid)
        precondition(gap == 0)
        invalid[1] = try bounds.input(1, records: Array(repeating: bounds.records(1)[0], count: 128))
        let largeLeaf = try bounds.commit(invalid)
        precondition(largeLeaf == 0)
        invalid[1] = try bounds.input(1, records: [FileMetadata(name: "directory", device: bounds.device, inode: 5,
            isDirectory: true, isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0)])
        let nonLeaf = try bounds.commit(invalid)
        precondition(nonLeaf == 0)
        let tooManyRecords = try (0..<9).map { index in
            try bounds.input(index, records: Array(repeating: bounds.records(index)[0], count: 127))
        }
        let recordCap = try bounds.commit(tooManyRecords)
        precondition(recordCap == 0)
        let longName = String(repeating: "x", count: 256 * 1_024)
        invalid = try Array(bounds.group().prefix(2))
        invalid[1] = try bounds.input(1, records: [FileMetadata(name: longName, device: bounds.device, inode: 5,
            isDirectory: false, isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0)])
        let byteCap = try bounds.commit(invalid)
        precondition(byteCap == 0)
        bounds.untouched()

        let empty = try Fixture(base.appendingPathComponent("empty"))
        let emptyInputs = try (0..<4).map { try empty.input($0, records: []) }
        let emptyCount = try empty.commit(emptyInputs)
        precondition(emptyCount == 4 && empty.query("SELECT COUNT(*) FROM directories WHERE pending=1") == 0)
        precondition(empty.query("SELECT sequence FROM orbit_async_progress") == 4)

        for boundary in ["fresh-group-written", "fresh-group-committed"] {
            let path = base.appendingPathComponent(boundary)
            let child = Process()
            child.executableURL = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
            child.arguments = ["fresh-leaf-group-child", path.path, boundary]
            try child.run(); child.waitUntilExit()
            precondition(child.terminationReason == .uncaughtSignal && child.terminationStatus == SIGKILL)
            let store = try ListingStage(path: path.appendingPathComponent("stage.sqlite").path, mirror: true)
            var db: OpaquePointer?
            precondition(sqlite3_open_v2(store.databasePath, &db, SQLITE_OPEN_READONLY, nil) == SQLITE_OK)
            var query: OpaquePointer?
            precondition(sqlite3_prepare_v2(db, "SELECT (SELECT COUNT(*) FROM nodes WHERE is_dir=0),COALESCE((SELECT sequence FROM orbit_async_progress),0),(SELECT COUNT(*) FROM directories WHERE pending=1)", -1, &query, nil) == SQLITE_OK)
            precondition(sqlite3_step(query) == SQLITE_ROW)
            let committed: Int64 = boundary == "fresh-group-committed" ? 4 : 0
            precondition(sqlite3_column_int64(query, 0) == committed && sqlite3_column_int64(query, 1) == committed
                         && sqlite3_column_int64(query, 2) == 4 - committed)
            sqlite3_finalize(query); sqlite3_close(db); store.close()
        }
        print("PASS: fresh leaf groups bound commits, preserve prefix watermarks/hardlinks, and roll back stale, failed, cancelled and crashed writes")
    }
}
