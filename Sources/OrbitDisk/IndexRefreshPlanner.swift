import Darwin
import Foundation
import OrbitCore

func refreshDirectories(_ paths: Set<String>, store: DiskIndexStore, snapshot: IndexSnapshot) throws -> [String] {
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

    var candidates = Set<String>()
    for original in paths {
        let original = URL(fileURLWithPath: original).standardizedFileURL.path
        guard contains(original, in: root) else { continue }
        var candidate = original
        while try store.node(path: candidate, snapshot: snapshot)?.isDirectory != true {
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
            if try store.node(path: path, snapshot: snapshot)?.isAccessible == false {
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
    for path in candidates.sorted(by: { $0.count == $1.count ? $0 < $1 : $0.count < $1.count }) {
        if !minimal.contains(where: { contains(path, in: $0) }) { minimal.append(path) }
    }
    return minimal
}
