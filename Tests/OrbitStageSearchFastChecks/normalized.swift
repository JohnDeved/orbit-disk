import Foundation
import OrbitCore
import CSQLite

private struct Signature: Equatable {
    let path: Data
    let name: String
    let identity: String?
    let size: Int64
    let allocated: Int64
    let rawAllocated: Int64
    let count: Int
    let directory: Bool
    let symbolicLink: Bool
    let accessible: Bool
    let state: NodeScanState

    init(_ node: DiskNode) {
        path = Data(node.id.utf8)
        name = node.name
        identity = node.fileIdentity
        size = node.size
        allocated = node.allocatedSize
        rawAllocated = node.rawAllocatedSize
        count = node.fileCount
        directory = node.isDirectory
        symbolicLink = node.isSymbolicLink
        accessible = node.isAccessible
        state = node.scanState
    }
}

@main struct Main {
    static func main() throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1])
        let root = base.appendingPathComponent("root")
        let cache = base.appendingPathComponent(UUID().uuidString)
        let stage = try ResumableStage(cache: cache, root: root,
            current: StageCheckpoint(rootPath: root.path, identity: "test", eventID: 0))
        stage.testDisableAsyncDiscovery = true
        while try stage.advance(progress: { _ in }) {}
        var db: OpaquePointer?
        precondition(sqlite3_open_v2(stage.testPreviewSnapshot.databaseURL.path, &db, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK)
        defer { sqlite3_close(db) }
        var update: OpaquePointer?
        precondition(sqlite3_prepare_v2(db, "UPDATE nodes SET name=? WHERE name=?", -1, &update, nil) == SQLITE_OK)
        defer { sqlite3_finalize(update) }
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        let punctuation = [" ", "!", "#", "%20", "%2F", "'", "(", ")", "+", ",", "-", ".", "..", ";", "=", "@", "[", "]", "_", "~", "\n", "\t", "\\", "💾"]
        for i in 0..<1300 {
            let suffix: String
            if i < 1200 {
                suffix = i.isMultiple(of: 2) ? "Å-\(i / 2)" : "A\u{30a}-\(i / 2)"
            } else {
                suffix = "\(punctuation[(i - 1200) % punctuation.count])-\(i)"
            }
            let name = String(format: "file-%04d", i)
            sqlite3_reset(update)
            sqlite3_clear_bindings(update)
            precondition(sqlite3_bind_text(update, 1, suffix, -1, transient) == SQLITE_OK)
            precondition(sqlite3_bind_text(update, 2, name, -1, transient) == SQLITE_OK)
            precondition(sqlite3_step(update) == SQLITE_DONE && sqlite3_changes(db) == 1)
        }
        let original = try stage.search(query: "file-1399", cancellation: ScanCancellation(), removals: [], limit: 1).nodes.first!
        let mismatch = [ScanRemoval(path: original.id, identity: "not-the-file-identity")]
        let removed = [ScanRemoval(path: original.id, identity: original.fileIdentity!)]
        for limit in [0, 1, 2, 5, 299, 300, 301, 599, 10_000] {
            let actual = try stage.search(query: "file", cancellation: ScanCancellation(), removals: [], limit: limit)
            // An unmatched overlay selects the original search path without changing its results.
            let reference = try stage.search(query: "file", cancellation: ScanCancellation(), removals: mismatch, limit: limit)
            precondition(actual.count == 1400 && actual.count == reference.count)
            precondition(actual.nodes.map(Signature.init) == reference.nodes.map(Signature.init))
            let overlay = try stage.search(query: "file", cancellation: ScanCancellation(), removals: removed, limit: limit)
            precondition(overlay.count == 1399 && !overlay.nodes.contains { $0.name == original.name })
            if limit == 300 {
                var identities: [Data: String] = [:]
                var normalizedCollision = false
                for node in actual.nodes {
                    let path = Data(node.id.utf8)
                    if let identity = identities[path], identity != node.fileIdentity { normalizedCollision = true }
                    identities[path] = node.fileIdentity
                }
                precondition(normalizedCollision, "fixture must retain equal normalized IDs with distinct identities")
            }
            if limit == 10_000 {
                precondition(actual.nodes.contains { $0.id.contains("%2F") })
                precondition(actual.nodes.contains { $0.id.contains("\n") })
                precondition(actual.nodes.contains { $0.id.contains("\t") })
            }
        }
        print("PASS: equal-size search retains normalized-equal IDs and punctuation, with unchanged overlays")
    }
}
