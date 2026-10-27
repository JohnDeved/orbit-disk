import Foundation

public struct DiskRemovalProjection: Sendable {
    struct Delta: Sendable { var size: Int64 = 0; var allocated: Int64 = 0; var count = 0 }
    private let hidden: Set<Data>
    private let deltas: [Data: (path: String, delta: Delta)]
    var indexedDeltas: [(String, Delta)] { deltas.values.map { ($0.path, $0.delta) } }
    private static func key(_ path: String) -> Data { Data(path.utf8) }
    private static func parent(_ path: String) -> String {
        let bytes = path.utf8
        let simple = bytes.first == 47 && (bytes.count == 1 || bytes.last != 47)
            && !path.hasSuffix("/.") && !path.hasSuffix("/..")
            && !zip(bytes, bytes.dropFirst()).contains { $0 == 47 && $1 == 47 }
        if simple, let slash = bytes.lastIndex(of: 47) {
            return slash == bytes.startIndex ? "/" : String(decoding: bytes[..<slash], as: UTF8.self)
        }
        return URL(fileURLWithPath: path).deletingLastPathComponent().path
    }

    public init(removals: [ScanRemoval], source: [DiskNode]) {
        self = try! DiskRemovalProjection(removals: removals, source: source, checkCancellation: {})
    }
    init(removals: [ScanRemoval], source: [DiskNode], cancellation: ScanCancellation) throws {
        try self.init(removals: removals, source: source, checkCancellation: {
            try Task.checkCancellation()
            if cancellation.isCancelled { throw CancellationError() }
        })
    }
    private init(removals: [ScanRemoval], source: [DiskNode], checkCancellation: () throws -> Void) throws {
        var nodes: [Data: DiskNode] = [:]
        for (index, node) in source.enumerated() {
            if index & 255 == 0 { try checkCancellation() }
            nodes[Self.key(node.id)] = node
        }
        var hidden = Set<Data>()
        for (index, removal) in removals.enumerated() {
            if index & 255 == 0 { try checkCancellation() }
            let path = Self.key(removal.path)
            if nodes[path]?.fileIdentity == removal.identity { hidden.insert(path) }
        }
        var totals: [Data: (path: String, delta: Delta)] = [:]
        for (index, path) in hidden.enumerated() {
            if index & 255 == 0 { try checkCancellation() }
            guard let node = nodes[path] else { continue }
            var current = node.id, parents: [String] = [], covered = false
            while current != "/" {
                let parent = Self.parent(current)
                guard parent != current else { break }
                if hidden.contains(Self.key(parent)) { covered = true; break }
                parents.append(parent); current = parent
            }
            if covered { continue }
            for parent in parents {
                let key = Self.key(parent)
                var delta = totals[key]?.delta ?? Delta()
                delta.size += node.size; delta.allocated += node.allocatedSize; delta.count += node.fileCount
                totals[key] = (parent, delta)
            }
        }
        self.hidden = hidden; self.deltas = totals
    }
    public func apply(_ node: DiskNode) -> DiskNode? {
        try! apply(node, checkCancellation: {})
    }
    func apply(_ node: DiskNode, cancellation: ScanCancellation) throws -> DiskNode? {
        try apply(node, checkCancellation: {
            try Task.checkCancellation()
            if cancellation.isCancelled { throw CancellationError() }
        })
    }
    private func apply(_ node: DiskNode, checkCancellation: () throws -> Void) throws -> DiskNode? {
        try checkCancellation()
        if hidden.isEmpty { return node }
        var current = node.id
        while true {
            if hidden.contains(Self.key(current)) { return nil }
            if current == "/" { break }
            let parent = Self.parent(current)
            if parent == current { break }; current = parent
        }
        guard let delta = deltas[Self.key(node.id)]?.delta else { return node }
        guard node.size >= delta.size, node.allocatedSize >= delta.allocated, node.fileCount >= delta.count else { return nil }
        let children = try node.children.compactMap { try apply($0, checkCancellation: checkCancellation) }.sorted { $0.size == $1.size ? $0.name < $1.name : $0.size > $1.size }
        return DiskNode(url: node.url, name: node.name, isDirectory: node.isDirectory,
                        isSymbolicLink: node.isSymbolicLink, size: node.size - delta.size,
                        allocatedSize: node.allocatedSize - delta.allocated, fileCount: node.fileCount - delta.count,
                        children: children, isAccessible: node.isAccessible, rawAllocatedSize: node.rawAllocatedSize,
                        fileIdentity: node.fileIdentity, scanState: node.scanState)
    }
}
