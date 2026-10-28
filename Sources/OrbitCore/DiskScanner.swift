import Darwin
import Foundation

public struct DiskScanner: Sendable {
    let useBulkEnumeration: Bool
    #if ORBIT_TESTING
    let testBatch: (@Sendable () throws -> Void)?
    let testFallback: (@Sendable () throws -> Void)?
    let testAfterListing: (@Sendable () -> Void)?
    #endif

    public init() {
        useBulkEnumeration = true
        #if ORBIT_TESTING
        testBatch = nil
        testFallback = nil
        testAfterListing = nil
        #endif
    }
    package init(useBulkEnumeration: Bool) {
        self.useBulkEnumeration = useBulkEnumeration
        #if ORBIT_TESTING
        testBatch = nil
        testFallback = nil
        testAfterListing = nil
        #endif
    }
    #if ORBIT_TESTING
    package init(testBatch: @escaping @Sendable () throws -> Void,
                 testFallback: (@Sendable () throws -> Void)? = nil,
                 testAfterListing: (@Sendable () -> Void)? = nil) {
        useBulkEnumeration = true
        self.testBatch = testBatch
        self.testFallback = testFallback
        self.testAfterListing = testAfterListing
    }
    #endif

    /// Runs synchronously on the caller's thread; call from a detached task or worker queue.
    public func scan(_ url: URL, cancellation: ScanCancellation = ScanCancellation(),
                     preview: DiskScanPreview? = nil,
                     excluding: [URL] = [],
                     progress: @escaping @Sendable (ScanProgress) -> Void = { _ in }) throws -> ScanResult {
        try CloudStoragePolicy.withoutMaterialization {
            try scanLocal(url, cancellation: cancellation, preview: preview, excluding: excluding, progress: progress)
        }
    }

    private func scanLocal(_ url: URL, cancellation: ScanCancellation, preview: DiskScanPreview?,
                           excluding: [URL], progress: @escaping @Sendable (ScanProgress) -> Void) throws -> ScanResult {
        guard url.isFileURL else { throw ScanError.notDirectory(url.absoluteString) }
        let rootURL = url.standardizedFileURL
        let cloudRoots = CloudStoragePolicy.scanExclusions()
        if let reason = CloudStoragePolicy.exclusionReason(for: rootURL, excluding: cloudRoots) {
            throw ScanError.excluded(reason)
        }
        let excluding = excluding + cloudRoots
        let state = ScanState(cancellation: cancellation, progress: progress, useBulk: useBulkEnumeration, preview: preview)
        state.excludedDirectories = Set(excluding.compactMap { url in
            var info = stat()
            guard lstat(url.path, &info) == 0, (info.st_mode & S_IFMT) == S_IFDIR else { return nil }
            return FileIdentity(device: info.st_dev, inode: info.st_ino)
        })
        state.excludedPaths = IndexLocations.exclusionPaths(excluding)
        try state.checkCancellation()
        let metadata = try FileMetadata.read(rootURL)
        guard metadata.isDirectory, !metadata.isSymbolicLink else { throw ScanError.notDirectory(rootURL.path) }
        #if ORBIT_TESTING
        state.testBatch = testBatch
        state.testFallback = testFallback
        state.testAfterListing = testAfterListing
        #endif
        state.rootDevice = metadata.device
        if let preview { preview.begin(state.previewNode(rootURL, metadata: metadata, scanState: .scanning)) }
        state.report(rootURL.path, force: true)
        let root = try state.walk(rootURL, metadata: metadata, isRoot: true)
        try state.checkCancellation()
        state.report(rootURL.path, force: true)
        return ScanResult(root: root, elapsed: ProcessInfo.processInfo.systemUptime - state.started,
                          skippedCount: state.skippedCount, issues: state.issues)
    }
}

private struct FileIdentity: Hashable {
    let device: Int32
    let inode: UInt64
}

package struct FileMetadata {
    let name: String
    let device: Int32
    let inode: UInt64
    let isDirectory: Bool
    let isSymbolicLink: Bool
    let isMountPoint: Bool
    let linkCount: UInt32
    let size: Int64
    let allocatedSize: Int64

    static func read(_ url: URL) throws -> FileMetadata {
        var info = stat()
        guard url.withUnsafeFileSystemRepresentation({ lstat($0!, &info) }) == 0 else {
            throw ScanError.filesystem(url.path, errno)
        }
        let kind = info.st_mode & mode_t(S_IFMT)
        let isDirectory = kind == mode_t(S_IFDIR)
        return FileMetadata(name: url.lastPathComponent, device: info.st_dev, inode: info.st_ino,
                            isDirectory: isDirectory, isSymbolicLink: kind == mode_t(S_IFLNK),
                            isMountPoint: false, linkCount: UInt32(info.st_nlink),
                            size: isDirectory ? 0 : max(0, info.st_size),
                            allocatedSize: isDirectory ? 0 : max(0, info.st_blocks * 512))
    }
}

private final class ScanState {
    let cancellation: ScanCancellation
    let progress: @Sendable (ScanProgress) -> Void
    let useBulk: Bool
    let preview: DiskScanPreview?
    let started = ProcessInfo.processInfo.systemUptime
    #if ORBIT_TESTING
    var testBatch: (@Sendable () throws -> Void)?
    var testFallback: (@Sendable () throws -> Void)?
    var testAfterListing: (@Sendable () -> Void)?
    #endif
    var rootDevice: Int32 = 0
    var itemCount = 0
    var bytesScanned: Int64 = 0
    var skippedCount = 0
    var issues: [String] = []
    var hardLinks = Set<FileIdentity>()
    var excludedDirectories = Set<FileIdentity>()
    var excludedPaths = Set<Data>()
    var lastProgress: TimeInterval = 0

    init(cancellation: ScanCancellation, progress: @escaping @Sendable (ScanProgress) -> Void,
         useBulk: Bool, preview: DiskScanPreview?) {
        self.cancellation = cancellation
        self.progress = progress
        self.useBulk = useBulk
        self.preview = preview
    }

    func checkCancellation() throws {
        if cancellation.isCancelled { throw CancellationError() }
    }

    func report(_ path: @autoclosure () -> String, force: Bool = false, extraItems: Int = 0, extraBytes: Int64 = 0) {
        let now = ProcessInfo.processInfo.systemUptime
        guard force || now - lastProgress >= 0.1 else { return }
        lastProgress = now
        progress(ScanProgress(itemCount: itemCount + extraItems, bytesScanned: bytesScanned + extraBytes, currentPath: path()))
    }

    func skip(_ message: String) {
        skippedCount += 1
        if issues.count < 100 { issues.append(message) }
    }

    func walk(_ url: URL, metadata: FileMetadata, isRoot: Bool = false) throws -> DiskNode {
        try checkCancellation()
        report(url.path)
        if !metadata.isDirectory {
            return fileNode(url, metadata: metadata)
        }
        preview?.markScanning(url.path)

        var children: [DiskNode] = []
        var isAccessible = true
        if !isRoot && (metadata.isMountPoint || metadata.device != rootDevice) {
            isAccessible = false
            skip("\(url.path): nested volume skipped")
        } else {
            do {
                let entries = try readDirectory(url).filter {
                    !$0.isDirectory || (!excludedPaths.contains(Data(url.appendingPathComponent($0.name).path.utf8)) &&
                        !excludedDirectories.contains(FileIdentity(device: $0.device, inode: $0.inode)))
                }
                itemCount += entries.count
                bytesScanned += entries.reduce(0) { $0 + $1.size }
                children.reserveCapacity(entries.count)
                var orderedTraversal: [FileMetadata] = []
                var previewChildren: [DiskNode] = []
                if preview != nil { previewChildren.reserveCapacity(entries.count) }
                for (index, entry) in entries.enumerated() {
                    if index & 127 == 0 {
                        try checkCancellation()
                        report(url.path)
                    }
                    if entry.isDirectory || entry.linkCount > 1 {
                        orderedTraversal.append(entry)
                        if preview != nil {
                            let childURL = url.appendingPathComponent(entry.name, isDirectory: entry.isDirectory)
                            previewChildren.append(previewNode(childURL, metadata: entry, scanState: .pending))
                        }
                    } else {
                        let childURL = url.appendingPathComponent(entry.name, isDirectory: false)
                        let node = fileNode(childURL, metadata: entry)
                        children.append(node)
                        if preview != nil { previewChildren.append(node) }
                    }
                }
                if let preview {
                    try checkCancellation()
                    preview.recordListing(url.path, children: previewChildren)
                    #if ORBIT_TESTING
                    testAfterListing?()
                    #endif
                    report(url.path, force: isRoot)
                    try checkCancellation()
                }
                // Only directories and hard links affect which path owns a shared allocation.
                for entry in orderedTraversal.sorted(by: { $0.name < $1.name }) {
                    try checkCancellation()
                    let childURL = url.appendingPathComponent(entry.name, isDirectory: entry.isDirectory)
                    if entry.isDirectory {
                        children.append(try walk(childURL, metadata: entry))
                    } else {
                        let node = fileNode(childURL, metadata: entry)
                        children.append(node)
                        preview?.complete(node)
                    }
                }
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                preview?.clearListing(url.path)
                if isRoot { throw error }
                isAccessible = false
                skip(error.localizedDescription)
            }
        }
        children.sort { $0.size == $1.size ? $0.name < $1.name : $0.size > $1.size }
        let node = DiskNode(url: url, name: metadata.name.isEmpty ? url.path : metadata.name,
                        isDirectory: true, size: children.reduce(0) { $0 + $1.size },
                        allocatedSize: children.reduce(0) { $0 + $1.allocatedSize },
                        fileCount: children.reduce(0) { $0 + $1.fileCount }, children: children,
                        isAccessible: isAccessible, fileIdentity: "\(metadata.device):\(metadata.inode)")
        preview?.complete(node)
        return node
    }

    func previewNode(_ url: URL, metadata: FileMetadata, scanState: NodeScanState) -> DiskNode {
        DiskNode(url: url, name: metadata.name.isEmpty ? url.path : metadata.name,
                 isDirectory: metadata.isDirectory, isSymbolicLink: metadata.isSymbolicLink,
                 size: metadata.size, allocatedSize: 0, fileCount: metadata.isDirectory ? 0 : 1,
                 rawAllocatedSize: metadata.allocatedSize, fileIdentity: "\(metadata.device):\(metadata.inode)",
                 scanState: scanState)
    }

    func fileNode(_ url: URL, metadata: FileMetadata) -> DiskNode {
        var allocation = metadata.allocatedSize
        if metadata.linkCount > 1 {
            let identity = FileIdentity(device: metadata.device, inode: metadata.inode)
            if !hardLinks.insert(identity).inserted { allocation = 0 }
        }
        return DiskNode(url: url, name: metadata.name, isDirectory: false,
                        isSymbolicLink: metadata.isSymbolicLink, size: metadata.size,
                        allocatedSize: allocation, fileCount: 1,
                        rawAllocatedSize: metadata.allocatedSize,
                        fileIdentity: "\(metadata.device):\(metadata.inode)")
    }

    func readDirectory(_ url: URL) throws -> [FileMetadata] {
        let descriptor = url.withUnsafeFileSystemRepresentation {
            open($0!, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        }
        guard descriptor >= 0 else { throw ScanError.filesystem(url.path, errno) }
        defer { close(descriptor) }
        var info = stat()
        guard fstat(descriptor, &info) == 0 else { throw ScanError.filesystem(url.path, errno) }
        guard info.st_dev == rootDevice else { throw ScanError.filesystem(url.path, EXDEV) }

        if useBulk {
            do {
                var pendingIssues: [String] = []
                var published = 0
                let entries = try BulkDirectory.read(descriptor, url: url, cancellation: cancellation,
                    progress: { count, bytes in self.report(url.path, extraItems: count, extraBytes: bytes) },
                    issue: { pendingIssues.append($0) },
                    batch: { entries in
                        if let preview = self.preview, entries.count >= 1_000, published == 0 {
                            var nodes: [DiskNode] = []
                            nodes.reserveCapacity(512)
                            for (index, entry) in entries.prefix(512).enumerated() {
                                if index & 127 == 0 { try self.checkCancellation() }
                                let childURL = url.appendingPathComponent(entry.name, isDirectory: entry.isDirectory)
                                if entry.isDirectory && (self.excludedPaths.contains(Data(childURL.path.utf8)) ||
                                    self.excludedDirectories.contains(FileIdentity(device: entry.device, inode: entry.inode))) { continue }
                                nodes.append(self.previewNode(childURL, metadata: entry, scanState: .pending))
                            }
                            preview.appendListing(url.path, children: nodes)
                            published = nodes.count
                        }
                        #if ORBIT_TESTING
                        try self.testBatch?()
                        #endif
                    })
                pendingIssues.forEach(skip)
                return entries
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                // A fresh descriptor restarts enumeration, avoiding double counts after a partial bulk read.
            }
        }
        return try readDirectoryFallback(url)
    }

    func readDirectoryFallback(_ url: URL) throws -> [FileMetadata] {
        let descriptor = url.withUnsafeFileSystemRepresentation {
            open($0!, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        }
        guard descriptor >= 0 else { throw ScanError.filesystem(url.path, errno) }
        var info = stat()
        if fstat(descriptor, &info) != 0 {
            let code = errno
            close(descriptor)
            throw ScanError.filesystem(url.path, code)
        }
        if info.st_dev != rootDevice {
            close(descriptor)
            throw ScanError.filesystem(url.path, EXDEV)
        }
        guard let directory = fdopendir(descriptor) else {
            let code = errno
            close(descriptor)
            throw ScanError.filesystem(url.path, code)
        }
        defer { closedir(directory) }
        #if ORBIT_TESTING
        try testFallback?()
        #endif
        var result: [FileMetadata] = []
        var bytes: Int64 = 0
        while true {
            try checkCancellation()
            errno = 0
            guard let entry = readdir(directory) else {
                if errno != 0 { throw ScanError.filesystem(url.path, errno) }
                break
            }
            let name = withUnsafePointer(to: &entry.pointee.d_name) {
                $0.withMemoryRebound(to: CChar.self, capacity: Int(entry.pointee.d_namlen) + 1) { String(cString: $0) }
            }
            if name == "." || name == ".." { continue }
            do {
                let metadata = try FileMetadata.read(url.appendingPathComponent(name, isDirectory: false))
                result.append(metadata)
                bytes += metadata.size
            } catch {
                skip(error.localizedDescription)
            }
            report(url.path, extraItems: result.count, extraBytes: bytes)
        }
        return result
    }
}
