import Foundation

public struct ScanPreviewView: Sendable {
    public let root: DiskNode
    public let folder: DiskNode
    public let ancestors: [DiskNode]
    public let revision: UInt64
}

/// Shared between the scanner and readers; it never persists an incomplete scan.
public final class DiskScanPreview: @unchecked Sendable {
    private final class Entry {
        let metadata: DiskNode
        let parent: String?
        var size: Int64
        var allocatedSize: Int64
        var fileCount: Int
        var isAccessible: Bool
        var scanState: NodeScanState
        var children: [String] = []
        var orderedChildren: [String]?

        init(_ node: DiskNode, parent: String?) {
            metadata = node
            self.parent = parent
            size = node.size
            allocatedSize = node.allocatedSize
            fileCount = node.fileCount
            isAccessible = node.isAccessible
            scanState = node.scanState
        }

        func node(children: [DiskNode] = []) -> DiskNode {
            DiskNode(url: metadata.url, name: metadata.name, isDirectory: metadata.isDirectory,
                     isSymbolicLink: metadata.isSymbolicLink, size: size, allocatedSize: allocatedSize,
                     fileCount: fileCount, children: children, isAccessible: isAccessible,
                     rawAllocatedSize: metadata.rawAllocatedSize, fileIdentity: metadata.fileIdentity,
                     scanState: scanState)
        }
    }

    private let lock = NSLock()
    private var entries: [String: Entry] = [:]
    private var rootID: String?
    private var version: UInt64 = 0

    public init() {}

    public var revision: UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return version
    }

    public func view(path: String? = nil) -> ScanPreviewView? {
        lock.lock()
        defer { lock.unlock() }
        guard let rootID, let rootEntry = entries[rootID],
              let folderEntry = entries[path ?? rootID], folderEntry.metadata.isDirectory else { return nil }
        var budget = 2_000
        let folder = hydrate(folderEntry, depth: 0, budget: &budget, allChildren: true)
        let root = folderEntry === rootEntry ? folder : rootEntry.node()
        var ancestors = [folder]
        var parent = folderEntry.parent
        while let id = parent, let entry = entries[id] {
            ancestors.append(id == rootID ? root : entry.node())
            parent = entry.parent
        }
        return ScanPreviewView(root: root, folder: folder, ancestors: Array(ancestors.reversed()), revision: version)
    }

    private func hydrate(_ entry: Entry, depth: Int, budget: inout Int, allChildren: Bool) -> DiskNode {
        guard depth < 4 else { return entry.node() }
        let ordered = orderedChildren(entry)
        let childIDs = allChildren ? ordered : Array(ordered.prefix(budget))
        if !allChildren { budget -= childIDs.count }
        var children = childIDs.compactMap { entries[$0]?.node() }
        if depth < 3, budget > 0 {
            let directories = children.indices.filter { children[$0].isDirectory }
            for index in directories where budget > 0 {
                if let child = entries[children[index].id] {
                    children[index] = hydrate(child, depth: depth + 1, budget: &budget, allChildren: false)
                }
            }
        }
        return entry.node(children: children)
    }

    private func orderedChildren(_ entry: Entry) -> [String] {
        if let ordered = entry.orderedChildren { return ordered }
        let ordered = entry.children.compactMap { path in entries[path].map { (path, $0) } }.sorted {
            let left = $0.1, right = $1.1
            return left.size == right.size ? left.metadata.name < right.metadata.name : left.size > right.size
        }.map { $0.0 }
        entry.orderedChildren = ordered
        return ordered
    }

    func begin(_ root: DiskNode) {
        lock.lock()
        defer { lock.unlock() }
        entries.removeAll(keepingCapacity: true)
        rootID = root.id
        entries[root.id] = Entry(root, parent: nil)
        version &+= 1
    }

    func markScanning(_ path: String) {
        lock.lock()
        defer { lock.unlock() }
        guard let entry = entries[path], entry.scanState != .scanning else { return }
        entry.scanState = .scanning
        version &+= 1
    }

    func recordListing(_ path: String, children: [DiskNode]) {
        lock.lock()
        defer { lock.unlock() }
        guard let directory = entries[path] else { return }
        directory.children = children.map(\.id)
        directory.orderedChildren = nil
        var size: Int64 = 0
        var allocatedSize: Int64 = 0
        var fileCount = 0
        for child in children {
            entries[child.id] = Entry(child, parent: path)
            size += child.size
            allocatedSize += child.allocatedSize
            fileCount += child.fileCount
        }
        updateTotals(directory, size: size, allocatedSize: allocatedSize, fileCount: fileCount)
        version &+= 1
    }

    func complete(_ node: DiskNode) {
        lock.lock()
        defer { lock.unlock() }
        guard let entry = entries[node.id] else { return }
        updateTotals(entry, size: node.size, allocatedSize: node.allocatedSize, fileCount: node.fileCount)
        entry.isAccessible = node.isAccessible
        entry.scanState = .complete
        version &+= 1
    }

    private func updateTotals(_ entry: Entry, size: Int64, allocatedSize: Int64, fileCount: Int) {
        let sizeDelta = size - entry.size
        let allocationDelta = allocatedSize - entry.allocatedSize
        let countDelta = fileCount - entry.fileCount
        entry.size = size
        entry.allocatedSize = allocatedSize
        entry.fileCount = fileCount
        var parent = entry.parent
        while let id = parent, let ancestor = entries[id] {
            if sizeDelta != 0 { ancestor.orderedChildren = nil }
            ancestor.size += sizeDelta
            ancestor.allocatedSize += allocationDelta
            ancestor.fileCount += countDelta
            parent = ancestor.parent
        }
    }
}
