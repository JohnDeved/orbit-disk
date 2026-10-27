import Foundation

struct PendingInventoryBatchIndex {
    let offsets: [Data: Int]
    let linkedFiles: [FileMetadata]
    let estimatedByteCount: Int

    init(records: [FileMetadata]) {
        var offsets: [Data: Int] = [:]
        offsets.reserveCapacity(records.count)
        var linked: [FileMetadata] = []
        var bytes = 0
        for (offset, record) in records.enumerated() {
            offsets[Data(record.name.utf8)] = offset
            if !record.isDirectory, record.linkCount > 1 { linked.append(record) }
            bytes += 96 + record.name.utf8.count
        }
        self.offsets = offsets
        self.linkedFiles = linked
        self.estimatedByteCount = bytes + linked.count * 96
    }
}

final class PendingInventoryRead {
    struct Delta {
        var size: Int64 = 0
        var allocated: Int64 = 0
        var count: Int64 = 0
    }

    private let snapshot: PendingInventorySnapshot
    private let batches: [PendingInventoryBatch]
    private let byParent: [Data: [PendingInventoryBatch]]
    private let completed: Set<Data>
    private var deltas: [Data: Delta] = [:]
    private var affectedParents = Set<Data>()
    private var allocations: [Data: Int64] = [:]
    private var hasAllocationChanges = false
    private let root: Data
    private let rootDevice: Int32
    var isEmpty: Bool { batches.isEmpty && !hasAllocationChanges }

    init(snapshot: PendingInventorySnapshot, committedSequence: Int64,
         durableLinks: (DirectoryIdentity) throws -> [DiskNode], check: () throws -> Void) throws {
        guard snapshot.isCurrent() else { throw CancellationError() }
        self.snapshot = snapshot
        self.batches = snapshot.batches.filter { $0.sequence > committedSequence && $0.failure == nil }
        self.byParent = Dictionary(grouping: batches, by: \.parentPath)
        self.completed = Set(batches.lazy.filter { $0.completion != nil }.map(\.parentPath))
        self.root = snapshot.rootPath
        self.rootDevice = snapshot.rootIdentity.device
        var links: [DirectoryIdentity: [(Data, FileMetadata)]] = [:]
        for (offset, batch) in snapshot.batches.enumerated() {
            if offset & 63 == 0 { try check() }
            for record in batch.index.linkedFiles {
                links[DirectoryIdentity(device: record.device, inode: record.inode)] = []
            }
        }
        for (offset, batch) in batches.enumerated() {
            if offset & 63 == 0 { try check() }
            adjustAncestors(batch.parentPath, size: batch.logicalSize, allocated: batch.allocatedSize, count: batch.fileCount)
            for record in batch.index.linkedFiles {
                let path = HardLinkPathOrder.path(batch.parentPath, Data(record.name.utf8))
                links[DirectoryIdentity(device: record.device, inode: record.inode), default: []].append((path, record))
            }
        }
        for (identity, pending) in links {
            try check()
            let durable = try durableLinks(identity)
            var winner = pending.first?.0
            for (path, _) in pending.dropFirst() {
                if winner == nil || HardLinkPathOrder.precedes(path, winner!) { winner = path }
            }
            for node in durable {
                let path = Data(node.id.utf8)
                if winner == nil || HardLinkPathOrder.precedes(path, winner!) { winner = path }
            }
            for (path, record) in pending {
                let allocation = path == winner ? record.allocatedSize : 0
                allocations[path] = allocation
                adjustAncestors(Self.parent(path), allocated: allocation - record.allocatedSize)
            }
            for node in durable {
                let path = Data(node.id.utf8)
                let allocation = path == winner ? node.rawAllocatedSize : 0
                allocations[path] = allocation
                if allocation != node.allocatedSize { hasAllocationChanges = true }
                adjustAncestors(Self.parent(path), allocated: allocation - node.allocatedSize)
            }
        }
        try validate()
    }

    func validate() throws {
        guard snapshot.isCurrent() else { throw CancellationError() }
    }

    private func adjustAncestors(_ parent: Data, size: Int64 = 0, allocated: Int64 = 0, count: Int64 = 0) {
        guard size != 0 || allocated != 0 || count != 0 else { return }
        var current = parent
        while current == root || Self.isDescendant(current, of: root) {
            var delta = deltas[current] ?? Delta()
            delta.size += size; delta.allocated += allocated; delta.count += count
            deltas[current] = delta
            affectedParents.insert(Self.parent(current))
            if current == root { break }
            let next = Self.parent(current)
            if next == current { break }
            current = next
        }
    }

    func changesChildrenOrder(_ parent: String) -> Bool {
        let parent = Data(parent.utf8)
        return byParent[parent] != nil || affectedParents.contains(parent)
    }

    private func record(_ path: Data) -> FileMetadata? {
        let parent = Self.parent(path)
        guard let slash = path.lastIndex(of: 47), let batches = byParent[parent] else { return nil }
        let name = path[path.index(after: slash)...]
        for batch in batches.reversed() {
            if let offset = batch.index.offsets[Data(name)] { return batch.records[offset] }
        }
        return nil
    }

    func node(_ path: String, durable: DiskNode?) -> DiskNode? {
        let key = Data(path.utf8)
        return node(path, key: key, record: record(key), durable: durable)
    }

    func adjustDurable(_ node: DiskNode, path: String? = nil) -> DiskNode {
        let path = path ?? node.id
        return self.node(path, key: Data(path.utf8), record: nil, durable: node)!
    }

    private func node(_ path: String, key: Data, record: FileMetadata?, durable: DiskNode?) -> DiskNode? {
        let value: DiskNode
        if let record {
            let accessible = !record.isDirectory || (!record.isMountPoint && record.device == rootDevice)
            let state: NodeScanState = !record.isDirectory || !accessible ? .complete : .pending
            value = DiskNode(url: URL(fileURLWithPath: path, isDirectory: record.isDirectory), name: record.name,
                isDirectory: record.isDirectory, isSymbolicLink: record.isSymbolicLink,
                size: record.isDirectory ? 0 : record.size, allocatedSize: record.isDirectory ? 0 : record.allocatedSize,
                fileCount: record.isDirectory ? 0 : 1, isAccessible: accessible,
                rawAllocatedSize: record.isDirectory ? 0 : record.allocatedSize,
                fileIdentity: "\(record.device):\(record.inode)", scanState: state)
        } else if let durable { value = durable }
        else { return nil }
        let delta = value.isDirectory ? deltas[key] ?? Delta() : Delta()
        var state = value.scanState
        if value.isDirectory, byParent[key] != nil {
            state = completed.contains(key) ? .complete : .scanning
        } else if !value.isDirectory, byParent[Self.parent(key)] != nil {
            state = completed.contains(Self.parent(key)) ? .complete : .pending
        }
        return DiskNode(url: value.url, name: value.name, isDirectory: value.isDirectory, isSymbolicLink: value.isSymbolicLink,
            size: value.size + delta.size, allocatedSize: (allocations[key] ?? value.allocatedSize) + delta.allocated,
            fileCount: value.fileCount + Int(delta.count), children: value.children, isAccessible: value.isAccessible,
            rawAllocatedSize: value.rawAllocatedSize, fileIdentity: value.fileIdentity, scanState: state)
    }

    func children(_ parent: String, durable: [DiskNode], limit: Int, check: () throws -> Void) throws -> [DiskNode] {
        var children: [Data: DiskNode] = [:]
        children.reserveCapacity(durable.count)
        var inspected = 0
        for child in durable {
            if inspected & 255 == 0 { try check() }; inspected += 1
            children[Data(child.id.utf8)] = adjustDurable(child)
        }
        for batch in byParent[Data(parent.utf8)] ?? [] {
            for record in batch.records {
                if inspected & 255 == 0 { try check() }; inspected += 1
                let path = HardLinkPathOrder.path(batch.parentPath, Data(record.name.utf8))
                let text = String(decoding: path, as: UTF8.self)
                children[path] = node(text, key: path, record: record, durable: nil)
            }
        }
        try check()
        let ordered = children.values.sorted { $0.size == $1.size ? $0.name < $1.name : $0.size > $1.size }
        try check()
        return limit >= ordered.count ? ordered : Array(ordered.prefix(limit))
    }

    func forEachMatching(_ needle: String, check: () throws -> Void, body: (DiskNode) throws -> Void) throws {
        var inspected = 0
        for batch in batches {
            for record in batch.records {
                if inspected & 255 == 0 { try check() }; inspected += 1
                let folded = record.name.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: Locale(identifier: "en_US_POSIX"))
                guard folded.range(of: needle, options: .literal) != nil else { continue }
                let path = HardLinkPathOrder.path(batch.parentPath, Data(record.name.utf8))
                if let node = node(String(decoding: path, as: UTF8.self), key: path, record: record, durable: nil) { try body(node) }
            }
        }
    }

    static func parent(_ path: Data) -> Data {
        guard let slash = path.lastIndex(of: 47) else { return path }
        return slash == path.startIndex ? Data([47]) : path[..<slash]
    }

    private static func isDescendant(_ path: Data, of parent: Data) -> Bool {
        parent == Data([47]) ? path.first == 47 : path.count > parent.count && path.starts(with: parent) && path[path.startIndex + parent.count] == 47
    }
}
