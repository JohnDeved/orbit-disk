import Darwin
import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

enum SecureCursorChecks {
    private static func components(_ root: URL, _ names: [String]) throws -> [DirectoryComponent] {
        var current = root
        return try names.map { name in
            current.appendPathComponent(name, isDirectory: true)
            let record = try FileMetadata.read(current)
            return DirectoryComponent(name: name, expected: DirectoryIdentity(device: record.device, inode: record.inode))
        }
    }

    private static func openAndClose(_ cursor: SecureDirectoryCursor, _ components: [DirectoryComponent]) throws {
        let descriptor = try cursor.openDirectory(components)
        defer { close(descriptor) }
        var info = stat()
        try require(fstat(descriptor, &info) == 0, "Returned cursor descriptor is invalid")
        if let last = components.last {
            try require(info.st_dev == last.expected.device && info.st_ino == last.expected.inode,
                        "Cursor opened the wrong directory inode")
        }
    }

    private static func rejected(_ operation: () throws -> Int32) throws {
        do {
            let descriptor = try operation()
            close(descriptor)
            throw AsyncCheckFailure(description: "Cursor accepted a replaced ancestor")
        } catch is SecureTraversalError {}
    }

    private static func names(_ descriptor: Int32) throws -> Set<String> {
        guard let stream = fdopendir(descriptor) else {
            close(descriptor)
            throw AsyncCheckFailure(description: "Could not read returned cursor directory")
        }
        defer { closedir(stream) }
        var result: Set<String> = []
        while let entry = readdir(stream) {
            let name = withUnsafePointer(to: &entry.pointee.d_name) { pointer in
                pointer.withMemoryRebound(to: CChar.self, capacity: Int(entry.pointee.d_namlen) + 1) { String(cString: $0) }
            }
            if name != "." && name != ".." { result.insert(name) }
        }
        return result
    }

    static func run(_ base: URL) throws {
        try cachedAncestorReplacement(base)
        try cachedSymlinkReplacement(base)
        try rootReplacement(base)
        try offsetsDepthAndClose(base)
        print("PASS: secure cursor validates cached ancestors/root, rejects symlinks, bounds deep traversal and closes independent descriptors")
    }

    private static func cachedAncestorReplacement(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "cursor-ancestor-rename")
        try fixture.directory("a/b/c")
        let root = try SecureRoot(url: fixture.root)
        let cursor = try SecureDirectoryCursor(root: root, rootURL: fixture.root)
        defer { cursor.close() }
        let chain = try components(fixture.root, ["a", "b", "c"])
        try openAndClose(cursor, chain)
        try FileManager.default.moveItem(at: fixture.root.appendingPathComponent("a/b"), to: fixture.base.appendingPathComponent("moved-b"))
        try fixture.directory("a/b/c")
        try rejected { try cursor.openDirectory(chain) }
        try require(cursor.testCachedDescriptors.count == 1, "Rejected ancestor left stale child descriptors cached")
    }

    private static func cachedSymlinkReplacement(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "cursor-ancestor-symlink")
        try fixture.directory("a/b/c")
        let root = try SecureRoot(url: fixture.root)
        let cursor = try SecureDirectoryCursor(root: root, rootURL: fixture.root)
        defer { cursor.close() }
        let chain = try components(fixture.root, ["a", "b", "c"])
        try openAndClose(cursor, chain)
        let moved = fixture.base.appendingPathComponent("moved-b")
        try FileManager.default.moveItem(at: fixture.root.appendingPathComponent("a/b"), to: moved)
        try FileManager.default.createSymbolicLink(at: fixture.root.appendingPathComponent("a/b"), withDestinationURL: moved)
        try rejected { try cursor.openDirectory(chain) }
    }

    private static func rootReplacement(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "cursor-root-replacement")
        try fixture.directory("a/b")
        let root = try SecureRoot(url: fixture.root)
        let cursor = try SecureDirectoryCursor(root: root, rootURL: fixture.root)
        defer { cursor.close() }
        let chain = try components(fixture.root, ["a", "b"])
        try openAndClose(cursor, chain)
        try FileManager.default.moveItem(at: fixture.root, to: fixture.base.appendingPathComponent("old-root"))
        try fixture.directory("a/b")
        try rejected { try cursor.openDirectory(chain) }
    }

    private static func offsetsDepthAndClose(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "cursor-depth-close")
        try fixture.file("a/first")
        try fixture.file("a/second")
        try fixture.directory("a/b/c/d/e/f/g/h")
        let root = try SecureRoot(url: fixture.root)
        var cursor: SecureDirectoryCursor? = try SecureDirectoryCursor(root: root, rootURL: fixture.root, maximumCachedDirectories: 4)
        let shallow = try components(fixture.root, ["a"])
        let first = try names(cursor!.openDirectory(shallow))
        let second = try names(cursor!.openDirectory(shallow))
        try require(first == ["first", "second", "b"] && second == first,
                    "Returned directory descriptors share an enumeration offset")
        let deepNames = ["a", "b", "c", "d", "e", "f", "g", "h"]
        let chain = try components(fixture.root, deepNames)
        for depth in 1...chain.count {
            try openAndClose(cursor!, Array(chain.prefix(depth)))
            try require(cursor!.testCachedDescriptors.count <= 4, "Deep traversal exceeded the descriptor cache limit")
        }
        try require(cursor!.testFallbackCount > 0, "Deep fixture did not exercise the bounded fallback")
        let held = cursor!.testCachedDescriptors
        cursor!.close(); cursor!.close()
        for descriptor in held {
            errno = 0
            try require(fcntl(descriptor, F_GETFD) == -1 && errno == EBADF, "Explicit close leaked a cached descriptor")
        }
        do {
            let descriptor = try cursor!.openDirectory(shallow)
            close(descriptor)
            throw AsyncCheckFailure(description: "Closed cursor reopened a directory")
        } catch SecureTraversalError.filesystem(let code) {
            try require(code == EBADF, "Closed cursor returned the wrong failure")
        }
        cursor = try SecureDirectoryCursor(root: root, rootURL: fixture.root)
        try openAndClose(cursor!, shallow)
        let owned = cursor!.testCachedDescriptors
        cursor = nil
        for descriptor in owned {
            errno = 0
            try require(fcntl(descriptor, F_GETFD) == -1 && errno == EBADF, "Cursor destruction leaked a cached descriptor")
        }
    }
}
