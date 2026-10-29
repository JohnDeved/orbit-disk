import CSQLite
import Darwin
import Foundation

final class SQLProfiler {
    struct Entry { var calls = 0; var nanoseconds: UInt64 = 0; var vmSteps: Int64 = 0 }
    var entries: [String: Entry] = [:]
    func record(_ statement: OpaquePointer, nanoseconds: UInt64) {
        let sql = sqlite3_sql(statement).map { String(cString: $0) } ?? "unknown"
        var entry = entries[sql, default: Entry()]
        entry.calls += 1; entry.nanoseconds += nanoseconds
        entry.vmSteps += Int64(sqlite3_stmt_status(statement, SQLITE_STMTSTATUS_VM_STEP, 1))
        entries[sql] = entry
    }
    var json: [[String: Any]] {
        entries.sorted { $0.value.nanoseconds > $1.value.nanoseconds }.map {
            ["sql": $0.key, "calls": $0.value.calls, "profile_ms": Double($0.value.nanoseconds) / 1e6,
             "vm_steps": $0.value.vmSteps]
        }
    }
}

struct ListingInput {
    let path: Data
    let batches: [[FileMetadata]]
    let recordCount: Int
}

final class Timings {
    var seconds: [String: Double] = [:]
    var calls: [String: Int] = [:]
    @discardableResult func measure<T>(_ label: String, _ operation: () throws -> T) rethrows -> T {
        let start = ProcessInfo.processInfo.systemUptime
        defer { seconds[label, default: 0] += ProcessInfo.processInfo.systemUptime - start; calls[label, default: 0] += 1 }
        return try operation()
    }
}

@main struct IndexingProbe {
    static func rssAndCPU() -> [String: Double] {
        var usage = rusage(); getrusage(RUSAGE_SELF, &usage)
        return ["peak_rss_mib": Double(usage.ru_maxrss) / 1_048_576,
                "user_seconds": Double(usage.ru_utime.tv_sec) + Double(usage.ru_utime.tv_usec) / 1e6,
                "system_seconds": Double(usage.ru_stime.tv_sec) + Double(usage.ru_stime.tv_usec) / 1e6]
    }
    static func fileBytes(_ path: String) -> Int64 {
        var info = stat(); return lstat(path, &info) == 0 ? info.st_size : 0
    }
    static func walState(_ path: String) -> [String: Int64] {
        var result = ["file_bytes": fileBytes(path + "-wal")]
        guard let handle = FileHandle(forReadingAtPath: path + "-shm") else { return result }
        defer { try? handle.close() }
        guard let bytes = try? handle.read(upToCount: 100), bytes.count == 100,
              bytes.prefix(48) == bytes[48..<96], bytes[12] == 1 else { return result }
        // Read-only SQLite WAL-index diagnostics, sampled between synchronous writer calls.
        bytes.withUnsafeBytes { buffer in
            result["index_version"] = Int64(buffer.loadUnaligned(fromByteOffset: 0, as: UInt32.self))
            result["frames"] = Int64(buffer.loadUnaligned(fromByteOffset: 16, as: UInt32.self))
            result["backfilled_frames"] = Int64(buffer.loadUnaligned(fromByteOffset: 96, as: UInt32.self))
            result["page_size"] = Int64(buffer.loadUnaligned(fromByteOffset: 14, as: UInt16.self))
        }
        return result
    }
    static func chunks(_ records: [FileMetadata]) -> [[FileMetadata]] {
        guard records.count >= 128 else { return [records] }
        var result = [Array(records.prefix(128))], offset = 128
        while offset < records.count {
            let end = min(offset + 1024, records.count)
            result.append(Array(records[offset..<end])); offset = end
        }
        return result
    }
    static func validateDatabase(_ url: URL, expectedFiles: Int, expectedNodes: Int, expectedLogical: Int64,
                                 expectedAllocated: Int64) throws -> [String: Any] {
        var db: OpaquePointer?
        guard sqlite3_open_v2(url.path, &db, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else { throw Failure.message("open result") }
        defer { sqlite3_close_v2(db) }
        func values(_ sql: String) throws -> [Int64] {
            var statement: OpaquePointer?
            guard sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK else { throw Failure.message("prepare validation") }
            defer { sqlite3_finalize(statement) }
            guard sqlite3_step(statement) == SQLITE_ROW else { throw Failure.message("read validation") }
            return (0..<sqlite3_column_count(statement)).map { sqlite3_column_int64(statement, $0) }
        }
        let root = try values("SELECT file_count,size,allocated FROM nodes WHERE parent IS NULL")
        let counts = try values("SELECT COUNT(*),SUM(is_dir=0) FROM nodes")
        let queues = try values("SELECT (SELECT COUNT(*) FROM directories WHERE pending=1)+(SELECT COUNT(*) FROM trim)+(SELECT COUNT(*) FROM dirty_links)")
        guard root == [Int64(expectedFiles), expectedLogical, expectedAllocated],
              counts == [Int64(expectedNodes), Int64(expectedFiles)], queues == [0] else {
            throw Failure.message("result mismatch root=\(root) counts=\(counts) queues=\(queues)")
        }
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(db, "PRAGMA quick_check", -1, &statement, nil) == SQLITE_OK else { throw Failure.message("quick check") }
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW, let text = sqlite3_column_text(statement, 0),
              String(cString: text) == "ok" else { throw Failure.message("database integrity") }
        return ["root_totals": root, "node_counts": counts, "pending_work": 0, "quick_check": "ok"]
    }
    enum Failure: Error { case message(String) }

    static func main() throws {
        guard CommandLine.arguments.count >= 6 else {
            throw Failure.message("Usage: IndexingProbe output-dir shape files directories generation [profile]")
        }
        let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).standardizedFileURL
        let shape = CommandLine.arguments[2], fileCount = Int(CommandLine.arguments[3])!
        let directoryCount = Int(CommandLine.arguments[4])!, generation = Int64(CommandLine.arguments[5])!
        let profiling = CommandLine.arguments.dropFirst(6).contains("profile")
        guard !FileManager.default.fileExists(atPath: directory.path), fileCount >= 0, directoryCount >= 0,
              generation == 0 || generation == 1, ["flat", "small", "empty"].contains(shape),
              shape == "flat" ? directoryCount == 0 : directoryCount > 0 else { throw Failure.message("invalid or existing fixture") }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let root = directory.appendingPathComponent("virtual-root", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        var metadata = stat(); guard lstat(root.path, &metadata) == 0 else { throw Failure.message("fixture root") }
        let device = metadata.st_dev
        let timings = Timings(), fixtureStart = ProcessInfo.processInfo.systemUptime
        var inputs: [ListingInput] = [], expectedLogical: Int64 = 0, expectedAllocated: Int64 = 0
        func files(_ start: Int, _ count: Int) -> [FileMetadata] {
            (start..<(start + count)).map { index in
                let size = Int64(1 + index % 32) * 1024
                let allocated: Int64 = index % 31 == 0 ? 0 : ((size + 4095) / 4096) * 4096
                expectedLogical += size; expectedAllocated += allocated
                let name = index % 19 == 0 ? "résumé-\(index).bin" : "file-\(index).bin"
                return FileMetadata(name: name, device: device, inode: UInt64(index) + 1_000_000,
                    isDirectory: false, isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: size, allocatedSize: allocated)
            }
        }
        if shape == "flat" {
            inputs = [ListingInput(path: Data(root.path.utf8), batches: chunks(files(0, fileCount)), recordCount: fileCount)]
        } else {
            let directories = (0..<directoryCount).map { index in
                FileMetadata(name: "d-\(index)", device: device, inode: UInt64(index) + 100,
                    isDirectory: true, isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0)
            }
            inputs.append(ListingInput(path: Data(root.path.utf8), batches: chunks(directories), recordCount: directoryCount))
            var offset = 0
            for index in 0..<directoryCount {
                let count = fileCount / directoryCount + (index < fileCount % directoryCount ? 1 : 0)
                inputs.append(ListingInput(path: Data(root.appendingPathComponent("d-\(index)").path.utf8),
                                           batches: chunks(files(offset, count)), recordCount: count))
                offset += count
            }
        }
        let fixtureSeconds = ProcessInfo.processInfo.systemUptime - fixtureStart
        let checkpoint = StageCheckpoint(rootPath: root.path, identity: "synthetic-indexing-benchmark", eventID: 0)
        let stage = try timings.measure("initialization") {
            try DurableCrawler(rootURL: root, database: directory.appendingPathComponent("stage.sqlite").path,
                               checkpoint: checkpoint, mirror: true)
        }
        let store = stage.store, profiler = SQLProfiler()
        guard chmod(store.databasePath, 0o600) == 0 else { throw Failure.message("private stage permissions") }
        if profiling { store.benchmarkProfile(profiler) }
        try timings.measure("queue_indexes") { try store.prepareQueueIndexes(cancellation: stage.cancellation) }
        if generation != 0 {
            try timings.measure("generation_setup") {
                try store.markFullRefresh()
                while try store.markFullRefreshChunk() {}
            }
        }
        let cpuBefore = rssAndCPU(), ingestionStart = ProcessInfo.processInfo.systemUptime
        var sequence: Int64 = 0, freshSuccess = 0, freshReject = 0, finishReject = 0, maximumWAL: Int64 = 0
        var maximumWALState: [String: Int64] = [:]
        let token = UUID().uuidString
        func nextCommit() -> AsyncInventoryCommit {
            sequence += 1; return AsyncInventoryCommit(generation: token, sequence: sequence)
        }
        var directoryIndex = 0, groupedListings = 0
        while directoryIndex < inputs.count {
            let input = inputs[directoryIndex]
            #if GROUPED_WRITER
            if directoryIndex > 0 {
                var group: [ListingStage.FreshLeafListing] = [], groupRecords = 0, groupBytes = 0
                for index in directoryIndex..<min(directoryIndex + 32, inputs.count) {
                    let leaf = inputs[index]
                    guard leaf.batches.count == 1, leaf.recordCount < 128,
                          !leaf.batches[0].contains(where: \.isDirectory),
                          groupRecords + leaf.recordCount <= 1024 else { break }
                    let listing = ListingStage.FreshLeafListing(
                        directory: .init(path: leaf.path, device: device, inode: UInt64(index - 1) + 100),
                        records: leaf.batches[0], expectedRevision: 0, expectedFullGeneration: generation,
                        asyncCommit: .init(generation: token, sequence: sequence + Int64(group.count + 1)))
                    guard groupBytes + listing.byteCount <= 256 * 1024 else { break }
                    group.append(listing); groupRecords += leaf.recordCount; groupBytes += listing.byteCount
                }
                if group.count > 1 {
                    let committed = try timings.measure("fresh_group") {
                        try store.completeFreshLeafListings(group, rootDevice: device, cancellation: stage.cancellation)
                    }
                    if committed > 0 {
                        sequence += Int64(committed); freshSuccess += committed; groupedListings += committed
                        try timings.measure("links") { while try store.reconcileLinksChunk(cancellation: stage.cancellation) {} }
                        if sequence / 128 > (sequence - Int64(committed)) / 128 || directoryIndex + committed == inputs.count {
                            let state = walState(store.databasePath)
                            if (state["file_bytes"] ?? 0) > maximumWAL {
                                maximumWAL = state["file_bytes"] ?? 0; maximumWALState = state
                            }
                        }
                        try timings.measure("trim") { while try store.trimOneChunk() {} }
                        directoryIndex += committed
                        continue
                    }
                }
            }
            #endif
            var epoch: Int64?, completed = false
            for (batchIndex, batch) in input.batches.enumerated() {
                let last = batchIndex == input.batches.count - 1, commit = nextCommit()
                if last, epoch == nil, batch.count < 128 {
                    completed = try timings.measure("fresh_listing") {
                        try store.completeFreshListing(parent: input.path, records: batch, rootDevice: device,
                            expectedRevision: 0, expectedFullGeneration: generation, asyncCommit: commit)
                    }
                    if completed { freshSuccess += 1 } else { freshReject += 1 }
                }
                if !completed {
                    if !batch.isEmpty || !last {
                        epoch = try timings.measure("append") {
                            try store.append(parent: input.path, epoch: epoch, records: batch, rootDevice: device, asyncCommit: commit)
                        }
                    }
                    if last {
                        completed = try timings.measure("finish") {
                            try store.finish(parent: input.path, epoch: epoch, expectedRevision: 0,
                                expectedFullGeneration: generation, asyncCommit: commit)
                        }
                        if !completed { finishReject += 1 }
                    }
                }
                try timings.measure("links") { while try store.reconcileLinksChunk(cancellation: stage.cancellation) {} }
                if sequence % 128 == 0 || (directoryIndex == inputs.count - 1 && last) {
                    let state = walState(store.databasePath)
                    if (state["file_bytes"] ?? 0) > maximumWAL {
                        maximumWAL = state["file_bytes"] ?? 0; maximumWALState = state
                    }
                }
                // Crawler dispatch drains completed-listing trim work before consuming the next listing.
                if last { try timings.measure("trim") { while try store.trimOneChunk() {} } }
            }
            directoryIndex += 1
        }
        let ingestionSeconds = ProcessInfo.processInfo.systemUptime - ingestionStart, cpuAfter = rssAndCPU()
        let walAfterIngestion = walState(store.databasePath)
        let publicationStart = ProcessInfo.processInfo.systemUptime
        var boundaries: [String: Double] = [:]
        let published = try timings.measure("publication") {
            try MirrorPublisher.publish(stage, checkpoint: checkpoint, directory: directory.appendingPathComponent("published"),
                                        elapsed: ingestionSeconds, expected: .absent,
                                        boundary: { boundaries[$0] = ProcessInfo.processInfo.systemUptime - publicationStart })
        }
        let validation = try timings.measure("verification") {
            try validateDatabase(published.databaseURL, expectedFiles: fileCount, expectedNodes: fileCount + directoryCount + 1,
                                 expectedLogical: expectedLogical, expectedAllocated: expectedAllocated)
        }
        var result: [String: Any] = ["shape": shape, "files": fileCount, "directories": directoryCount + 1,
            "generation": generation, "profiled": profiling, "fixture_seconds": fixtureSeconds,
            "ingestion_seconds": ingestionSeconds, "records_per_second": Double(fileCount + directoryCount) / ingestionSeconds,
            "phase_seconds": timings.seconds, "phase_calls": timings.calls, "publication_boundaries_seconds": boundaries,
            "fresh_success": freshSuccess, "fresh_reject": freshReject, "finish_reject": finishReject,
            "grouped_listings": groupedListings,
            "async_commits": sequence, "wal_max_sampled_bytes": maximumWAL,
            "wal_at_maximum": maximumWALState, "wal_after_ingestion": walAfterIngestion,
            "database_bytes": fileBytes(published.databaseURL.path), "resource_before": cpuBefore,
            "resource_after": cpuAfter, "validation": validation, "pid": getpid()]
        if profiling { result["sql_profile"] = profiler.json }
        let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
        try data.write(to: directory.appendingPathComponent("result.json"))
        print(String(decoding: data, as: UTF8.self))
    }
}
