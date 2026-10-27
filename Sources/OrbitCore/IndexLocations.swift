import Darwin
import Foundation

enum IndexLocations {
    private static let dataVolume = "/System/Volumes/Data"
    private static func sameFile(_ a: String, _ b: String) -> Bool {
        var first = stat(), second = stat()
        return stat(a, &first) == 0 && stat(b, &second) == 0 && first.st_dev == second.st_dev && first.st_ino == second.st_ino
    }
    private static func matchesThroughAncestor(_ first: String, _ second: String) -> Bool {
        var a = first, b = second
        while b != "/" {
            if sameFile(a, b) { return true }
            a = URL(fileURLWithPath: a).deletingLastPathComponent().path
            b = URL(fileURLWithPath: b).deletingLastPathComponent().path
        }
        return false
    }
    static func exclusionPaths(_ urls: [URL]) -> Set<Data> {
        var paths = Set<Data>()
        for url in urls {
            for path in [url.standardizedFileURL.path, url.resolvingSymlinksInPath().path] {
                paths.insert(Data(path.utf8))
                if path.hasPrefix(dataVolume + "/") {
                    let native = String(path.dropFirst(dataVolume.count))
                    if matchesThroughAncestor(path, native) { paths.insert(Data(native.utf8)) }
                } else if path.hasPrefix("/"), path != "/" {
                    let physical = dataVolume + path
                    if matchesThroughAncestor(physical, path) { paths.insert(Data(physical.utf8)) }
                }
            }
        }
        return paths
    }
    // Resolve existing ancestors even when the cache itself has not been created yet.
    static func canonicalCache(_ url: URL) -> URL {
        let input = url.standardizedFileURL
        guard input.path != "/" else { return input }
        var current = input.deletingLastPathComponent(), suffix = [input.lastPathComponent]
        var info = stat()
        while lstat(current.path, &info) != 0 && current.path != "/" {
            suffix.append(current.lastPathComponent)
            current.deleteLastPathComponent()
        }
        if let resolved = realpath(current.path, nil) {
            defer { free(resolved) }
            current = URL(fileURLWithPath: String(cString: resolved), isDirectory: true)
        }
        for component in suffix.reversed() { current.appendPathComponent(component, isDirectory: true) }
        let path = current.path
        if path.hasPrefix(dataVolume + "/") {
            let native = String(path.dropFirst(dataVolume.count))
            if matchesThroughAncestor(path, native) { return URL(fileURLWithPath: native, isDirectory: true) }
        }
        return current
    }
    static func locks(for cache: URL) -> URL {
        let cache = canonicalCache(cache)
        let caches = canonicalCache(FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0])
        let parent = cache.deletingLastPathComponent()
        if cache.lastPathComponent == "Indexes", parent.deletingLastPathComponent().path == caches.path {
            return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent(parent.lastPathComponent, isDirectory: true)
                .appendingPathComponent("Mapping", isDirectory: true).standardizedFileURL
        }
        return cache.appendingPathExtension("staging")
    }
    static func validateCache(_ cache: URL) throws {
        var info = stat()
        guard lstat(cache.path, &info) == 0, (info.st_mode & S_IFMT) == S_IFDIR,
              info.st_uid == geteuid() else { throw DiskIndexError.invalid("The index directory is not a safe folder.") }
    }
}
