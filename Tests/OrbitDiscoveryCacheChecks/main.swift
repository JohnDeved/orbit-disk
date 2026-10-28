import CSQLite
import Darwin
import Foundation
#if DISCOVERY_CACHE_SHARED_MODULE
@testable import OrbitCore
#endif

private struct CheckFailure: Error, CustomStringConvertible {
    let description: String
}

private func require(_ condition: @autoclosure () throws -> Bool, _ message: String) throws {
    guard try condition() else { throw CheckFailure(description: message) }
}

private final class Fixture {
    let path: String
    let writer: OpaquePointer
    let identity = DirectoryIdentity(device: 1, inode: 10)
    var directory: ListingStage.QueuedDirectory {
        ListingStage.QueuedDirectory(path: Data("/root".utf8), device: identity.device, inode: identity.inode)
    }

    init(_ base: URL, _ name: String) throws {
        path = base.appendingPathComponent(name + ".sqlite").path
        var handle: OpaquePointer?
        guard sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nil) == SQLITE_OK,
              let handle else { throw CheckFailure(description: "Could not create private fixture") }
        writer = handle
        sqlite3_busy_timeout(writer, 0)
        try exec("""
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=FULL;
            PRAGMA wal_autocheckpoint=0;
            CREATE TABLE full_refresh(generation INTEGER,pending INTEGER);
            INSERT INTO full_refresh VALUES(7,0);
            CREATE TABLE directories(path BLOB,device INTEGER,inode INTEGER,pending INTEGER,full_generation INTEGER);
            INSERT INTO directories VALUES(x'2f726f6f74',1,10,0,7);
            CREATE TABLE dirty(parent BLOB,pending INTEGER,revision INTEGER);
            INSERT INTO dirty VALUES(x'2f726f6f74',0,0);
            CREATE TABLE nodes(id INTEGER PRIMARY KEY,parent INTEGER,name TEXT,is_dir INTEGER,is_link INTEGER,
                accessible INTEGER,size INTEGER,raw_alloc INTEGER,dev INTEGER,inode INTEGER,is_mount INTEGER,
                links INTEGER,search_name TEXT);
            INSERT INTO nodes VALUES(1,NULL,'root',1,0,1,0,0,1,10,0,1,'root');
            INSERT INTO nodes VALUES(2,1,'before',0,0,1,1,4096,1,20,0,1,'before');
            INSERT INTO nodes VALUES(3,1,'second',0,0,1,2,4096,1,21,0,1,'second');
            CREATE TABLE directory_paths(id INTEGER PRIMARY KEY,path TEXT);
            INSERT INTO directory_paths VALUES(1,'/root');
            """)
    }

    deinit { sqlite3_close_v2(writer) }

    func exec(_ sql: String) throws {
        guard sqlite3_exec(writer, sql, nil, nil, nil) == SQLITE_OK else {
            throw CheckFailure(description: "Fixture SQL: " + String(cString: sqlite3_errmsg(writer)))
        }
    }

    func reader(_ cancellation: ScanCancellation = ScanCancellation(), identity: DirectoryIdentity? = nil) throws -> DiscoveryCacheReader {
        try DiscoveryCacheReader(path: path, rootIdentity: identity ?? self.identity, cancellation: cancellation)
    }

    func checkpoint() throws {
        let status = sqlite3_wal_checkpoint_v2(writer, nil, SQLITE_CHECKPOINT_TRUNCATE, nil, nil)
        try require(status == SQLITE_OK, "Idle reader prevented WAL checkpoint: \(status)")
        let bytes = try FileManager.default.attributesOfItem(atPath: path + "-wal")[.size] as? NSNumber
        try require(bytes?.int64Value == 0, "Checkpoint did not truncate the WAL")
    }

    func write(_ revision: Int) throws {
        try exec("""
            BEGIN IMMEDIATE;
            UPDATE dirty SET revision=\(revision);
            UPDATE full_refresh SET generation=\(revision + 7);
            UPDATE directories SET full_generation=\(revision + 7);
            UPDATE nodes SET size=\(revision + 1) WHERE id=2;
            COMMIT;
            """)
    }
}

@main private enum DiscoveryCacheChecks {
    static func idleReaders(_ base: URL, count: Int) throws {
        let fixture = try Fixture(base, "idle-\(count)")
        let readers = try (0..<count).map { _ in try fixture.reader() }
        try withExtendedLifetime(readers) {
            // Initialization itself used to pin a read mark, even before the first listing.
            try fixture.write(1)
            try fixture.checkpoint()
            for revision in 2...16 {
                try fixture.write(revision)
                for reader in readers {
                    let result = try reader.listing(fixture.directory)
                    try require(result?.revision == Int64(revision), "Reader kept an earlier dirty revision")
                    try require(result?.generation == Int64(revision + 7), "Reader kept an earlier refresh generation")
                    try require(result?.records?.first(where: { $0.name == "before" })?.size == Int64(revision + 1),
                                "Reader did not see the latest committed listing")
                }
                try fixture.checkpoint()
            }
        }
        print("PASS: \(count) live idle reader(s) allow repeated WAL checkpoints and observe new revisions")
    }

    static func consistentListing(_ base: URL) throws {
        let fixture = try Fixture(base, "consistent")
        let reader = try fixture.reader()
        reader.testAfterStateRead = {
            try fixture.write(1)
            try fixture.exec("UPDATE nodes SET name='after',search_name='after' WHERE id=2")
            try require(sqlite3_wal_checkpoint_v2(fixture.writer, nil, SQLITE_CHECKPOINT_TRUNCATE, nil, nil) == SQLITE_BUSY,
                        "The in-flight listing did not retain its consistent snapshot")
        }
        let old = try reader.listing(fixture.directory)
        reader.testAfterStateRead = nil
        try require(old?.revision == 0 && old?.generation == 7, "Listing state changed mid-read")
        try require(old?.records?.contains(where: { $0.name == "before" && $0.size == 1 }) == true,
                    "Listing combined old state with new rows")
        try fixture.checkpoint()
        let current = try reader.listing(fixture.directory)
        try require(current?.revision == 1 && current?.generation == 8, "Next listing did not refresh its state")
        try require(current?.records?.contains(where: { $0.name == "after" && $0.size == 2 }) == true,
                    "Next listing did not refresh its rows")
        try fixture.checkpoint()
        print("PASS: concurrent writes cannot tear one listing; the next lookup sees the new snapshot")
    }

    static func exitsReleaseSnapshot(_ base: URL) throws {
        let fixture = try Fixture(base, "exits")
        let reader = try fixture.reader()
        let absent = ListingStage.QueuedDirectory(path: Data("/absent".utf8), device: 1, inode: 10)
        try require(try reader.listing(absent) == nil, "Missing directory was reused")
        let replacement = ListingStage.QueuedDirectory(path: Data("/root".utf8), device: 1, inode: 11)
        try require(try reader.listing(replacement) == nil, "Replaced directory identity was reused")
        try fixture.write(1); try fixture.checkpoint()
        for sql in ["UPDATE directories SET pending=1", "UPDATE directories SET pending=0; UPDATE dirty SET pending=1",
                    "UPDATE dirty SET pending=0; UPDATE full_refresh SET pending=1; UPDATE directories SET full_generation=0"] {
            try fixture.exec(sql)
            try require(try reader.listing(fixture.directory)?.records == nil, "Incomplete listing was reused")
            try fixture.write(2); try fixture.checkpoint()
        }
        try fixture.exec("UPDATE full_refresh SET pending=0; UPDATE nodes SET name='invalid/name' WHERE id=2")
        do {
            _ = try reader.listing(fixture.directory)
            throw CheckFailure(description: "Malformed cache record was accepted")
        } catch StageReuseSource.Failure.unsuitable { }
        try fixture.write(3); try fixture.checkpoint()
        try fixture.exec("DROP TABLE nodes")
        do {
            _ = try reader.listing(fixture.directory)
            throw CheckFailure(description: "Missing cache table was accepted")
        } catch StageReuseSource.Failure.unsuitable { }
        try fixture.exec("UPDATE dirty SET revision=4")
        try fixture.checkpoint()
        print("PASS: missing/replaced/unfinished listings and decode/SQL errors release their snapshots")
    }

    static func cancellationReleasesSnapshot(_ base: URL) throws {
        let fixture = try Fixture(base, "cancel")
        let cancellation = ScanCancellation()
        let reader = try fixture.reader(cancellation)
        reader.testAfterRecordRead = { cancellation.cancel() }
        do {
            _ = try reader.listing(fixture.directory)
            throw CheckFailure(description: "Mid-listing cancellation was ignored")
        } catch is CancellationError { }
        try fixture.write(1); try fixture.checkpoint()
        do {
            _ = try reader.listing(fixture.directory)
            throw CheckFailure(description: "Cancelled reader started another listing")
        } catch is CancellationError { }
        try fixture.write(2); try fixture.checkpoint()
        do {
            _ = try fixture.reader(identity: DirectoryIdentity(device: 1, inode: 99))
            throw CheckFailure(description: "Wrong root identity was accepted")
        } catch StageReuseSource.Failure.unsuitable { }
        try fixture.write(3); try fixture.checkpoint()
        print("PASS: cancellation and initializer failure release read transactions")
    }

    static func main() throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        try idleReaders(base, count: 1)
        try idleReaders(base, count: 4)
        try consistentListing(base)
        try exitsReleaseSnapshot(base)
        try cancellationReleasesSnapshot(base)
        print("PASS: discovery cache snapshots remain short and consistent")
    }
}
