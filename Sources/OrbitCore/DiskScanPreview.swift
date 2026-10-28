import Foundation

public struct ScanPreviewView: Sendable {
    public let root: DiskNode
    public let folder: DiskNode
    public let ancestors: [DiskNode]
    public let revision: UInt64
    public let isListing: Bool
}

/// Shares partial scan views with readers while keeping incomplete stages private.
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
        var hasListedChildren = false

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
    private var staged: ResumableStage?
    private var preferredPath: String?
    private let changes = ScanChangeNotifications()
    private var stageObservation: Task<Void, Never>?
    public var updates: AsyncStream<Void> { changes.stream() }

    public func attach(stage: ResumableStage) {
        lock.lock(); defer { lock.unlock(); changes.send() }
        staged = stage; version &+= 1; stage.prioritize(path: preferredPath)
        stageObservation?.cancel()
        let updates = stage.updates
        stageObservation = Task { [weak self] in
            for await _ in updates {
                if Task.isCancelled { return }
                self?.changes.send()
            }
        }
    }
    public func prioritize(path: String?) {
        lock.lock(); defer { lock.unlock() }
        preferredPath = path; staged?.prioritize(path: path)
    }
    public func detachStage() {
        lock.lock(); staged = nil; stageObservation?.cancel(); stageObservation = nil; version &+= 1; lock.unlock()
        changes.send()
    }
    private func currentStage() -> ResumableStage? {
        lock.lock(); defer { lock.unlock() }; return staged
    }
    public func removalReplayReadiness() -> (cursor: RemovalCursor?, blocked: Bool, generation: UInt64)? {
        currentStage()?.removalReplayReadiness()
    }
    public var isSearchPrepared: Bool { currentStage()?.isSearchPrepared ?? false }
    #if ORBIT_TESTING
    package var testAfterSearchRemovals: (@Sendable () -> Void)?
    package func testBegin(_ root: DiskNode) { begin(root) }
    package func testReplaceListing(_ path: String, children: [DiskNode]) { recordListing(path, children: children) }
    #endif

    public init() {}
    deinit { stageObservation?.cancel() }

    public var revision: UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return staged?.revision ?? version
    }

    public func view(path: String? = nil) -> ScanPreviewView? {
        if let staged = currentStage() { return try? staged.view(path: path, removals: [], cancellation: ScanCancellation()) }
        return try! view(path: path, removals: [], checkCancellation: nil)
    }

    public func view(path: String? = nil, cancellation: ScanCancellation) throws -> ScanPreviewView? {
        if let staged = currentStage() { return try staged.view(path: path, removals: [], cancellation: cancellation) }
        return try view(path: path, removals: []) {
            try Task.checkCancellation()
            if cancellation.isCancelled { throw CancellationError() }
        }
    }

    public func view(path: String? = nil, cancellation: ScanCancellation, removals: [ScanRemoval]) throws -> ScanPreviewView? {
        if let staged = currentStage() { return try staged.view(path: path, removals: removals, cancellation: cancellation) }
        return try view(path: path, removals: removals) {
            try Task.checkCancellation()
            if cancellation.isCancelled { throw CancellationError() }
        }
    }

    func view(path: String?, checkCancellation: (() throws -> Void)?) throws -> ScanPreviewView? {
        try view(path: path, removals: [], checkCancellation: checkCancellation)
    }

    private func view(path: String?, removals: [ScanRemoval], checkCancellation: (() throws -> Void)?) throws -> ScanPreviewView? {
        try checkCancellation?()
        lock.lock()
        defer { lock.unlock() }
        try checkCancellation?()
        guard let rootID, let rootEntry = entries[rootID],
              let folderEntry = entries[path ?? rootID], folderEntry.metadata.isDirectory else { return nil }
        var budget = 2_000
        let folder = try hydrate(folderEntry, depth: 0, budget: &budget, allChildren: true,
                                 checkCancellation: checkCancellation)
        let root = folderEntry === rootEntry ? folder : rootEntry.node()
        var ancestors = [folder]
        var parent = folderEntry.parent
        while let id = parent, let entry = entries[id] {
            try checkCancellation?()
            ancestors.append(id == rootID ? root : entry.node())
            parent = entry.parent
        }
        try checkCancellation?()
        let view = ScanPreviewView(root: root, folder: folder, ancestors: Array(ancestors.reversed()), revision: version,
                                   isListing: folderEntry.scanState == .scanning && !folderEntry.hasListedChildren)
        return try project(view, removals: removals, checkCancellation: checkCancellation)
    }

    private struct Delta { var size: Int64 = 0; var allocated: Int64 = 0; var count = 0 }
    private func project(_ view: ScanPreviewView, removals: [ScanRemoval],
                         checkCancellation: (() throws -> Void)?) throws -> ScanPreviewView? {
        guard !removals.isEmpty else { return view }
        var hidden = Set<String>()
        for removal in removals {
            try checkCancellation?()
            if let entry = entries[removal.path], entry.metadata.fileIdentity == removal.identity { hidden.insert(removal.path) }
        }
        var deltas: [String: Delta] = [:]
        for id in hidden {
            try checkCancellation?()
            guard let entry = entries[id] else { continue }
            var ancestor = entry.parent, covered = false
            while let id = ancestor, let current = entries[id] {
                if hidden.contains(id) { covered = true; break }
                ancestor = current.parent
            }
            if covered { continue }
            ancestor = entry.parent
            while let id = ancestor, let current = entries[id] {
                var delta = deltas[id, default: Delta()]
                delta.size += entry.size; delta.allocated += entry.allocatedSize; delta.count += entry.fileCount
                deltas[id] = delta
                ancestor = current.parent
            }
        }
        func apply(_ node: DiskNode) throws -> DiskNode? {
            try checkCancellation?()
            var path: String? = node.id
            while let current = path, let entry = entries[current] {
                if hidden.contains(current) { return nil }
                path = entry.parent
            }
            guard let delta = deltas[node.id] else { return node }
            guard node.size >= delta.size, node.allocatedSize >= delta.allocated, node.fileCount >= delta.count else { return nil }
            let children = try node.children.compactMap { try apply($0) }
                .sorted { $0.size == $1.size ? $0.name < $1.name : $0.size > $1.size }
            return DiskNode(url: node.url, name: node.name, isDirectory: node.isDirectory,
                isSymbolicLink: node.isSymbolicLink, size: node.size - delta.size,
                allocatedSize: node.allocatedSize - delta.allocated, fileCount: node.fileCount - delta.count,
                children: children, isAccessible: node.isAccessible, rawAllocatedSize: node.rawAllocatedSize,
                fileIdentity: node.fileIdentity, scanState: node.scanState)
        }
        guard let root = try apply(view.root), let folder = try apply(view.folder) else { return nil }
        var ancestors: [DiskNode] = []
        for ancestor in view.ancestors { guard let value = try apply(ancestor) else { return nil }; ancestors.append(value) }
        return ScanPreviewView(root: root, folder: folder, ancestors: ancestors, revision: view.revision, isListing: view.isListing)
    }

    public func search(query: String, cancellation: ScanCancellation, removals: [ScanRemoval] = [], limit: Int = 300) throws -> (nodes: [DiskNode], count: Int, revision: UInt64, removalsChanged: Bool) {
        if let staged = currentStage() { return try staged.search(query: query, cancellation: cancellation, removals: removals, limit: limit) }
        let locale = Locale(identifier: "en_US_POSIX")
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).folding(options: [.caseInsensitive, .diacriticInsensitive], locale: locale)
        guard !needle.isEmpty else { return ([], 0, revision, false) }
        let needleBytes = Array(needle.utf8)
        func containsFoldedBytes(_ name: String) -> Bool {
            let folded = name.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: locale)
            let bytes = folded.utf8
            return bytes.indices.contains { bytes[$0...].starts(with: needleBytes) }
        }
        struct Frame { let path: String; let identity: String?; let children: [String]; var offset: Int }
        func frame(_ path: String, identity: String?) -> Frame? {
            lock.lock(); defer { lock.unlock() }
            guard let entry = entries[path], entry.metadata.isDirectory, entry.metadata.fileIdentity == identity else { return nil }
            return Frame(path: path, identity: identity, children: entry.children, offset: 0)
        }
        lock.lock()
        let root = rootID.flatMap { entries[$0] }.map { ($0.metadata.id, $0.metadata.fileIdentity) }
        let initialRevision = version
        lock.unlock()
        guard let root, let initial = frame(root.0, identity: root.1) else { return ([], 0, initialRevision, false) }
        var hidden: [String: String] = [:]
        for offset in stride(from: 0, to: removals.count, by: 256) {
            try Task.checkCancellation(); if cancellation.isCancelled { throw CancellationError() }
            lock.lock()
            for removal in removals[offset..<min(offset + 256, removals.count)] {
                if let entry = entries[removal.path], entry.metadata.fileIdentity == removal.identity { hidden[removal.path] = removal.identity }
            }
            lock.unlock()
            if offset + 256 < removals.count { Thread.sleep(forTimeInterval: 0.0001) }
        }
        #if ORBIT_TESTING
        testAfterSearchRemovals?()
        #endif
        let hiddenIDs = Array(hidden.keys)
        var deltas: [String: Delta] = [:]
        var removalOffset = 0
        while removalOffset < hiddenIDs.count {
            try Task.checkCancellation(); if cancellation.isCancelled { throw CancellationError() }
            lock.lock()
            var steps = 0
            while removalOffset < hiddenIDs.count && steps < 512 {
                let id = hiddenIDs[removalOffset]; removalOffset += 1
                guard let entry = entries[id], hidden[id] == entry.metadata.fileIdentity else { continue }
                var ancestor = entry.parent, covered = false
                while let current = ancestor, let parent = entries[current] {
                    steps += 1
                    if let expected = hidden[current], expected == parent.metadata.fileIdentity { covered = true; break }
                    ancestor = parent.parent
                }
                if covered { continue }
                ancestor = entry.parent
                while let current = ancestor, let parent = entries[current] {
                    steps += 1
                    var delta = deltas[current, default: Delta()]
                    delta.size += entry.size; delta.allocated += entry.allocatedSize; delta.count += entry.fileCount
                    deltas[current] = delta; ancestor = parent.parent
                }
            }
            lock.unlock()
            if removalOffset < hiddenIDs.count { Thread.sleep(forTimeInterval: 0.0001) }
        }
        let resultLimit = max(0, min(limit, 10_000))
        var stack = [initial], result: [DiskNode] = [], count = 0
        // A min heap keeps only the largest matches, so common queries remain bounded.
        func worse(_ a: DiskNode, _ b: DiskNode) -> Bool { a.size == b.size ? b.id.utf8.lexicographicallyPrecedes(a.id.utf8) : a.size < b.size }
        func retain(_ node: DiskNode) {
            guard resultLimit > 0 else { return }
            if result.count < resultLimit {
                result.append(node); var child = result.count - 1
                while child > 0 { let parent = (child - 1) / 2; if !worse(result[child], result[parent]) { break }; result.swapAt(child, parent); child = parent }
            } else if worse(result[0], node) {
                result[0] = node; var parent = 0
                while true { let left = parent * 2 + 1; if left >= result.count { break }; let right = left + 1; let child = right < result.count && worse(result[right], result[left]) ? right : left; if !worse(result[child], result[parent]) { break }; result.swapAt(child, parent); parent = child }
            }
        }
        while !stack.isEmpty {
            try Task.checkCancellation(); if cancellation.isCancelled { throw CancellationError() }
            let last = stack.count - 1
            if stack[last].offset >= stack[last].children.count { stack.removeLast(); continue }
            let end = min(stack[last].offset + 256, stack[last].children.count)
            let ids = stack[last].children[stack[last].offset..<end]
            stack[last].offset = end
            lock.lock()
            var nodes: [DiskNode] = []; nodes.reserveCapacity(ids.count)
            if let parent = entries[stack[last].path], parent.metadata.fileIdentity == stack[last].identity {
                for id in ids {
                    guard let entry = entries[id], entry.parent == stack[last].path else { continue }
                    if let expected = hidden[id], expected == entry.metadata.fileIdentity { continue }
                    let node = entry.node()
                    if let delta = deltas[id], node.size >= delta.size, node.allocatedSize >= delta.allocated, node.fileCount >= delta.count {
                        nodes.append(DiskNode(url: node.url, name: node.name, isDirectory: node.isDirectory, isSymbolicLink: node.isSymbolicLink,
                            size: node.size - delta.size, allocatedSize: node.allocatedSize - delta.allocated, fileCount: node.fileCount - delta.count,
                            isAccessible: node.isAccessible, rawAllocatedSize: node.rawAllocatedSize, fileIdentity: node.fileIdentity, scanState: node.scanState))
                    } else if deltas[id] == nil { nodes.append(node) }
                }
            }
            lock.unlock()
            for node in nodes {
                if containsFoldedBytes(node.name) { count += 1; retain(node) }
                if node.isDirectory, let next = frame(node.id, identity: node.fileIdentity) { stack.append(next) }
            }
        }
        result.sort { worse($1, $0) }
        var removalsChanged = false
        for offset in stride(from: 0, to: removals.count, by: 256) {
            try Task.checkCancellation(); if cancellation.isCancelled { throw CancellationError() }
            lock.lock()
            for removal in removals[offset..<min(offset + 256, removals.count)] {
                let matches = entries[removal.path]?.metadata.fileIdentity == removal.identity
                if matches != (hidden[removal.path] == removal.identity) { removalsChanged = true; break }
            }
            lock.unlock()
            if removalsChanged { break }
            if offset + 256 < removals.count { Thread.sleep(forTimeInterval: 0.0001) }
        }
        return (result, count, initialRevision, removalsChanged)
    }

    public func confirmsMissingDirectory(_ path: String) -> Bool {
        if let staged = currentStage() { return staged.confirmsMissingDirectory(path) }
        lock.lock()
        defer { lock.unlock() }
        if let entry = entries[path] { return !entry.metadata.isDirectory }
        var current = path
        while let separator = current.lastIndex(of: "/") {
            let parent = separator == current.startIndex ? "/" : String(current[..<separator])
            if let entry = entries[parent] { return !entry.metadata.isDirectory || entry.hasListedChildren }
            if parent == "/" { break }
            current = parent
        }
        return false
    }

    private func hydrate(_ entry: Entry, depth: Int, budget: inout Int, allChildren: Bool,
                         checkCancellation: (() throws -> Void)?) throws -> DiskNode {
        try checkCancellation?()
        guard depth < 4 else { return entry.node() }
        let ordered = try orderedChildren(entry, checkCancellation: checkCancellation)
        let childIDs = allChildren ? ordered : Array(ordered.prefix(budget))
        if !allChildren { budget -= childIDs.count }
        var children: [DiskNode] = []
        children.reserveCapacity(childIDs.count)
        for (index, id) in childIDs.enumerated() {
            if index & 255 == 0 { try checkCancellation?() }
            if let child = entries[id] { children.append(child.node()) }
        }
        if depth < 3, budget > 0 {
            for index in children.indices where children[index].isDirectory && budget > 0 {
                if let child = entries[children[index].id] {
                    children[index] = try hydrate(child, depth: depth + 1, budget: &budget, allChildren: false,
                                                  checkCancellation: checkCancellation)
                }
            }
        }
        return entry.node(children: children)
    }

    private func orderedChildren(_ entry: Entry, checkCancellation: (() throws -> Void)?) throws -> [String] {
        if let ordered = entry.orderedChildren { return ordered }
        var unordered: [(String, Entry)] = []
        unordered.reserveCapacity(entry.children.count)
        for (index, path) in entry.children.enumerated() {
            if index & 255 == 0 { try checkCancellation?() }
            if let child = entries[path] { unordered.append((path, child)) }
        }
        try checkCancellation?()
        let sorted = unordered.sorted {
            let left = $0.1, right = $1.1
            return left.size == right.size ? left.metadata.name < right.metadata.name : left.size > right.size
        }
        try checkCancellation?()
        let ordered = sorted.map { $0.0 }
        try checkCancellation?()
        entry.orderedChildren = ordered
        return ordered
    }

    func begin(_ root: DiskNode) {
        lock.lock()
        defer { lock.unlock(); changes.send() }
        entries.removeAll(keepingCapacity: true)
        rootID = root.id
        entries[root.id] = Entry(root, parent: nil)
        version &+= 1
    }

    func appendListing(_ path: String, children: [DiskNode]) {
        lock.lock()
        defer { lock.unlock(); changes.send() }
        guard let directory = entries[path] else { return }
        var size: Int64 = 0, allocated: Int64 = 0, count = 0
        for child in children {
            let id = child.id
            directory.children.append(id)
            entries[id] = Entry(child, parent: path)
            size += child.size
            allocated += child.allocatedSize
            count += child.fileCount
        }
        directory.orderedChildren = nil
        updateTotals(directory, size: directory.size + size,
                     allocatedSize: directory.allocatedSize + allocated,
                     fileCount: directory.fileCount + count)
        version &+= 1
    }

    func clearListing(_ path: String) {
        lock.lock()
        defer { lock.unlock(); changes.send() }
        guard let directory = entries[path] else { return }
        for id in directory.children { entries.removeValue(forKey: id) }
        directory.children.removeAll()
        directory.orderedChildren = nil
        updateTotals(directory, size: 0, allocatedSize: 0, fileCount: 0)
        version &+= 1
    }

    func markScanning(_ path: String) {
        lock.lock()
        defer { lock.unlock(); changes.send() }
        guard let entry = entries[path], entry.scanState != .scanning else { return }
        entry.scanState = .scanning
        version &+= 1
    }

    func recordListing(_ path: String, children: [DiskNode]) {
        lock.lock()
        defer { lock.unlock(); changes.send() }
        guard let directory = entries[path] else { return }
        if !directory.hasListedChildren {
            for id in directory.children { entries.removeValue(forKey: id) }
        }
        var childIDs: [String] = []
        childIDs.reserveCapacity(children.count)
        var size: Int64 = 0
        var allocatedSize: Int64 = 0
        var fileCount = 0
        for child in children {
            let id = child.id
            childIDs.append(id)
            entries[id] = Entry(child, parent: path)
            size += child.size
            allocatedSize += child.allocatedSize
            fileCount += child.fileCount
        }
        directory.children = childIDs
        directory.hasListedChildren = true
        directory.orderedChildren = nil
        updateTotals(directory, size: size, allocatedSize: allocatedSize, fileCount: fileCount)
        version &+= 1
    }

    func complete(_ node: DiskNode) {
        lock.lock()
        defer { lock.unlock(); changes.send() }
        guard let entry = entries[node.id] else { return }
        updateTotals(entry, size: node.size, allocatedSize: node.allocatedSize, fileCount: node.fileCount)
        entry.isAccessible = node.isAccessible
        entry.scanState = .complete
        entry.hasListedChildren = true
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
