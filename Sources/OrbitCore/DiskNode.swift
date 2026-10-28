import Foundation

public enum NodeScanState: Sendable, Equatable {
    case pending, scanning, complete
}

public struct DiskNode: Identifiable, Sendable {
    public var id: String { url.path }
    public let url: URL
    public let name: String
    public let isDirectory: Bool
    public let isSymbolicLink: Bool
    public let isAccessible: Bool
    public let size: Int64
    public let allocatedSize: Int64
    /// Leaf allocation before hard-link deduplication; zero for directories.
    public let rawAllocatedSize: Int64
    /// Device and inode for the filesystem entry, including directories.
    public let fileIdentity: String?
    public let fileCount: Int
    public let children: [DiskNode]
    public let scanState: NodeScanState
    public var isScanComplete: Bool { scanState == .complete }

    public init(url: URL, name: String, isDirectory: Bool, isSymbolicLink: Bool = false,
                size: Int64, allocatedSize: Int64, fileCount: Int, children: [DiskNode] = [],
                isAccessible: Bool = true, rawAllocatedSize: Int64? = nil, fileIdentity: String? = nil,
                scanState: NodeScanState = .complete) {
        self.url = url
        self.name = name
        self.isDirectory = isDirectory
        self.isSymbolicLink = isSymbolicLink
        self.isAccessible = isAccessible
        self.size = size
        self.allocatedSize = allocatedSize
        self.rawAllocatedSize = isDirectory ? 0 : (rawAllocatedSize ?? allocatedSize)
        self.fileIdentity = fileIdentity
        self.fileCount = fileCount
        self.children = children
        self.scanState = scanState
    }
}

public struct ScanProgress: Sendable {
    public let itemCount: Int
    public let bytesScanned: Int64
    public let currentPath: String
}

public struct ScanResult: Sendable {
    public let root: DiskNode
    public let elapsed: TimeInterval
    public let skippedCount: Int
    public let issues: [String]

    public init(root: DiskNode, elapsed: TimeInterval, skippedCount: Int, issues: [String]) {
        self.root = root
        self.elapsed = elapsed
        self.skippedCount = skippedCount
        self.issues = issues
    }
}

public final class ScanCancellation: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false

    public init() {}

    public func cancel() {
        lock.lock()
        cancelled = true
        lock.unlock()
    }

    public var isCancelled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancelled
    }

    func commitIfNotCancelled<T>(_ operation: () throws -> T) throws -> T {
        lock.lock()
        defer { lock.unlock() }
        guard !cancelled else { throw CancellationError() }
        return try operation()
    }
}

public enum ScanError: LocalizedError {
    case excluded(String)
    case notDirectory(String)
    case filesystem(String, Int32)

    public var errorDescription: String? {
        switch self {
        case .excluded(let reason): return reason
        case .notDirectory(let path):
            return "Choose a folder or disk to scan. \(path) is not a folder."
        case .filesystem(let path, let code):
            return "\(path): \(String(cString: strerror(code)))"
        }
    }
}
