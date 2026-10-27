import Darwin
import Foundation
import OrbitCore

struct RefreshIndexMetadataCache {
    private var indexed: [Data: (isDirectory: Bool, isAccessible: Bool)] = [:]
    private var missing = Set<Data>()

    mutating func lookup(_ path: String, read: () throws -> (isDirectory: Bool, isAccessible: Bool)?) rethrows -> (isDirectory: Bool, isAccessible: Bool)? {
        let key = Data(path.utf8)
        if let value = indexed[key] { return value }
        if missing.contains(key) { return nil }
        guard let value = try read() else { missing.insert(key); return nil }
        indexed[key] = value
        return value
    }
}

func refreshDirectories(_ paths: Set<String>, store: DiskIndexStore, snapshot: IndexSnapshot,
                        cancellation: ScanCancellation? = nil) throws -> [String] {
    func checkCancellation() throws {
        if Task.isCancelled || cancellation?.isCancelled == true { throw CancellationError() }
    }
    try checkCancellation()
    let root = snapshot.rootURL.standardizedFileURL.path
    let canonicalRoot = snapshot.rootURL.resolvingSymlinksInPath().path
    var rootInfo = stat()
    guard lstat(root, &rootInfo) == 0 else { throw ScanError.filesystem(root, errno) }
    guard rootInfo.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR) else {
        throw ScanError.filesystem(root, rootInfo.st_mode & mode_t(S_IFMT) == mode_t(S_IFLNK) ? ELOOP : ENOTDIR)
    }

    func contains(_ path: String, in ancestor: String) -> Bool {
        path == ancestor || (ancestor == "/" ? path.hasPrefix("/") : path.hasPrefix(ancestor + "/"))
    }
    func parent(_ path: String) -> String {
        let candidate = URL(fileURLWithPath: path).deletingLastPathComponent().path
        return contains(candidate, in: root) ? candidate : root
    }
    func mountPath(_ filesystem: statfs) -> String {
        var name = filesystem.f_mntonname
        return withUnsafeBytes(of: &name) { String(cString: $0.baseAddress!.assumingMemoryBound(to: CChar.self)) }
    }

    // Index metadata is immutable for a snapshot; filesystem checks still run for each path.
    var cache = RefreshIndexMetadataCache()
    func indexedNode(_ path: String) throws -> (isDirectory: Bool, isAccessible: Bool)? {
        try checkCancellation()
        return try cache.lookup(path) {
            guard let node = try store.node(path: path, snapshot: snapshot, cancellation: cancellation) else { return nil }
            return (isDirectory: node.isDirectory, isAccessible: node.isAccessible)
        }
    }

    var candidates = Set<String>()
    for original in paths {
        try checkCancellation()
        let original = URL(fileURLWithPath: original).standardizedFileURL.path
        guard contains(original, in: root) else { continue }
        var candidate = original
        while try indexedNode(candidate)?.isDirectory != true {
            guard candidate != root else { throw ScanError.filesystem(root, ENOENT) }
            candidate = parent(candidate)
        }

        var ancestors: [String] = []
        var current = candidate
        while current != root {
            ancestors.append(current)
            current = parent(current)
        }
        for path in ancestors.reversed() {
            if try indexedNode(path)?.isAccessible == false {
                candidate = parent(path)
                break
            }
            var info = stat()
            if lstat(path, &info) != 0 || info.st_mode & mode_t(S_IFMT) != mode_t(S_IFDIR)
                || info.st_dev != rootInfo.st_dev {
                candidate = parent(path)
                break
            }
            var filesystem = statfs()
            guard statfs(path, &filesystem) == 0 else { throw ScanError.filesystem(path, errno) }
            let suffix = root == "/" ? path : String(path.dropFirst(root.count))
            let physicalPath = canonicalRoot == "/" ? suffix : canonicalRoot + suffix
            if mountPath(filesystem) == physicalPath {
                candidate = parent(path)
                break
            }
        }
        candidates.insert(candidate)
    }
    var minimal: [String] = []
    var selected = Set<String>()
    for path in candidates.sorted(by: { $0.count == $1.count ? $0 < $1 : $0.count < $1.count }) {
        try checkCancellation()
        var current = path
        var covered = false
        while current != root {
            current = parent(current)
            if selected.contains(current) { covered = true; break }
        }
        if !covered { minimal.append(path); selected.insert(path) }
    }
    return minimal
}
