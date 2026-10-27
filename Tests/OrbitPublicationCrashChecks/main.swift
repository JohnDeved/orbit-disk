import Darwin
import Foundation

private final class RefreshCounter: @unchecked Sendable { var count = 0 }

@main struct PublicationCrashChecks {
    static func main() throws {
        let args = CommandLine.arguments
        if args.count == 4 && args[1] == "recovery-child" {
            let root = URL(fileURLWithPath: args[2], isDirectory: true)
            let store = DiskIndexStore(directory: URL(fileURLWithPath: args[3], isDirectory: true))
            guard let saved = store.savedSnapshot(for: root) else { throw NSError(domain: "missing snapshot", code: 1) }
            DiskIndexStore.testBeforeUpdatePublish = { _ = kill(getpid(), SIGKILL) }
            _ = try store.update(snapshot: saved, replacements: [IndexReplacement(path: root.appendingPathComponent("a").path, result: nil)],
                                 eventID: 2, eventIdentity: "fixture")
            fatalError("expected SIGKILL")
        }
        if args.count == 6 && args[1] == "child" {
            let root = URL(fileURLWithPath: args[2], isDirectory: true)
            let base = URL(fileURLWithPath: args[3], isDirectory: true)
            let checkpoint = try JSONDecoder().decode(StageCheckpoint.self, from: Data(base64Encoded: args[4])!)
            let stage = try DurableCrawler(rootURL: root, database: base.appendingPathComponent("stage.sqlite").path, checkpoint: checkpoint, mirror: true)
            let cache = base.appendingPathComponent("cache")
            let expected = try DiskIndexStore(directory: cache).captureExpectedSnapshot(for: root)
            _ = try MirrorPublisher.publish(stage, checkpoint: checkpoint, directory: cache, elapsed: 1, expected: expected, boundary: { boundary in
                if boundary == args[5] { kill(getpid(), SIGKILL) }
            })
            fatalError("expected SIGKILL")
        }
        let fm = FileManager.default
        let base = fm.temporaryDirectory.appendingPathComponent("orbit-publish-crash-\(UUID().uuidString)", isDirectory: true)
        defer { try? fm.removeItem(at: base) }
        let root = base.appendingPathComponent("root", isDirectory: true)
        let cache = base.appendingPathComponent("cache", isDirectory: true)
        try fm.createDirectory(at: root, withIntermediateDirectories: true)
        let stageFile = base.appendingPathComponent("stage.sqlite")
        let descriptor = Darwin.open(stageFile.path, O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else { throw NSError(domain: "create private stage", code: Int(errno)) }
        close(descriptor)
        try Data([1]).write(to: root.appendingPathComponent("old"))
        let raw = try DiskChangeMonitor.capture(root: root)
        let cp = StageCheckpoint(rootPath: raw.rootPath, identity: raw.identity, eventID: raw.eventID)
        let store = DiskIndexStore(directory: cache)
        let old = try store.save(DiskScanner().scan(root), eventID: cp.eventID, eventIdentity: cp.identity)
        try Data([1,2]).write(to: root.appendingPathComponent("new"))
        do {
            let stage = try DurableCrawler(rootURL: root, database: base.appendingPathComponent("stage.sqlite").path, checkpoint: cp, mirror: true)
            while try stage.step() { }
        }
        let encoded = try JSONEncoder().encode(cp).base64EncodedString()
        func crash(_ at: String) throws {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: args[0]).standardizedFileURL
            p.arguments = ["child", root.path, base.path, encoded, at]
            try p.run()
            p.waitUntilExit()
            guard p.terminationReason == .uncaughtSignal, p.terminationStatus == SIGKILL else { throw NSError(domain: "child did not die at \(at)", code: 1) }
        }
        try crash("cloned")
        guard let afterClone = store.savedSnapshot(for: root), afterClone.id == old.id,
              Set(try store.loadFolder(path: root.path, snapshot: afterClone).children.map(\.name)) == ["old"] else { throw NSError(domain: "clone crash lost old snapshot", code: 1) }
        try crash("renamed")
        guard let afterRename = store.savedSnapshot(for: root), afterRename.id != old.id,
              afterRename.eventID == cp.eventID, afterRename.eventIdentity == cp.identity,
              Set(try store.loadFolder(path: root.path, snapshot: afterRename).children.map(\.name)) == ["old", "new"] else { throw NSError(domain: "rename crash lost new snapshot", code: 1) }
        print("PASS: SIGKILL after clone preserves old snapshot; SIGKILL after rename leaves complete new snapshot with original checkpoint")
        try checkStreamingRecovery(base: base.appendingPathComponent("streaming"), executable: URL(fileURLWithPath: args[0]).standardizedFileURL)
    }

    private static func checkStreamingRecovery(base: URL, executable: URL) throws {
        let root = base.appendingPathComponent("root", isDirectory: true)
        let a = root.appendingPathComponent("a", isDirectory: true)
        let z = root.appendingPathComponent("z", isDirectory: true)
        let cache = base.appendingPathComponent("cache", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let count = 2_048
        let total = Int64(count) * 4_096
        func leaf(_ parent: URL, _ index: Int, _ allocated: Int64) -> DiskNode {
            let name = "file-\(index)"
            return DiskNode(url: parent.appendingPathComponent(name), name: name, isDirectory: false,
                            size: 4_096, allocatedSize: allocated, fileCount: 1,
                            rawAllocatedSize: 4_096, fileIdentity: "1:\(index)")
        }
        let owner = DiskNode(url: a, name: "a", isDirectory: true, size: total, allocatedSize: total,
                             fileCount: count, children: (0..<count).map { leaf(a, $0, 4_096) })
        let links = DiskNode(url: z, name: "z", isDirectory: true, size: total, allocatedSize: 0,
                             fileCount: count, children: (0..<count).map { leaf(z, $0, 0) })
        let tree = DiskNode(url: root, name: "root", isDirectory: true, size: total * 2, allocatedSize: total,
                            fileCount: count * 2, children: [owner, links])
        let store = DiskIndexStore(directory: cache)
        let saved = try store.save(ScanResult(root: tree, elapsed: 0, skippedCount: 0, issues: []),
                                   eventID: 1, eventIdentity: "fixture")
        let replacement = IndexReplacement(path: a.path, result: nil)
        let cancellation = ScanCancellation()
        let counter = RefreshCounter()
        DiskIndexStore.testDuringRefreshAncestors = {
            counter.count += 1
            if counter.count == 1_000 { cancellation.cancel() }
        }
        defer { DiskIndexStore.testDuringRefreshAncestors = nil }
        do {
            _ = try store.update(snapshot: saved, replacements: [replacement], eventID: 2,
                                 eventIdentity: "fixture", cancellation: cancellation)
            throw NSError(domain: "streaming cancellation did not interrupt", code: 1)
        } catch is CancellationError { }
        guard counter.count == 1_000, store.savedSnapshot(for: root)?.id == saved.id else {
            throw NSError(domain: "streaming cancellation changed published snapshot", code: 1)
        }
        DiskIndexStore.testDuringRefreshAncestors = nil
        let child = Process()
        child.executableURL = executable
        child.arguments = ["recovery-child", root.path, cache.path]
        try child.run()
        child.waitUntilExit()
        guard child.terminationReason == .uncaughtSignal, child.terminationStatus == SIGKILL,
              let afterCrash = store.savedSnapshot(for: root), afterCrash.id == saved.id,
              try store.node(path: root.path, snapshot: afterCrash)?.fileCount == count * 2 else {
            throw NSError(domain: "interrupted refresh changed published snapshot", code: 1)
        }
        let updated = try store.update(snapshot: saved, replacements: [replacement], eventID: 2, eventIdentity: "fixture")
        let remaining = try store.loadFolder(path: z.path, snapshot: updated)
        guard remaining.children.count == count, remaining.allocatedSize == total,
              remaining.children.allSatisfy({ $0.allocatedSize == $0.rawAllocatedSize && $0.allocatedSize == 4_096 }),
              let rootNode = try store.node(path: root.path, snapshot: updated),
              rootNode.allocatedSize == total, rootNode.fileCount == count else {
            throw NSError(domain: "hard-link allocation changed during streaming refresh", code: 1)
        }
        print("PASS: streaming hard-link refresh preserves the snapshot on cancellation and SIGKILL, then reallocates all owners")
    }
}
