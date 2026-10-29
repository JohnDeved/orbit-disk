import CSQLite
import Darwin
import Foundation

@main struct MarkingProbe {
    static func sql(_ path: String, _ sql: String) throws -> Int64 {
        var database: OpaquePointer?, statement: OpaquePointer?
        precondition(sqlite3_open_v2(path, &database, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK)
        defer { sqlite3_finalize(statement); sqlite3_close_v2(database) }
        precondition(sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK)
        let result = sqlite3_step(statement)
        precondition(result == SQLITE_ROW || result == SQLITE_DONE)
        return result == SQLITE_ROW ? sqlite3_column_int64(statement, 0) : 0
    }
    static func main() throws {
        let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).standardizedFileURL
        let count = Int(CommandLine.arguments[2])!, rounds = Int(CommandLine.arguments[3])!
        precondition(!FileManager.default.fileExists(atPath: directory.path))
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let root = directory.appendingPathComponent("virtual-root", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        var info = stat(); precondition(lstat(root.path, &info) == 0)
        let checkpoint = StageCheckpoint(rootPath: root.path, identity: "synthetic-marking-benchmark", eventID: 0)
        let crawler = try DurableCrawler(rootURL: root, database: directory.appendingPathComponent("stage.sqlite").path,
                                         checkpoint: checkpoint, mirror: true)
        let stage = crawler.store, path = Data(root.path.utf8)
        try stage.prepareQueueIndexes(cancellation: crawler.cancellation)
        let setupStart = ProcessInfo.processInfo.systemUptime
        var epoch: Int64?
        for start in stride(from: 0, to: count, by: 1024) {
            let records = (start..<min(start + 1024, count)).map { index in
                FileMetadata(name: "d-\(index)", device: info.st_dev, inode: UInt64(index) + 1_000,
                             isDirectory: true, isSymbolicLink: false, isMountPoint: false,
                             linkCount: 1, size: 0, allocatedSize: 0)
            }
            epoch = try stage.append(parent: path, epoch: epoch, records: records, rootDevice: info.st_dev)
        }
        let finished = try stage.finish(parent: path, epoch: epoch, expectedRevision: 0, expectedFullGeneration: 0)
        precondition(finished)
        while try stage.trimOneChunk() {}
        let setupSeconds = ProcessInfo.processInfo.systemUptime - setupStart
        var observations: [[String: Any]] = []
        for round in 1...rounds {
            _ = try sql(stage.databasePath, "UPDATE directories SET pending=0")
            let started = ProcessInfo.processInfo.systemUptime
            try stage.markFullRefresh()
            var chunks = 0
            while try stage.markFullRefreshChunk() { chunks += 1 }
            let elapsed = ProcessInfo.processInfo.systemUptime - started
            let generation = try stage.fullRefreshState().generation
            let verified = try sql(stage.databasePath,
                "SELECT COUNT(*) FROM directories WHERE pending=1 AND full_generation=\(generation)")
            precondition(verified == Int64(count + 1))
            observations.append(["round": round, "seconds": elapsed, "chunks": chunks,
                                 "generation": generation, "verified_directories": verified])
        }
        let result: [String: Any] = ["directories": count + 1, "setup_seconds": setupSeconds,
                                     "marking": observations]
        let bytes = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
        try bytes.write(to: directory.appendingPathComponent("result.json"))
        print(String(decoding: bytes, as: UTF8.self))
    }
}
