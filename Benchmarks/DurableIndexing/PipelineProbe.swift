#if SHARED_CORE
@testable import OrbitCore
#endif
import CSQLite
import Darwin
import Foundation

final class PipelineCounters: @unchecked Sendable {
    private let lock = NSLock()
    private var counts: [String: Int] = [:]
    func add(_ name: String) { lock.lock(); counts[name, default: 0] += 1; lock.unlock() }
    func read() -> [String: Int] { lock.lock(); defer { lock.unlock() }; return counts }
}

@main struct PipelineProbe {
    static func scalar(_ path: String, _ sql: String) -> Int64 {
        var database: OpaquePointer?, statement: OpaquePointer?
        precondition(sqlite3_open_v2(path, &database, SQLITE_OPEN_READONLY, nil) == SQLITE_OK)
        defer { sqlite3_finalize(statement); sqlite3_close_v2(database) }
        precondition(sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK)
        precondition(sqlite3_step(statement) == SQLITE_ROW)
        return sqlite3_column_int64(statement, 0)
    }

    static func main() throws {
        let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).standardizedFileURL
        let cache = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true).standardizedFileURL
        let files = Int(CommandLine.arguments[3])!, directories = Int(CommandLine.arguments[4])!
        precondition(!FileManager.default.fileExists(atPath: cache.path))
        let counts = PipelineCounters(), started = ProcessInfo.processInfo.systemUptime
        let checkpoint = StageCheckpoint(rootPath: root.path, identity: "pipeline-indexing-benchmark", eventID: 0)
        let stage = try ResumableStage(cache: cache, root: root, current: checkpoint, cancellation: ScanCancellation())
        defer { stage.crawler.cancelAsyncDiscovery() }
        try stage.prepareRemovalReplay()
        let initialized = ProcessInfo.processInfo.systemUptime
        AsyncListingProducer.testBeforeDirectory = { _, _ in counts.add("producer_directories") }
        ListingStage.testListingCommit = { counts.add($0) }
        defer { AsyncListingProducer.testBeforeDirectory = nil; ListingStage.testListingCommit = nil }
        try stage.startDiscovery(progress: { _ in })
        guard let inventory = stage.crawler.asyncInventory else { fatalError("Expected parallel discovery") }
        // Installing this after producer startup counts fallback enumeration without disabling async discovery.
        stage.crawler.testBeforeListing = { counts.add("serial_directories") }
        let writerStarted = ProcessInfo.processInfo.systemUptime
        var advances = 0
        while try stage.advance(progress: { _ in }) { advances += 1 }
        let ready = ProcessInfo.processInfo.systemUptime
        guard let discoveryFinished = inventory.discoveryFinishedAt else { fatalError("Discovery did not finish") }
        let snapshot = try stage.publish(elapsed: ready - initialized)
        let published = ProcessInfo.processInfo.systemUptime
        let actualFiles = scalar(snapshot.databaseURL.path, "SELECT COUNT(*) FROM nodes WHERE is_dir=0")
        let actualDirectories = scalar(snapshot.databaseURL.path, "SELECT COUNT(*) FROM nodes WHERE is_dir=1")
        let rootFiles = scalar(snapshot.databaseURL.path, "SELECT file_count FROM nodes WHERE parent IS NULL")
        let pending = scalar(snapshot.databaseURL.path, "SELECT COUNT(*) FROM directories WHERE pending=1")
        precondition(actualFiles == files && actualDirectories == directories && rootFiles == files && pending == 0)
        precondition(counts.read()["producer_directories"] == directories)
        precondition(counts.read()["serial_directories", default: 0] == 0)
        let result: [String: Any] = [
            "files": files, "directories": directories, "advances": advances,
            "initialization_seconds": initialized - started,
            "discovery_seconds": discoveryFinished - initialized,
            "writer_loop_seconds": ready - writerStarted,
            "post_discovery_drain_seconds": max(0, ready - discoveryFinished),
            "mapping_ready_seconds": ready - initialized,
            "publication_seconds": published - ready,
            "total_seconds": published - started,
            "counters": counts.read(),
            "verified": true,
        ]
        let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
        print(String(decoding: data, as: UTF8.self))
    }
}
