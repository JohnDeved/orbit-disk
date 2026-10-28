import Foundation

extension PendingInventoryRead {
    private func check(_ cancellation: ScanCancellation) throws {
        try Task.checkCancellation()
        if cancellation.isCancelled { throw CancellationError() }
        try validate()
    }

    private func projection(_ removals: [ScanRemoval], cancellation: ScanCancellation) throws -> DiskRemovalProjection {
        let sources = try removals.compactMap { removal -> DiskNode? in
            try check(cancellation)
            return node(removal.path, durable: nil)
        }
        return try DiskRemovalProjection(removals: removals, source: sources, cancellation: cancellation)
    }

    func memoryView(root seed: DiskNode, path: String?, revision: UInt64, removals: [ScanRemoval],
                    cancellation: ScanCancellation) throws -> ScanPreviewView? {
        try check(cancellation)
        let target = path ?? seed.id
        let rootBytes = Data(seed.id.utf8), targetBytes = Data(target.utf8)
        guard targetBytes == rootBytes || (targetBytes.starts(with: rootBytes)
            && (rootBytes == Data([47]) || targetBytes.dropFirst(rootBytes.count).first == 47)),
            let root = node(seed.id, durable: seed),
            let rawFolder = node(target, durable: target == seed.id ? seed : nil), rawFolder.isDirectory else { return nil }
        var budget = 2_000
        func hydrate(_ entry: DiskNode, depth: Int) throws -> DiskNode {
            guard entry.isDirectory, depth < 4, budget > 0 else { return entry }
            let items = try children(entry.id, durable: [], limit: min(96, budget), check: { try check(cancellation) })
            budget -= items.count
            return replacingChildren(entry, try items.map { try hydrate($0, depth: depth + 1) })
        }
        let direct = try children(target, durable: [], limit: Int.max, check: { try check(cancellation) })
        let folder = replacingChildren(rawFolder, try direct.map { try hydrate($0, depth: 1) })
        var ancestors = [folder], current = targetBytes
        while current != rootBytes {
            try check(cancellation)
            let parent = Self.parent(current), text = String(decoding: parent, as: UTF8.self)
            guard parent != current, let item = node(text, durable: parent == rootBytes ? seed : nil) else { return nil }
            ancestors.append(item); current = parent
        }
        let projection = try projection(removals, cancellation: cancellation)
        guard let visibleFolder = try projection.apply(folder, cancellation: cancellation),
              let visibleRoot = try projection.apply(root, cancellation: cancellation) else { return nil }
        let visibleAncestors = try ancestors.reversed().compactMap { try projection.apply($0, cancellation: cancellation) }
        guard visibleAncestors.count == ancestors.count else { return nil }
        try check(cancellation)
        return ScanPreviewView(root: targetBytes == rootBytes ? visibleFolder : visibleRoot,
            folder: visibleFolder, ancestors: visibleAncestors, revision: revision, isListing: rawFolder.scanState != .complete)
    }

    private func replacingChildren(_ node: DiskNode, _ children: [DiskNode]) -> DiskNode {
        DiskNode(url: node.url, name: node.name, isDirectory: node.isDirectory, isSymbolicLink: node.isSymbolicLink,
            size: node.size, allocatedSize: node.allocatedSize, fileCount: node.fileCount, children: children,
            isAccessible: node.isAccessible, rawAllocatedSize: node.rawAllocatedSize,
            fileIdentity: node.fileIdentity, scanState: node.scanState)
    }

    func memorySearch(query: String, revision: UInt64, removals: [ScanRemoval], cancellation: ScanCancellation, limit: Int)
        throws -> (nodes: [DiskNode], count: Int, revision: UInt64, removalsChanged: Bool) {
        try check(cancellation)
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
            .folding(options: [.caseInsensitive, .diacriticInsensitive], locale: Locale(identifier: "en_US_POSIX"))
        guard !needle.isEmpty else { return ([], 0, revision, false) }
        let projection = try projection(removals, cancellation: cancellation)
        let limit = max(0, min(limit, 10_000))
        var result: [DiskNode] = [], count = 0
        func worse(_ a: DiskNode, _ b: DiskNode) -> Bool {
            a.size == b.size ? b.id.utf8.lexicographicallyPrecedes(a.id.utf8) : a.size < b.size
        }
        try forEachMatching(needle, check: { try check(cancellation) }) { raw in
            guard let node = try projection.apply(raw, cancellation: cancellation) else { return }
            count += 1
            guard limit > 0 else { return }
            if result.count < limit {
                result.append(node)
                var child = result.count - 1
                while child > 0 {
                    let parent = (child - 1) / 2
                    if !worse(result[child], result[parent]) { break }
                    result.swapAt(child, parent); child = parent
                }
            } else if worse(result[0], node) {
                result[0] = node
                var parent = 0
                while true {
                    let left = parent * 2 + 1
                    if left >= result.count { break }
                    let right = left + 1
                    let child = right < result.count && worse(result[right], result[left]) ? right : left
                    if !worse(result[child], result[parent]) { break }
                    result.swapAt(child, parent); parent = child
                }
            }
        }
        try check(cancellation)
        return (result.sorted { worse($1, $0) }, count, revision, false)
    }
}
