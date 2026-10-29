import CSQLite
import Foundation

enum FullRefreshQueueChecks {
    static func run(_ base: URL) throws {
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        let path = base.appendingPathComponent("queue.sqlite").path
        var store = try ListingStage(path: path)
        defer { store.close() }
        func query(_ sql: String) -> Int64 {
            var database: OpaquePointer?, statement: OpaquePointer?
            precondition(sqlite3_open_v2(path, &database, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK)
            defer { sqlite3_finalize(statement); sqlite3_close(database) }
            precondition(sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK)
            let status = sqlite3_step(statement)
            precondition(status == SQLITE_ROW || status == SQLITE_DONE)
            return status == SQLITE_ROW ? sqlite3_column_int64(statement, 0) : 0
        }
        func drain() throws {
            var steps = 0
            while try store.markFullRefreshChunk() {
                steps += 1
                precondition(steps < 10, "Full-refresh queue failed to finish")
            }
        }
        func markChunk() throws {
            let advanced = try store.markFullRefreshChunk()
            precondition(advanced)
        }
        _ = query("""
            WITH RECURSIVE numbers(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM numbers WHERE n<259)
            INSERT INTO directories(path,device,inode,pending)
            SELECT CAST('/refresh/d-' || printf('%03d',n) AS BLOB),1,n+1,0 FROM numbers
            """)
        try store.markFullRefresh()
        try markChunk()
        precondition(query("SELECT COUNT(*) FROM directories WHERE full_generation=1") == 128)
        try store.registerDirectory(path: Data("/refresh/000-added".utf8), device: 1, inode: 1000)
        try store.startPrune(path: Data("/refresh/d-250".utf8), parent: Data("/refresh".utf8), name: Data("d-250".utf8))
        while try store.pruneOneChunk() {}
        try drain()
        precondition(query("SELECT COUNT(*) FROM directories") == 260)
        precondition(query("SELECT COUNT(*) FROM directories WHERE full_generation=1") == 259)
        precondition(query("SELECT COUNT(*) FROM directories WHERE pending=1") == 260,
                     "A newly registered path before the cursor lost its already pending work")

        _ = query("UPDATE directories SET pending=0")
        try store.markFullRefresh()
        try markChunk()
        precondition(query("SELECT COUNT(*) FROM directories WHERE full_generation=2") == 128)
        store.close()
        store = try ListingStage(path: path)
        try drain()
        precondition(query("SELECT COUNT(*) FROM directories WHERE full_generation=2 AND pending=1") == 260,
                     "Reopening an interrupted refresh skipped unmarked directories")

        _ = query("UPDATE directories SET pending=0")
        try store.markFullRefresh()
        try markChunk()
        try store.markFullRefresh()
        try drain()
        precondition(query("SELECT COUNT(*) FROM directories WHERE full_generation=4 AND pending=1") == 260,
                     "A newer generation inherited the previous generation's cursor")

        try store.markFullRefresh()
        try markChunk()
        _ = query("CREATE TRIGGER fail_refresh BEFORE UPDATE OF full_generation ON directories WHEN NEW.full_generation=5 BEGIN SELECT RAISE(ABORT,'rollback refresh'); END")
        do { _ = try store.markFullRefreshChunk(); preconditionFailure("Faulted refresh transaction committed") }
        catch ListingStage.Failure.sqlite {}
        _ = query("DROP TRIGGER fail_refresh")
        try drain()
        precondition(query("SELECT COUNT(*) FROM directories WHERE full_generation=5") == 260,
                     "A rolled-back refresh transaction advanced the cursor")
        print("PASS: bounded refresh cursor preserves low-path additions, pruning, interrupted reopen, newer generations and rollback")
    }
}
