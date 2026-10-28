import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

enum MetadataBookkeepingChecks {
    static func run(_ base: URL) throws {
        try accounting()
        try acceptedListings(base)
        print("PASS: fused metadata accounting, exclusion ordering and rejected-batch frontier rollback")
    }

    private static func accounting() throws {
        let identity = DirectoryIdentity(device: 1, inode: 2)
        let records = [
            FileMetadata(name: "empty", device: 1, inode: 3, isDirectory: false, isSymbolicLink: false,
                         isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0),
            FileMetadata(name: "が-é-quoted'file", device: 1, inode: 4, isDirectory: false, isSymbolicLink: false,
                         isMountPoint: false, linkCount: 2, size: 8193, allocatedSize: 12288),
            FileMetadata(name: String(repeating: "é", count: 127), device: 1, inode: 5,
                         isDirectory: false, isSymbolicLink: true, isMountPoint: false,
                         linkCount: 1, size: 17, allocatedSize: 0),
            FileMetadata(name: "folder", device: 1, inode: 6, isDirectory: true, isSymbolicLink: false,
                         isMountPoint: false, linkCount: 4, size: 999, allocatedSize: 4096)
        ]
        for input in [[], records] {
            let parent = Data("/fixture/é".utf8)
            let components = [DirectoryComponent(name: "é", expected: identity)]
            let issues = ["Unavailable: が", "Renamed"]
            let batch = PendingInventoryBatch(sequence: 1, parentPath: parent, parentIdentity: identity,
                components: components, records: input,
                completion: PendingListingCompletion(issueCount: 2, issues: issues), failure: nil)
            let files = input.filter { !$0.isDirectory }
            let directories = input.filter(\.isDirectory)
            let linked = files.filter { $0.linkCount > 1 }
            let expectedIndexBytes = input.reduce(0) { $0 + 96 + $1.name.utf8.count }
                + (linked.count + directories.count) * 96
            let expectedBytes = 256 + parent.count + components.reduce(0) { $0 + 64 + $1.name.utf8.count }
                + input.reduce(0) { $0 + 160 + $1.name.utf8.count * 2 }
                + expectedIndexBytes + issues.reduce(0) { $0 + 32 + $1.utf8.count }
            try require(batch.logicalSize == files.reduce(0) { $0 + $1.size }
                && batch.allocatedSize == files.reduce(0) { $0 + $1.allocatedSize }
                && batch.fileCount == Int64(files.count), "Fused metadata accounting changed file totals")
            try require(batch.byteCount == expectedBytes && batch.index.estimatedByteCount == expectedIndexBytes,
                        "Fused metadata accounting changed backlog byte estimates")
            try require(batch.index.linkedFiles.map(\.inode) == linked.map(\.inode)
                && batch.index.directories.map(\.inode) == directories.map(\.inode),
                "Fused metadata accounting lost directory or hardlink records")
            for (offset, record) in input.enumerated() {
                try require(batch.index.offsets[Data(record.name.utf8)] == offset,
                            "Fused metadata accounting changed filename lookup")
            }
        }
    }

    private static func acceptedListings(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "metadata-acceptance")
        let first = try fixture.file("first"), last = try fixture.file("last")
        let kept = try fixture.directory("kept"), deferred = try fixture.directory("deferred")
        let excludedPath = try fixture.directory("excluded-path")
        let excludedIdentity = try fixture.directory("excluded-identity")
        for name in ["kept", "deferred", "excluded-path", "excluded-identity"] {
            try fixture.file("\(name)/leaf")
        }
        let ordered = try [first, kept, excludedPath, deferred, excludedIdentity, last].map(FileMetadata.read)
        let root = try SecureRoot(url: fixture.root)
        let rootPath = Data(fixture.root.path.utf8)
        let excluded = try FileMetadata.read(excludedIdentity)
        for filtered in [false, true] {
            AsyncListingProducer.testRecords = { path, records in path == rootPath ? ordered : records }
            defer { AsyncListingProducer.testRecords = nil }
            let signals = Signals()
            let producer = try AsyncListingProducer(rootURL: fixture.root, rootIdentity: root.identity,
                cancellation: ScanCancellation(),
                excludedDirectories: filtered ? [DirectoryIdentity(device: excluded.device, inode: excluded.inode)] : [],
                excludedPaths: filtered ? [Data(excludedPath.path.utf8)] : [],
                deferDirectory: filtered ? { $0 == Data(deferred.path.utf8) } : nil,
                preferredDirectory: nil, onAccepted: { signals.signal() })
            defer { producer.cancel() }
            producer.start()
            try signals.until("metadata acceptance") { producer.mailbox.discoveryFinished }
            let batches = producer.mailbox.capture().batches
            let actual = batches.filter { $0.parentPath == rootPath }.flatMap(\.records).map(\.name)
            let expected = filtered ? ["first", "kept", "deferred", "last"] : ordered.map(\.name)
            try require(actual == expected, "Metadata filtering changed accepted record order")
            if filtered {
                let parents = Set(batches.map(\.parentPath))
                try require(parents == [rootPath, Data(kept.path.utf8)]
                    && producer.mailbox.reuseDeferredDirectoryCount == 1,
                    "Excluded or deferred directories entered discovery")
            }
        }

        AsyncListingProducer.testRecords = { path, records in
            path == rootPath ? [ordered[1], ordered[3], ordered[1]] : records
        }
        defer { AsyncListingProducer.testRecords = nil }
        let signals = Signals()
        let producer = try AsyncListingProducer(rootURL: fixture.root, rootIdentity: root.identity,
            cancellation: ScanCancellation(), excludedDirectories: [], excludedPaths: [],
            deferDirectory: { $0 == Data(deferred.path.utf8) }, preferredDirectory: nil,
            onAccepted: { signals.signal() })
        defer { producer.cancel() }
        producer.start()
        try signals.until("duplicate directory rollback") { producer.mailbox.discoveryFinished }
        let batches = producer.mailbox.capture().batches
        try require(batches.allSatisfy { $0.parentPath == rootPath && $0.records.isEmpty }
            && producer.mailbox.retryDeferredDirectoryCount == 1
            && producer.mailbox.reuseDeferredDirectoryCount == 0,
            "A rejected duplicate batch left queued or deferred directories behind")
    }
}
