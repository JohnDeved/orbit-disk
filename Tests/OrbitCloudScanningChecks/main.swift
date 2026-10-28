import CSQLite
import Foundation

private func require(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
    if try !value() { throw NSError(domain: message, code: 1) }
}

@main struct CloudScanningChecks {
    static func main() throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let root = base.appendingPathComponent("home"), cache = base.appendingPathComponent("cache")
        let cloud = root.appendingPathComponent("Library/CloudStorage"), documents = root.appendingPathComponent("Documents")
        func write(_ path: String) throws {
            let url = root.appendingPathComponent(path)
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data([1, 2, 3]).write(to: url)
        }
        try write("Library/CloudStorage/provider/cloud-file")
        try write("Library/CloudStorage-sibling/local-file")
        try write("Documents/cloud-document")
        try write("Downloads/local-download")
        for index in 0..<1_100 { try write("local-\(index)") }
        CloudStoragePolicy.testScanExclusions = [cloud, documents]
        defer { CloudStoragePolicy.testScanExclusions = nil }
        func verify(_ node: DiskNode) throws {
            try require(!node.id.hasPrefix(cloud.path + "/") && node.id != cloud.path, "Cloud entry leaked into inventory")
            try require(!node.id.hasPrefix(documents.path + "/") && node.id != documents.path, "Synced Documents leaked into inventory")
            for child in node.children { try verify(child) }
        }
        for bulk in [true, false] {
            let result = try DiskScanner(useBulkEnumeration: bulk).scan(root, preview: DiskScanPreview())
            try verify(result.root)
            try require(result.root.fileCount == 1_102, "Local totals changed")
            for blocked in [cloud, cloud.appendingPathComponent("provider"), documents] {
                do { _ = try DiskScanner(useBulkEnumeration: bulk).scan(blocked); throw NSError(domain: "Cloud root accepted", code: 1) }
                catch ScanError.excluded { }
            }
        }
        print("PASS bulk, fallback, direct cloud roots, sibling prefixes and local totals")
        let checkpoint = StageCheckpoint(rootPath: root.path, identity: "fixture", eventID: 0)
        for asynchronous in [false, true] {
            let directory = cache.appendingPathComponent(asynchronous ? "async" : "sync")
            let stage = try ResumableStage(cache: directory, root: root, current: checkpoint)
            stage.testDisableAsyncDiscovery = !asynchronous
            if asynchronous { try stage.startDiscovery(progress: { _ in }) }
            while try stage.advance(progress: { _ in }) { }
            let saved = try stage.publish(elapsed: 0)
            let store = DiskIndexStore(directory: directory)
            try require(try store.node(path: root.path, snapshot: saved)?.fileCount == 1_102, "Staged totals include cloud files")
            for blocked in [cloud, cloud.appendingPathComponent("provider/cloud-file"), documents] {
                try require(try store.node(path: blocked.path, snapshot: saved) == nil, "Cloud entry persisted")
            }
            try require(try store.search(query: "cloud", snapshot: saved).totalCount == 1, "Cloud contents entered search index")
            print("PASS \(asynchronous ? "async" : "sync") discovery, durable cache and search exclusions")
        }
        let legacyStore = DiskIndexStore(directory: cache.appendingPathComponent("legacy"))
        let legacy = try legacyStore.save(DiskScanner().scan(root), eventID: 0, eventIdentity: "fixture")
        var db: OpaquePointer?
        try require(sqlite3_open(legacy.databaseURL.path, &db) == SQLITE_OK, "Open legacy fixture")
        try require(sqlite3_exec(db, "PRAGMA user_version=4", nil, nil, nil) == SQLITE_OK, "Mark legacy fixture")
        sqlite3_close(db)
        try require(legacyStore.savedSnapshot(for: root) == nil, "Pre-exclusion saved map reopened")
        try require(try MappingCoverage.find(for: root, cache: legacyStore.directory) == nil, "Legacy coverage reused")
        let replacement = try ResumableStage(cache: legacyStore.directory, root: root, current: checkpoint)
        while try replacement.advance(progress: { _ in }) { }
        _ = try replacement.publish(elapsed: 0)
        try require(legacyStore.savedSnapshot(for: root) != nil, "Legacy map could not be replaced")
        print("PASS old inventories rejected and replaced")
    }
}
