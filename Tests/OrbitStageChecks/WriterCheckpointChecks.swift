import CSQLite
import Foundation

enum WriterCheckpointChecks {
    static func run(_ base: URL) throws {
        let root = base.appendingPathComponent("root", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let checkpoint = StageCheckpoint(rootPath: root.path, identity: "writer-checkpoint", eventID: 0)
        let database = base.appendingPathComponent("stage.sqlite")
        var activeReaders: Set<String> = []
        var commits = 0
        ListingStage.testCommitReaders = { readers in
            commits += 1
            activeReaders.formUnion(readers)
        }
        defer { ListingStage.testCommitReaders = nil }
        let crawler = try DurableCrawler(rootURL: root, database: database.path, checkpoint: checkpoint, mirror: true)
        let store = crawler.store, device = crawler.root.identity.device
        try store.prepareQueueIndexes(cancellation: crawler.cancellation)
        let count = 1_000, filesPerDirectory = 10
        let directories = (0..<count).map {
            FileMetadata(name: "d-\($0)", device: device, inode: UInt64($0 + 100), isDirectory: true,
                         isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0)
        }
        let rootFinished = try store.completeFreshListing(parent: crawler.rootPath, records: directories,
            rootDevice: device, expectedRevision: 0, expectedFullGeneration: 0)
        precondition(rootFinished)
        func directoryPath(_ index: Int) -> Data { Data(root.appendingPathComponent("d-\(index)").path.utf8) }
        func files(_ index: Int, count: Int = 10, size: Int64 = 64, allocation: Int64 = 4_096) -> [FileMetadata] {
            (0..<count).map { file in
                let shared = index < 2 && file == 0
                return FileMetadata(name: "f-\(file)", device: device,
                    inode: shared ? 999_999 : UInt64(1_000_000 + index * filesPerDirectory + file),
                    isDirectory: false, isSymbolicLink: false, isMountPoint: false,
                    linkCount: shared ? 2 : 1, size: size, allocatedSize: allocation)
            }
        }
        func walBytes() throws -> Int64 {
            let attributes = try FileManager.default.attributesOfItem(atPath: database.path + "-wal")
            return (attributes[.size] as! NSNumber).int64Value
        }
        var maximumWAL: Int64 = 0
        for index in 0..<count {
            let completed = try store.completeFreshListing(parent: directoryPath(index), records: files(index),
                rootDevice: device, expectedRevision: 0, expectedFullGeneration: 0,
                asyncCommit: AsyncInventoryCommit(generation: "checkpoint-fixture", sequence: Int64(index + 1)))
            precondition(completed)
            while try store.reconcileLinksChunk() {}
            maximumWAL = max(maximumWAL, try walBytes())
        }
        // No test checkpoint runs here: the writer's 16,384-page checkpoints must recycle the log.
        precondition(maximumWAL < 80 * 1_024 * 1_024,
                     "Writer WAL grew to \(maximumWAL) bytes without any external reader")
        let initialTotals = try store.folderTotals(path: crawler.rootPath)
        precondition(initialTotals?.count == 10_000 && initialTotals?.logical == 640_000
                     && initialTotals?.allocated == 9_999 * 4_096)

        for index in 0..<10 {
            let parent = directoryPath(index), epoch = try store.begin(parent: parent)
            try store.append(parent: parent, epoch: epoch, records: files(index, count: 5, size: 128, allocation: 8_192), rootDevice: device)
            let finished = try store.finish(parent: parent, epoch: epoch, expectedRevision: 0, expectedFullGeneration: 0)
            precondition(finished)
            while try store.trimOneChunk() {}
            while try store.reconcileLinksChunk() {}
        }
        for index in 10..<20 {
            try store.startPrune(path: directoryPath(index), parent: crawler.rootPath, name: Data("d-\(index)".utf8))
            while try store.pruneOneChunk() {}
            while try store.reconcileLinksChunk() {}
        }
        let totals = try store.folderTotals(path: crawler.rootPath)
        precondition(totals?.count == 9_850 && totals?.logical == 633_600
                     && totals?.allocated == 9_800 * 4_096 + 49 * 8_192)
        let snapshot = try store.sealMirror(rootURL: root, checkpoint: checkpoint, elapsed: 0)
        precondition(commits > count && activeReaders.isEmpty,
                     "Reader statements survived until writer COMMIT: \(activeReaders.sorted())")
        store.close()

        let reopened = try ListingStage(path: database.path, mirror: true)
        defer { reopened.close() }
        let durableTotals = try reopened.folderTotals(path: crawler.rootPath)
        precondition(durableTotals?.count == totals?.count && durableTotals?.logical == totals?.logical
                     && durableTotals?.allocated == totals?.allocated)
        let read = try DiskIndexStore(directory: base).loadFolder(path: root.path, snapshot: snapshot)
        precondition(read.fileCount == 9_850 && read.size == 633_600 && read.children.count == 990)
        print("PASS: writer autocheckpoints recycle WAL (peak \(maximumWAL) bytes), commits release readers, and reopened totals agree")
    }
}
