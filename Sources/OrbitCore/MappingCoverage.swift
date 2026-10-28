import CryptoKit
import CSQLite
import Darwin
import Foundation

public struct MappingCoverage: Sendable {
    public enum Source: Sendable { case stage, saved }
    public let rootURL: URL
    public let rootIdentity: String
    public let requestedIdentity: String
    public let source: Source

    /// Finds existing work without opening a writer or changing its queue. Reopening still validates publication and removal state.
    public static func find(for requestedURL: URL, cache: URL, stagingDirectory: URL? = nil,
                            cancellation: ScanCancellation = ScanCancellation()) throws -> MappingCoverage? {
        try check(cancellation)
        guard requestedURL.isFileURL, !requestedURL.path.utf8.contains(0) else { return nil }
        let requested = requestedURL.standardizedFileURL
        guard let target = directoryInfo(requested) else { return nil }
        let store = DiskIndexStore(directory: cache)
        let staging = (stagingDirectory ?? store.lockDirectory).standardizedFileURL
        var root = requested
        while true {
            try check(cancellation)
            guard let info = directoryInfo(root) else { return nil }
            let scope = VolumeTraversalScope(rootURL: root,
                rootIdentity: DirectoryIdentity(device: info.device, inode: info.inode))
            let eligible = scope.allowsIdentity(path: Data(requested.path.utf8),
                identity: DirectoryIdentity(device: target.device, inode: target.inode))
                && scope.allowsMount(path: Data(requested.path.utf8), mount: target.mount)
            var source: Source?
            if eligible, let saved = store.savedSnapshot(for: root), saved.rootURL.path.utf8.elementsEqual(root.path.utf8),
               let rootNode = try? store.node(path: root.path, snapshot: saved, cancellation: cancellation),
               rootNode.isDirectory, rootNode.fileIdentity == info.identity,
               let node = try? store.node(path: requested.path, snapshot: saved, cancellation: cancellation),
               node.isDirectory, node.id.utf8.elementsEqual(requested.path.utf8), node.fileIdentity == target.identity {
                source = .saved
            } else if eligible, try stagedCandidate(root: root, info: info, requested: requested, target: target,
                                          staging: staging, locks: store.lockDirectory, cancellation: cancellation) {
                source = .stage
            }
            try check(cancellation)
            if let source, try canReach(root: root, target: requested, expectedRootIdentity: info.identity,
                                        expectedTargetIdentity: target.identity, cancellation: cancellation) {
                return MappingCoverage(rootURL: root, rootIdentity: info.identity,
                                       requestedIdentity: target.identity, source: source)
            }
            if root.path == "/" { return nil }
            root.deleteLastPathComponent()
        }
    }

    public static func canReach(root: URL, target: URL, expectedRootIdentity: String,
                                expectedTargetIdentity: String? = nil,
                                cancellation: ScanCancellation = ScanCancellation()) throws -> Bool {
        try check(cancellation)
        guard root.isFileURL, target.isFileURL, !root.path.utf8.contains(0), !target.path.utf8.contains(0) else { return false }
        let root = root.standardizedFileURL, target = target.standardizedFileURL
        let rootBytes = Array(root.path.utf8), targetBytes = Array(target.path.utf8)
        let offset = rootBytes == [47] ? 1 : rootBytes.count + 1
        guard targetBytes == rootBytes || (targetBytes.count >= offset && targetBytes.starts(with: rootBytes)
            && (rootBytes == [47] || targetBytes[rootBytes.count] == 47)) else { return false }
        let descriptor = open(root.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { return false }
        defer { close(descriptor) }
        guard let rootInfo = directoryInfo(descriptor), rootInfo.identity == expectedRootIdentity else { return false }
        let scope = VolumeTraversalScope(rootURL: root,
            rootIdentity: DirectoryIdentity(device: rootInfo.device, inode: rootInfo.inode))
        var current = descriptor
        var currentPath = Data(rootBytes)
        defer { if current != descriptor { close(current) } }
        if targetBytes != rootBytes {
            for component in targetBytes.dropFirst(offset).split(separator: 47) {
                try check(cancellation)
                let name = String(decoding: component, as: UTF8.self)
                let child = openat(current, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                guard child >= 0 else { return false }
                if current != descriptor { close(current) }
                current = child
                currentPath = HardLinkPathOrder.path(currentPath, Data(component))
                guard let info = directoryInfo(child),
                      scope.allowsIdentity(path: currentPath, identity: DirectoryIdentity(device: info.device, inode: info.inode)),
                      scope.allowsMount(path: currentPath, mount: info.mount) else { return false }
            }
        }
        try check(cancellation)
        guard let final = directoryInfo(current), expectedTargetIdentity == nil || final.identity == expectedTargetIdentity,
              directoryInfo(root)?.identity == expectedRootIdentity,
              directoryInfo(target)?.identity == final.identity else { return false }
        return true
    }

    private struct DirectoryInfo {
        let device: Int32
        let inode: UInt64
        let mount: String
        var identity: String { "\(device):\(inode)" }
    }

    private static func directoryInfo(_ url: URL) -> DirectoryInfo? {
        let descriptor = open(url.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { return nil }
        defer { close(descriptor) }
        return directoryInfo(descriptor)
    }

    private static func directoryInfo(_ descriptor: Int32) -> DirectoryInfo? {
        var info = stat(), filesystem = statfs()
        guard fstat(descriptor, &info) == 0, fstatfs(descriptor, &filesystem) == 0 else { return nil }
        let mount = withUnsafePointer(to: &filesystem.f_mntonname) {
            String(cString: UnsafeRawPointer($0).assumingMemoryBound(to: CChar.self))
        }
        return DirectoryInfo(device: info.st_dev, inode: info.st_ino, mount: mount)
    }

    private static func check(_ cancellation: ScanCancellation) throws {
        if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
    }

    private static func privateFile(_ info: stat) -> Bool {
        (info.st_mode & S_IFMT) == S_IFREG && info.st_uid == geteuid()
            && info.st_nlink == 1 && (info.st_mode & 0o777) == 0o600
    }

    private static func sameFile(_ first: stat, _ second: stat) -> Bool {
        first.st_dev == second.st_dev && first.st_ino == second.st_ino
    }

    private static func generation(root: URL, locks: URL) -> String? {
        let directory = open(locks.appendingPathComponent(".orbit-locks").path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard directory >= 0 else { return nil }
        defer { close(directory) }
        var info = stat()
        guard fstat(directory, &info) == 0, info.st_uid == geteuid(), (info.st_mode & 0o777) == 0o700 else { return nil }
        let name = SHA256.hash(data: Data(root.path.utf8)).map { String(format: "%02x", $0) }.joined() + ".lock.generation"
        let file = openat(directory, name, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard file >= 0 else { return errno == ENOENT ? "0" : nil }
        defer { close(file) }
        var pathInfo = stat()
        guard fstat(file, &info) == 0, privateFile(info),
              fstatat(directory, name, &pathInfo, AT_SYMLINK_NOFOLLOW) == 0, sameFile(info, pathInfo) else { return nil }
        var bytes = [UInt8](repeating: 0, count: 64)
        let count = read(file, &bytes, bytes.count)
        guard count > 0, count < bytes.count, let value = String(bytes: bytes.prefix(count), encoding: .utf8),
              UUID(uuidString: value) != nil else { return nil }
        return value
    }

    private static func stagedCandidate(root: URL, info: DirectoryInfo, requested: URL, target: DirectoryInfo,
                                         staging: URL, locks: URL, cancellation: ScanCancellation) throws -> Bool {
        guard let key = try? StageLease.key(root: root) else { return false }
        let directoryURL = staging.appendingPathComponent(".orbit-stages")
        let directory = open(directoryURL.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard directory >= 0 else { return false }
        defer { close(directory) }
        var directoryIdentity = stat()
        guard fstat(directory, &directoryIdentity) == 0, directoryIdentity.st_uid == geteuid(),
              (directoryIdentity.st_mode & 0o777) == 0o700 else { return false }
        let name = key + ".sqlite"
        let file = openat(directory, name, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard file >= 0 else { return false }
        defer { close(file) }
        var identity = stat()
        guard fstat(file, &identity) == 0, privateFile(identity), identity.st_size > 0 else { return false }
        guard let resolved = realpath(directoryURL.path, nil) else { return false }
        let physicalDirectory = String(cString: resolved)
        free(resolved)
        func pathsMatch() -> Bool {
            var actual = stat()
            guard lstat(directoryURL.path, &actual) == 0, sameFile(actual, directoryIdentity),
                  (actual.st_mode & S_IFMT) == S_IFDIR, actual.st_uid == geteuid(), (actual.st_mode & 0o777) == 0o700,
                  lstat(physicalDirectory, &actual) == 0, sameFile(actual, directoryIdentity),
                  (actual.st_mode & S_IFMT) == S_IFDIR,
                  fstatat(directory, name, &actual, AT_SYMLINK_NOFOLLOW) == 0, privateFile(actual), sameFile(actual, identity) else { return false }
            for suffix in ["-wal", "-shm", "-journal"] {
                if fstatat(directory, name + suffix, &actual, AT_SYMLINK_NOFOLLOW) == 0 {
                    if !privateFile(actual) { return false }
                } else if errno != ENOENT { return false }
            }
            return true
        }
        guard pathsMatch() else { return false }
        var database: OpaquePointer?
        let opened = sqlite3_open_v2(physicalDirectory + "/" + name, &database, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX | SQLITE_OPEN_NOFOLLOW, nil)
        defer { if let database { sqlite3_close(database) } }
        guard opened == SQLITE_OK, let database else { return false }
        sqlite3_busy_timeout(database, 50)
        sqlite3_progress_handler(database, 1_000, { context in
            guard let context else { return 0 }
            return Unmanaged<ScanCancellation>.fromOpaque(context).takeUnretainedValue().isCancelled || Task<Never, Never>.isCancelled ? 1 : 0
        }, Unmanaged.passUnretained(cancellation).toOpaque())
        var failedQuery = false
        func row(_ sql: String, path: String? = nil) -> OpaquePointer? {
            var statement: OpaquePointer?
            guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK, let statement else {
                failedQuery = true
                return nil
            }
            if let path {
                let bytes = Array(path.utf8)
                _ = bytes.withUnsafeBytes { sqlite3_bind_blob(statement, 1, $0.baseAddress, Int32($0.count), unsafeBitCast(-1, to: sqlite3_destructor_type.self)) }
            }
            let status = sqlite3_step(statement)
            guard status == SQLITE_ROW else {
                failedQuery = failedQuery || status != SQLITE_DONE
                sqlite3_finalize(statement)
                return nil
            }
            return statement
        }
        guard let application = row("PRAGMA application_id") else { return false }
        let validApplication = sqlite3_column_int64(application, 0) == InventorySchema.applicationID
        sqlite3_finalize(application)
        guard validApplication, let version = row("PRAGMA user_version") else { return false }
        let validVersion = sqlite3_column_int64(version, 0) == InventorySchema.version
        sqlite3_finalize(version)
        guard validVersion, let state = row("""
            SELECT d.device,d.inode,g.generation,m.root_path,EXISTS(SELECT 1 FROM sealed WHERE singleton=1)
            FROM scan s JOIN directories d ON d.path=s.root JOIN scan_generation g ON g.root=s.root
            JOIN metadata m ON m.singleton=1 WHERE s.root=?
            """, path: root.path) else { return false }
        defer { sqlite3_finalize(state) }
        guard Int32(sqlite3_column_int64(state, 0)) == info.device,
              UInt64(bitPattern: sqlite3_column_int64(state, 1)) == info.inode,
              let storedGeneration = sqlite3_column_text(state, 2), let storedRoot = sqlite3_column_text(state, 3),
              String(cString: storedRoot).utf8.elementsEqual(root.path.utf8),
              String(cString: storedGeneration) == generation(root: root, locks: locks) else { return false }
        if let node = row("SELECT n.is_dir,n.dev,n.inode FROM directory_paths p JOIN nodes n ON n.id=p.id WHERE p.path=CAST(? AS TEXT)", path: requested.path) {
            defer { sqlite3_finalize(node) }
            guard sqlite3_column_int(node, 0) == 1,
                  InventorySchema.identity(node, device: 1, inode: 2) == target.identity else { return false }
        } else if sqlite3_column_int(state, 4) != 0 { return false }
        try check(cancellation)
        return !failedQuery && pathsMatch()
    }
}
