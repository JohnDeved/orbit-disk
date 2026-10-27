import Foundation
import OrbitCore
import CSQLite

private struct Signature: Equatable {
    let path: Data
    let name: String
    let size: Int64
    let allocated: Int64
    let count: Int
    let identity: String?
    let state: NodeScanState
    init(_ node: DiskNode) {
        path = Data(node.id.utf8); name = node.name; size = node.size
        allocated = node.allocatedSize; count = node.fileCount; identity = node.fileIdentity; state = node.scanState
    }
}

@main struct Checks {
    static let queries = ["file", "rare-needle", "missing-needle", "cafe", "CAFÉ", "K", "が", "नमस्ते", "%_", "[x]", "'", "💾", "e\u{301}", "i", "\0", "file\0tail"]
    static func main() throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1])
        try saved(base.appendingPathComponent("complete"), capped: false)
        try saved(base.appendingPathComponent("capped"), capped: true)
        try staged(base.appendingPathComponent("stage"), capped: true)
        try staged(base.appendingPathComponent("broad-stage"), capped: false)
        print("PASS: prepared saved/staged search, Unicode and literal queries, counts/order/limits, deletion overlays, capacity, stale files, name changes and monotonic tails")
    }

    static func saved(_ base: URL, capped: Bool) throws {
        let root = base.appendingPathComponent("root"), cache = base.appendingPathComponent("cache")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        var children: [DiskNode] = []
        for i in 0..<6_000 {
            let suffix = ["café", "cafe\u{301}", "K", "が", "नमस्ते", "%_", "[x]", "'", "💾"][i % 9]
            let name = "\(i % 100 == 0 ? "rare-needle" : "file")-\(suffix)-\(i)"
            children.append(DiskNode(url: root.appendingPathComponent(name), name: name, isDirectory: false,
                size: Int64(i % 41), allocatedSize: Int64(i % 41), fileCount: 1, fileIdentity: "1:\(i + 100)"))
        }
        let node = DiskNode(url: root, name: "root", isDirectory: true, size: children.reduce(0) { $0 + $1.size },
            allocatedSize: children.reduce(0) { $0 + $1.allocatedSize }, fileCount: children.count, children: children)
        let store = DiskIndexStore(directory: cache)
        let snapshot = try store.save(ScanResult(root: node, elapsed: 0, skippedCount: 0, issues: []), eventID: 0, eventIdentity: "tests")
        var reference: [String: ([Signature], Int)] = [:]
        for query in queries { for stable in [false, true] { for limit in [0, 1, 300, 10_000] {
            let result = try store.search(query: query, snapshot: snapshot, limit: limit, stablePathOrder: stable)
            reference["\(query)|\(stable)|\(limit)"] = (result.nodes.map(Signature.init), result.totalCount)
        } } }
        DiskIndexStore.testSearchIndexByteLimit = capped ? 3_200_000 : nil
        defer { DiskIndexStore.testSearchIndexByteLimit = nil }
        let prepared = try store.prepareSearch(snapshot: snapshot)
        precondition(prepared && store.isSearchPrepared(snapshot: snapshot))
        let stats = store.testSearchIndex(snapshot: snapshot)!
        precondition(stats.complete != capped && stats.count > 0)
        precondition(stats.bytes <= (capped ? 3_200_000 : 96 * 1024 * 1024))
        for query in queries { for stable in [false, true] { for limit in [0, 1, 300, 10_000] {
            let result = try store.search(query: query, snapshot: snapshot, limit: limit, stablePathOrder: stable)
            let expected = reference["\(query)|\(stable)|\(limit)"]!
            precondition(result.nodes.map(Signature.init) == expected.0 && result.totalCount == expected.1,
                "saved mismatch \(query) \(stable) \(limit), capped=\(capped)")
        } } }
        let removal = ScanRemoval(path: children[0].id, identity: children[0].fileIdentity!)
        let removed = try store.search(query: "rare-needle", snapshot: snapshot, removals: [removal])
        precondition(removed.totalCount == 59 && !removed.nodes.contains { $0.id == removal.path })
        let cancellation = ScanCancellation(); cancellation.cancel()
        do { _ = try store.search(query: "file", snapshot: snapshot, cancellation: cancellation); fatalError("cancelled cached query returned") }
        catch is CancellationError {}
        do { _ = try store.prepareSearch(snapshot: snapshot, cancellation: cancellation); fatalError("cancelled preparation returned") }
        catch is CancellationError {}
        try FileManager.default.removeItem(at: snapshot.databaseURL)
        precondition(!store.isSearchPrepared(snapshot: snapshot))
        do { _ = try store.search(query: "missing-needle", snapshot: snapshot); fatalError("missing database served from cache") }
        catch DiskIndexError.database {}
    }

    static func staged(_ base: URL, capped: Bool) throws {
        let root = base.appendingPathComponent("root"), cache = base.appendingPathComponent("cache")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        for i in 0..<(capped ? 500 : 3_000) {
            _ = FileManager.default.createFile(atPath: root.appendingPathComponent("file-\(i)").path, contents: Data(repeating: 1, count: i % 31))
        }
        let stage = try ResumableStage(cache: cache, root: root, current: StageCheckpoint(rootPath: root.path, identity: "test", eventID: 0))
        while try stage.advance(progress: { _ in }) {}
        let snapshot = stage.testPreviewSnapshot, store = DiskIndexStore(directory: cache)
        DiskIndexStore.testSearchIndexByteLimit = capped ? 3_160_000 : nil
        defer { DiskIndexStore.testSearchIndexByteLimit = nil }
        let prepared = try store.prepareStagedSearch(snapshot: snapshot)
        precondition(prepared && store.isStagedSearchPrepared(snapshot: snapshot))
        precondition(store.testSearchIndex(snapshot: snapshot, staged: true)!.complete != capped)
        var db: OpaquePointer?
        precondition(sqlite3_open_v2(snapshot.databaseURL.path, &db, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK)
        defer { sqlite3_close(db) }
        func sql(_ text: String) { precondition(sqlite3_exec(db, text, nil, nil, nil) == SQLITE_OK, String(cString: sqlite3_errmsg(db))) }
        func verify(_ query: String, removals: [ScanRemoval] = []) throws {
            for limit in [0, 1, 300] {
            DiskIndexStore.testDisablePreparedSearch = true
            let reference = try store.stagedSearch(query: query, snapshot: snapshot, revision: 0, removals: removals, cancellation: ScanCancellation(), limit: limit)
            DiskIndexStore.testDisablePreparedSearch = false
            let actual = try store.stagedSearch(query: query, snapshot: snapshot, revision: 0, removals: removals, cancellation: ScanCancellation(), limit: limit)
            precondition(actual.count == reference.count && actual.nodes.map(Signature.init) == reference.nodes.map(Signature.init), "staged mismatch \(query)")
            }
        }
        for query in ["file", "file-1", "absent", "file\0tail"] { try verify(query) }
        for invalid in ["-1", "'-1x'"] {
        sql("UPDATE nodes SET allocated=\(invalid) WHERE name='file-0'")
        for limit in [0, 1, 300] {
            do {
                _ = try store.stagedSearch(query: "file", snapshot: snapshot, revision: 0, removals: [], cancellation: ScanCancellation(), limit: limit)
                fatalError("prepared broad search missed damaged discarded row")
            } catch DiskIndexError.invalid {}
        }
        }
        sql("UPDATE nodes SET allocated=0 WHERE name='file-0'")
        sql("UPDATE nodes SET size='200x' WHERE name='file-0'")
        try verify("file")
        sql("UPDATE nodes SET size=0 WHERE name='file-0'")
        let highWater = store.testSearchIndex(snapshot: snapshot, staged: true)!.highWater
        sql("DELETE FROM nodes WHERE id=\(highWater)")
        sql("INSERT INTO nodes(parent,name,search_name,size,file_count,dev,inode) VALUES(1,'rare-needle-tail','rare-needle-tail',9999,1,1,9000001)")
        var lastID: Int64 = sqlite3_last_insert_rowid(db)
        precondition(lastID > highWater)
        try verify("rare-needle"); try verify("file")
        sql("UPDATE nodes SET name='renamed-hit',search_name='renamed-hit',size=99999 WHERE id=\(highWater - 1)")
        try verify("renamed-hit"); try verify("file")
        let renewed = try store.prepareStagedSearch(snapshot: snapshot); precondition(renewed)
        try verify("renamed-hit")
        sql("DELETE FROM nodes WHERE id=\(lastID)")
        sql("INSERT INTO nodes(parent,name,search_name,size,file_count,dev,inode) VALUES(1,'replacement-needle','replacement-needle',99999,1,1,9000002)")
        precondition(sqlite3_last_insert_rowid(db) > lastID); lastID = sqlite3_last_insert_rowid(db)
        try verify("replacement-needle"); try verify("rare-needle")
        let file = try stage.search(query: "file-1", cancellation: ScanCancellation(), removals: [], limit: 1).nodes.first!
        try verify("file", removals: [ScanRemoval(path: file.id, identity: file.fileIdentity!)])
    }
}
