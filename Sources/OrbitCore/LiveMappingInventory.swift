import Foundation

// Live observations outlive their save-queue entries; UI reads never open the database.
final class LiveMappingInventory: @unchecked Sendable {
    private struct Listing {
        let identity: DirectoryIdentity
        var batches: [PendingInventoryBatch] = []
        var fileCount = 0
    }
    private let lock = NSLock()
    private let root: URL
    private let identity: DirectoryIdentity
    private let scope: VolumeTraversalScope
    private let cancellation: ScanCancellation
    private var listings: [Data: Listing] = [:]
    private var version: UInt64 = 0
    private var invalidation: UInt64 = 0
    private var inaccessible = Set<Data>()
    private var cached: (UInt64, PendingInventoryRead)?
    private var cachedFileCount: (revision: UInt64, count: Int)?
    var onChange: () -> Void = {}

    init(root: URL, identity: DirectoryIdentity, cancellation: ScanCancellation) {
        self.root = root; self.identity = identity; self.cancellation = cancellation
        scope = VolumeTraversalScope(rootURL: root, rootIdentity: identity)
    }

    func begin(_ path: Data, identity: DirectoryIdentity, notify: Bool = true) {
        lock.lock(); defer { lock.unlock(); if notify { onChange() } }
        if listings[path] != nil { invalidation &+= 1 }
        listings[path] = Listing(identity: identity)
        inaccessible.remove(path)
        version &+= 1
    }

    func append(_ batch: PendingInventoryBatch, notify: Bool = true) {
        guard batch.failure == nil else { return }
        lock.lock(); defer { lock.unlock(); if notify { onChange() } }
        if listings[batch.parentPath]?.identity != batch.parentIdentity {
            listings[batch.parentPath] = Listing(identity: batch.parentIdentity)
            invalidation &+= 1
        }
        listings[batch.parentPath]!.batches.append(batch)
        listings[batch.parentPath]!.fileCount += Int(batch.fileCount)
        version &+= 1
    }

    func unavailable(_ path: Data, identity: DirectoryIdentity, missing: Bool, notify: Bool = true) {
        lock.lock(); defer { lock.unlock(); if notify { onChange() } }
        if missing {
            let parent = PendingInventoryRead.parent(path)
            if var listing = listings[parent] {
                var fileCount = 0
                listing.batches = listing.batches.map { batch in
                    let kept = batch.records.filter {
                        HardLinkPathOrder.path(parent, Data($0.name.utf8)) != path
                            || $0.device != identity.device || $0.inode != identity.inode
                    }
                    guard kept.count != batch.records.count else { fileCount += Int(batch.fileCount); return batch }
                    let replacement = PendingInventoryBatch(sequence: batch.sequence, parentPath: parent,
                        parentIdentity: batch.parentIdentity, components: batch.components, records: kept,
                        completion: batch.completion, failure: batch.failure)
                    fileCount += Int(replacement.fileCount)
                    return replacement
                }
                listing.fileCount = fileCount
                listings[parent] = listing
            }
            listings.removeValue(forKey: path)
        } else {
            listings[path] = Listing(identity: identity)
            inaccessible.insert(path)
        }
        invalidation &+= 1; version &+= 1
    }

    private func isCurrent(_ generation: UInt64) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return invalidation == generation && !cancellation.isCancelled
    }

    private func read(cancellation token: ScanCancellation) throws -> (PendingInventoryRead, UInt64) {
        try Task.checkCancellation()
        if token.isCancelled || cancellation.isCancelled { throw CancellationError() }
        lock.lock()
        if let cached, cached.0 == version { lock.unlock(); return (cached.1, cached.0) }
        let captured = Array(listings), revision = version, generation = invalidation, denied = inaccessible
        lock.unlock()
        let byPath = Dictionary(uniqueKeysWithValues: captured)
        var pending = [(Data(root.path.utf8), identity)], batches: [PendingInventoryBatch] = []
        // A replaced directory cannot keep contributing descendants from its previous identity.
        while let (path, expected) = pending.popLast() {
            try Task.checkCancellation()
            if token.isCancelled { throw CancellationError() }
            guard let listing = byPath[path], listing.identity == expected else { continue }
            batches.append(contentsOf: listing.batches)
            for batch in listing.batches {
                for child in batch.index.directories {
                    let childPath = HardLinkPathOrder.path(path, Data(child.name.utf8))
                    guard scope.allows(path: childPath, device: child.device, isMountPoint: child.isMountPoint) else { continue }
                    pending.append((childPath, DirectoryIdentity(device: child.device, inode: child.inode)))
                }
            }
        }
        let snapshot = PendingInventorySnapshot(generation: "live", revision: revision,
            rootPath: Data(root.path.utf8), rootIdentity: identity, batches: batches, inaccessibleDirectories: denied,
            current: { [weak self] in self?.isCurrent(generation) == true }, traversalScope: scope)
        let inventory = try PendingInventoryRead(snapshot: snapshot, committedSequence: 0, durableLinks: { _ in [] }) {
            try Task.checkCancellation()
            if token.isCancelled { throw CancellationError() }
        }
        lock.lock()
        // The reader retires the old aggregate outside the discovery lock.
        let retired = cached
        if version == revision { cached = (revision, inventory) }
        lock.unlock()
        withExtendedLifetime(retired) {}
        return (inventory, revision)
    }

    private var rootNode: DiskNode {
        DiskNode(url: root, name: root.lastPathComponent, isDirectory: true,
            size: 0, allocatedSize: 0, fileCount: 0,
            fileIdentity: "\(identity.device):\(identity.inode)", scanState: .pending)
    }

    func view(path: String?, removals: [ScanRemoval], cancellation: ScanCancellation) throws -> ScanPreviewView? {
        let (inventory, revision) = try read(cancellation: cancellation)
        let seed = rootNode
        return try inventory.memoryView(root: seed, path: path, revision: revision,
                                        removals: removals, cancellation: cancellation)
    }

    func fileCount(removals: [ScanRemoval], cancellation: ScanCancellation) throws -> Int? {
        func check() throws {
            try Task.checkCancellation()
            if cancellation.isCancelled || self.cancellation.isCancelled { throw CancellationError() }
        }
        try check()
        lock.lock()
        if removals.isEmpty, let cachedFileCount, cachedFileCount.revision == version {
            lock.unlock(); return cachedFileCount.count
        }
        let captured = Array(listings), revision = version, generation = invalidation
        lock.unlock()
        let byPath = Dictionary(uniqueKeysWithValues: captured)
        let rootPath = Data(root.path.utf8)
        var excluded: [Data: [Data: Set<DirectoryIdentity>]] = [:]
        for removal in removals {
            try check()
            guard let identity = InventorySchema.parseIdentity(removal.identity) else { continue }
            let path = Data(removal.path.utf8)
            if path == rootPath, identity.device == self.identity.device, identity.inode == self.identity.inode { return nil }
            guard let slash = path.lastIndex(of: 47) else { continue }
            let name = Data(path.suffix(from: path.index(after: slash)))
            excluded[PendingInventoryRead.parent(path), default: [:]][name, default: []]
                .insert(DirectoryIdentity(device: identity.device, inode: identity.inode))
        }
        var count = 0
        var pending = [(rootPath, identity)]
        while let (path, expected) = pending.popLast() {
            try check()
            guard let listing = byPath[path], listing.identity == expected else { continue }
            count += listing.fileCount
            let removals = excluded[path] ?? [:]
            for (name, identities) in removals {
                try check()
                for batch in listing.batches.reversed() {
                    try check()
                    guard let offset = batch.index.offsets[name] else { continue }
                    let record = batch.records[offset]
                    if !record.isDirectory, identities.contains(DirectoryIdentity(device: record.device, inode: record.inode)) {
                        count -= 1
                    }
                    break
                }
            }
            for batch in listing.batches {
                try check()
                // File subtotals already exist; only directories are needed to establish reachability.
                for (offset, child) in batch.index.directories.enumerated() {
                    if offset & 255 == 0 { try check() }
                    let name = Data(child.name.utf8), childIdentity = DirectoryIdentity(device: child.device, inode: child.inode)
                    let childPath = HardLinkPathOrder.path(path, name)
                    guard scope.allows(path: childPath, device: child.device, isMountPoint: child.isMountPoint) else { continue }
                    if removals[name]?.contains(childIdentity) == true { continue }
                    pending.append((childPath, childIdentity))
                }
            }
        }
        try check()
        lock.lock()
        guard invalidation == generation, !self.cancellation.isCancelled else { lock.unlock(); throw CancellationError() }
        if removals.isEmpty, version == revision { cachedFileCount = (revision, count) }
        lock.unlock()
        return count
    }

    func search(query: String, removals: [ScanRemoval], cancellation: ScanCancellation, limit: Int)
        throws -> (nodes: [DiskNode], count: Int, revision: UInt64, removalsChanged: Bool) {
        let (inventory, revision) = try read(cancellation: cancellation)
        return try inventory.memorySearch(query: query, revision: revision, removals: removals,
                                          cancellation: cancellation, limit: limit)
    }

    func confirmsMissingDirectory(_ path: String) -> Bool {
        guard let (inventory, _) = try? read(cancellation: cancellation) else { return false }
        if let node = inventory.node(path, durable: path == root.path ? rootNode : nil) { return !node.isDirectory }
        var current = Data(path.utf8)
        while current != Data(root.path.utf8) {
            let parent = PendingInventoryRead.parent(current)
            guard parent != current else { return false }
            if let node = inventory.node(String(decoding: parent, as: UTF8.self), durable: parent == Data(root.path.utf8) ? rootNode : nil) {
                return !node.isDirectory || node.scanState == .complete
            }
            current = parent
        }
        return false
    }
}
