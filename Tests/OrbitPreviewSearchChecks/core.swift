import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
    private let lock = NSLock(), releaseSignal = DispatchSemaphore(value: 0)
    private var state = false, used = false
    var waiting: Bool { lock.lock(); defer { lock.unlock() }; return state }
    func pause() { lock.lock(); let block = !used; used = true; if block { state = true }; lock.unlock(); if block { releaseSignal.wait() } }
    func release() { releaseSignal.signal() }
}
@main struct Test {
    static func main() async throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1]), child = base.appendingPathComponent("Folder")
        try FileManager.default.createDirectory(at: child, withIntermediateDirectories: true)
        try Data(repeating: 1, count: 9).write(to: base.appendingPathComponent("CAFÉ.txt"))
        try Data(repeating: 1, count: 3).write(to: base.appendingPathComponent("cafe-small.txt"))
        try Data(repeating: 1, count: 5).write(to: child.appendingPathComponent("cafe-nested.txt"))
        let preview = DiskScanPreview(), gate = Gate(), token = ScanCancellation()
        let scan = Task.detached { try DiskScanner().scan(base, cancellation: token, preview: preview) { progress in if progress.currentPath == base.path && progress.itemCount > 0 { gate.pause() } } }
        for _ in 0..<500 { if gate.waiting { break }; try await Task.sleep(nanoseconds: 10_000_000) }
        precondition(gate.waiting)
        let first = try preview.search(query: "CAFE", cancellation: token)
        precondition(first.count >= 2 && first.nodes.prefix(2).map(\.size) == [9, 3])
        gate.release(); _ = try await scan.value
        let result = try preview.search(query: "cafe", cancellation: token)
        precondition(result.count == 3 && result.nodes.map(\.size) == [9, 5, 3])
        let folder = try preview.view(path: base.path, cancellation: token)!.folder.children.first { $0.id == child.path }!
        let removed = try preview.search(query: "cafe", cancellation: token, removals: [ScanRemoval(path: child.path, identity: folder.fileIdentity!)])
        precondition(removed.count == 2 && !removed.nodes.contains { $0.id.hasPrefix(child.path + "/") })
        let nested = try preview.view(path: child.path, cancellation: token)!.folder.children.first!
        let projected = try preview.search(query: "folder", cancellation: token, removals: [ScanRemoval(path: nested.id, identity: nested.fileIdentity!)])
        precondition(projected.nodes.count == 1 && projected.nodes[0].size == 0 && projected.nodes[0].fileCount == 0)
        let limited = try preview.search(query: "cafe", cancellation: token, limit: 1)
        precondition(limited.count == 3 && limited.nodes.map(\.size) == [9])
        let rootChildren = try preview.view(path: base.path, cancellation: token)!.folder.children
        let oldFile = rootChildren.first { $0.name == "CAFÉ.txt" }!
        let replacement = DiskNode(url: oldFile.url, name: oldFile.name, isDirectory: false, size: 17, allocatedSize: 17, fileCount: 1, fileIdentity: "replacement")
        let replacementGate = Gate(); preview.testAfterSearchRemovals = { replacementGate.pause() }
        let pending = Task.detached { try preview.search(query: "cafe", cancellation: token, removals: [ScanRemoval(path: oldFile.id, identity: oldFile.fileIdentity!)]) }
        for _ in 0..<500 { if replacementGate.waiting { break }; try await Task.sleep(nanoseconds: 10_000_000) }
        precondition(replacementGate.waiting)
        preview.testReplaceListing(base.path, children: rootChildren.map { $0.id == oldFile.id ? replacement : $0 })
        replacementGate.release()
        let changed = try await pending.value
        preview.testAfterSearchRemovals = nil
        precondition(changed.removalsChanged, "Search must discard a projection when a tombstoned path is replaced")
        let retried = try preview.search(query: "cafe", cancellation: token, removals: [ScanRemoval(path: oldFile.id, identity: oldFile.fileIdentity!)])
        precondition(retried.nodes.contains { $0.id == oldFile.id && $0.fileIdentity == "replacement" })
        let unicodeRoot = base.appendingPathComponent("unicode-root"), unicodePreview = DiskScanPreview()
        let names = ["x-é", "y-e\u{301}", "x-नमस्ते", "y-が", "z-か\u{3099}"]
        let unicodeNodes = names.enumerated().map { index, name in DiskNode(url: unicodeRoot.appendingPathComponent(name), name: name, isDirectory: false, size: Int64(index + 1), allocatedSize: 1, fileCount: 1, fileIdentity: "1:\(index + 1)") }
        let unicodeTree = DiskNode(url: unicodeRoot, name: "unicode-root", isDirectory: true, size: 15, allocatedSize: 5, fileCount: 5, children: unicodeNodes, fileIdentity: "2:1")
        unicodePreview.testBegin(unicodeTree);unicodePreview.testReplaceListing(unicodeRoot.path, children: unicodeNodes)
        let unicodeStore = DiskIndexStore(directory: base.appendingPathComponent("unicode-index"))
        let unicodeSnapshot = try unicodeStore.save(ScanResult(root: unicodeTree, elapsed: 0, skippedCount: 0, issues: []), eventID: 0, eventIdentity: "test")
        for query in ["é", "e\u{301}", "नम", "が", "か\u{3099}"] {
            let partial = try unicodePreview.search(query: query, cancellation: token)
            let saved = try unicodeStore.search(query: query, snapshot: unicodeSnapshot)
            precondition(Set(partial.nodes.map(\.id)) == Set(saved.nodes.map(\.id)), "Unicode matching differs for \(query)")
        }
        let tieRoot = base.appendingPathComponent("tie-root"), tiePreview = DiskScanPreview()
        let tieFolders = ["a-é", "b-e\u{301}", "c-न", "d-が"].enumerated().map { folderIndex, folderName -> DiskNode in
            let folderURL = tieRoot.appendingPathComponent(folderName)
            let files = (0..<110).map { i in DiskNode(url: folderURL.appendingPathComponent(String(format: "match-%03d", i)), name: String(format: "match-%03d", i), isDirectory: false, size: 1, allocatedSize: 1, fileCount: 1, fileIdentity: "3:\(folderIndex * 110 + i + 1)") }
            return DiskNode(url: folderURL, name: folderName, isDirectory: true, size: 110, allocatedSize: 110, fileCount: 110, children: files, fileIdentity: "4:\(folderIndex + 1)")
        }
        let tieTree = DiskNode(url: tieRoot, name: "tie-root", isDirectory: true, size: 440, allocatedSize: 440, fileCount: 440, children: tieFolders, fileIdentity: "4:100")
        tiePreview.testBegin(tieTree);tiePreview.testReplaceListing(tieRoot.path, children: tieFolders)
        for folder in tieFolders { tiePreview.testReplaceListing(folder.id, children: folder.children) }
        let tieStore = DiskIndexStore(directory: base.appendingPathComponent("tie-index"))
        let tieSaved = try tieStore.save(ScanResult(root: tieTree, elapsed: 0, skippedCount: 0, issues: []), eventID: 0, eventIdentity: "test")
        let tiePartial = try tiePreview.search(query: "match", cancellation: token)
        let tieIndexed = try tieStore.search(query: "match", snapshot: tieSaved, stablePathOrder: true)
        precondition(tiePartial.count == 440 && tieIndexed.totalCount == 440 && tiePartial.nodes.count == 300)
        precondition(tiePartial.nodes.map { Array($0.id.utf8) } == tieIndexed.nodes.map { Array($0.id.utf8) }, "Partial and saved search must retain byte-identical tie ordering and cutoff")
        token.cancel()
        do { _ = try preview.search(query: "cafe", cancellation: token); preconditionFailure("search ignored cancellation") } catch is CancellationError {}
        print("PASS: progressive search, Unicode, size ranking, result cap, subtree removal and cancellation")
    }
}
