import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

enum BoundedChildrenChecks {
    private struct Stop: Error {}

    static func run(_ base: URL) throws {
        let root = base.appendingPathComponent("bounded-children", isDirectory: true)
        let rootPath = Data(root.path.utf8), identity = DirectoryIdentity(device: 1, inode: 1)
        func metadata(_ name: String, _ size: Int64, inode: UInt64 = 2, directory: Bool = false,
                      symbolic: Bool = false, mount: Bool = false, links: UInt32 = 1) -> FileMetadata {
            FileMetadata(name: name, device: 1, inode: inode, isDirectory: directory,
                isSymbolicLink: symbolic, isMountPoint: mount, linkCount: links,
                size: size, allocatedSize: size + 4096)
        }
        func durable(_ name: String, _ size: Int64, inode: UInt64 = 3, directory: Bool = false) -> DiskNode {
            DiskNode(url: root.appendingPathComponent(name, isDirectory: directory), name: name,
                isDirectory: directory, size: size, allocatedSize: size + 4096, fileCount: directory ? 2 : 1,
                fileIdentity: "1:\(inode)", scanState: .pending)
        }
        var sequence: Int64 = 0
        func batch(_ records: [FileMetadata], parent: Data? = nil, complete: Bool = false) -> PendingInventoryBatch {
            sequence += 1
            return PendingInventoryBatch(sequence: sequence, parentPath: parent ?? rootPath,
                parentIdentity: identity, components: [], records: records,
                completion: complete ? PendingListingCompletion(issueCount: 0, issues: []) : nil, failure: nil)
        }
        var batches: [PendingInventoryBatch] = []
        for start in stride(from: 0, to: 600, by: 100) {
            batches.append(batch((start..<(start + 100)).map {
                metadata(String(format: "item-%03d", $0), Int64(($0 * 37) % 89), inode: UInt64(100 + $0))
            }))
        }
        batches.append(batch([
            metadata("same-batch", 900_000), metadata("same-batch", 3),
            metadata("replaced", 800_000), metadata("item-005", 700_000),
            metadata("folder", 0, inode: 800, directory: true),
            metadata("denied", 0, inode: 801, directory: true),
            metadata("mount", 0, inode: 802, directory: true, mount: true),
            metadata("symbolic", 13, inode: 803, symbolic: true),
            metadata("が-é-quoted'file", 17, inode: 804),
            metadata("link-a", 25_000, inode: 900, links: 3),
            metadata("link-b", 25_000, inode: 900, links: 3)
        ]))
        batches.append(batch([metadata("replaced", 1), metadata("item-005", 2),
                              metadata("item-006", 60_000)], complete: true))
        let folderPath = HardLinkPathOrder.path(rootPath, Data("folder".utf8))
        let durableFolderPath = HardLinkPathOrder.path(rootPath, Data("durable-folder".utf8))
        batches.append(batch([metadata("nested", 50_000, inode: 901)], parent: folderPath))
        batches.append(batch([metadata("nested", 40_000, inode: 902)], parent: durableFolderPath, complete: true))
        let durableLink = durable("link-z", 25_000, inode: 900)
        let saved = [durable("durable-only", 70_000), durable("replaced", 2_000_000),
                     durable("durable-repeat", 1_000_000), durable("durable-repeat", 4),
                     durable("durable-folder", 1_000, directory: true), durableLink]
        let snapshot = PendingInventorySnapshot(generation: "bounded-children", revision: 1,
            rootPath: rootPath, rootIdentity: identity, batches: batches,
            inaccessibleDirectories: [HardLinkPathOrder.path(rootPath, Data("denied".utf8))], current: { true })
        let inventory = try PendingInventoryRead(snapshot: snapshot, committedSequence: 0,
            durableLinks: { link in link.inode == 900 ? [durableLink] : [] }, check: {})
        let complete = try inventory.children(root.path, durable: saved, limit: Int.max, check: {})
        try require(complete.prefix(4).map(\.name) == ["durable-only", "item-006", "folder", "durable-folder"],
                    "Full-list reference did not include pending directory totals or replacements")
        try require(complete.first { $0.name == "link-a" }?.allocatedSize == 29_096
            && complete.first { $0.name == "link-b" }?.allocatedSize == 0
            && complete.first { $0.name == "link-z" }?.allocatedSize == 0,
            "Full-list reference did not reconcile hard-link owners")
        for limit in [0, 1, 4, 7, 32, 96, complete.count - 1, complete.count, complete.count + 10] {
            let actual = try inventory.children(root.path, durable: saved, limit: limit, check: {})
            let expected = Array(complete.prefix(limit))
            try require(actual.count == expected.count, "Bounded preview returned the wrong child count")
            for (left, right) in zip(actual, expected) {
                try same(left, right)
            }
        }
        for stopAt in [1, 2, 3] {
            var checks = 0
            do {
                _ = try inventory.children(root.path, durable: saved, limit: 96, check: {
                    checks += 1
                    if checks == stopAt { throw Stop() }
                })
                throw AsyncCheckFailure(description: "Bounded child selection ignored cancellation")
            } catch is Stop {}
        }
        print("PASS: bounded child previews preserve ordering, replacements, totals, hard links, metadata and cancellation")
    }

    private static func same(_ actual: DiskNode, _ expected: DiskNode) throws {
        try require(actual.id == expected.id && actual.name == expected.name
            && actual.size == expected.size && actual.allocatedSize == expected.allocatedSize
            && actual.rawAllocatedSize == expected.rawAllocatedSize && actual.fileCount == expected.fileCount
            && actual.fileIdentity == expected.fileIdentity && actual.scanState == expected.scanState
            && actual.isDirectory == expected.isDirectory && actual.isSymbolicLink == expected.isSymbolicLink
            && actual.isAccessible == expected.isAccessible && actual.children.map(\.id) == expected.children.map(\.id),
            "Bounded child metadata differs from the complete listing at \(expected.name)")
    }
}
