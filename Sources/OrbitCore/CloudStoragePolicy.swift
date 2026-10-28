import Foundation
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#endif

public enum CloudStoragePolicy {
    public static let excludedMessage = "Cloud storage is excluded from mapping and collection."
    private static let datalessMessage = "This cloud-only location is excluded; Orbit Disk will not download it."

    #if ORBIT_TESTING
    package static var testScanExclusions: [URL]?
    #endif

    public static func isKnownCloudPath(_ path: String, home: URL = FileManager.default.homeDirectoryForCurrentUser) -> Bool {
        knownRoots(home: home).contains { contains(path, root: $0.path) }
    }

    public static func scanExclusions(home: URL = FileManager.default.homeDirectoryForCurrentUser) -> [URL] {
        #if ORBIT_TESTING
        if let roots = testScanExclusions { return roots }
        #endif
        return scanExclusions(home: home, probe: ubiquitousStatus)
    }

    package static func scanExclusions(home: URL, probe: (URL) throws -> Bool?) -> [URL] {
        let standard = knownRoots(home: home)
        return (try? withoutMaterialization {
            var roots = standard
            for name in ["Desktop", "Documents"] {
                let candidate = home.appendingPathComponent(name, isDirectory: true).standardizedFileURL
                do {
                    if try probe(candidate) == true { roots.append(candidate) }
                } catch {
                    if isDatalessError(error) { roots.append(candidate) }
                }
            }
            var seen = Set<Data>()
            return roots.flatMap { [$0.standardizedFileURL, $0.standardizedFileURL.resolvingSymlinksInPath()] }
                .filter { seen.insert(Data($0.path.utf8)).inserted }
        }) ?? standard
    }

    public static func exclusionReason(for url: URL,
                                       home: URL = FileManager.default.homeDirectoryForCurrentUser,
                                       excluding roots: [URL]? = nil,
                                       protectContainingRoots: Bool = false) -> String? {
        Validator(home: home, excluding: roots, protectContainingRoots: protectContainingRoots)
            .exclusionReason(for: url)
    }

    package static func exclusionReason(for url: URL, home: URL, excluding roots: [URL],
                                        protectContainingRoots: Bool,
                                        probe: @escaping (URL) throws -> Bool?) -> String? {
        Validator(home: home, excluding: roots, protectContainingRoots: protectContainingRoots, probe: probe)
            .exclusionReason(for: url)
    }

    // Discard after each synchronous collection-validation batch. Actual removal must recheck fresh ancestry.
    public final class Validator {
        private enum Decision {
            case local
            case excluded(String)

            var reason: String? {
                if case .excluded(let reason) = self { return reason }
                return nil
            }
        }

        private let roots: [URL]
        private let protectContainingRoots: Bool
        private let probe: (URL) throws -> Bool?
        private var rootIdentities = Set<DirectoryIdentity>()
        private var containingIdentities = Set<DirectoryIdentity>()
        private var ancestors: [Data: Decision] = [:]

        public convenience init(home: URL = FileManager.default.homeDirectoryForCurrentUser,
                                excluding roots: [URL]? = nil, protectContainingRoots: Bool = true) {
            self.init(home: home, excluding: roots ?? scanExclusions(home: home),
                      protectContainingRoots: protectContainingRoots, probe: ubiquitousStatus)
        }

        package init(home: URL, excluding roots: [URL], protectContainingRoots: Bool,
                     probe: @escaping (URL) throws -> Bool?) {
            self.roots = roots.map { $0.standardizedFileURL }
            self.protectContainingRoots = protectContainingRoots
            self.probe = probe
            try? withoutMaterialization {
                rootIdentities = Set(self.roots.compactMap(directoryIdentity))
                if protectContainingRoots {
                    var visited = Set<Data>()
                    for root in self.roots {
                        var ancestor = root
                        while visited.insert(Data(ancestor.path.utf8)).inserted {
                            if let identity = directoryIdentity(ancestor) { containingIdentities.insert(identity) }
                            guard ancestor.path != "/" else { break }
                            ancestor.deleteLastPathComponent()
                        }
                    }
                }
            }
        }

        public func exclusionReason(for url: URL) -> String? {
            guard url.isFileURL else { return nil }
            let original = url.standardizedFileURL
            if overlaps(original.path) { return excludedMessage }
            do {
                return try withoutMaterialization {
                    let target = original.resolvingSymlinksInPath()
                    if overlaps(target.path) { return excludedMessage }
                    // Every requested item is fresh, even if it was an ancestor earlier in this batch.
                    if let reason = classify(target, includeContainingRoots: protectContainingRoots).reason { return reason }
                    guard target.path != "/" else { return nil }
                    var ancestor = target.deletingLastPathComponent()
                    var visited: [Data] = []
                    var result = Decision.local
                    while true {
                        let key = Data(ancestor.path.utf8)
                        if let cached = ancestors[key] {
                            result = cached
                            break
                        }
                        visited.append(key)
                        result = classify(ancestor)
                        if result.reason != nil || ancestor.path == "/" { break }
                        let parent = ancestor.deletingLastPathComponent()
                        guard parent.path != ancestor.path else { break }
                        ancestor = parent
                    }
                    for key in visited { ancestors[key] = result }
                    return result.reason
                }
            } catch { return isDatalessError(error) ? datalessMessage : nil }
        }

        private func overlaps(_ path: String) -> Bool {
            roots.contains { contains(path, root: $0.path) || (protectContainingRoots && contains($0.path, root: path)) }
        }

        private func classify(_ url: URL, includeContainingRoots: Bool = false) -> Decision {
            if let identity = directoryIdentity(url),
               rootIdentities.contains(identity) || (includeContainingRoots && containingIdentities.contains(identity)) {
                return .excluded(excludedMessage)
            }
            do {
                if try probe(url) == true { return .excluded(excludedMessage) }
            } catch {
                if isDatalessError(error) { return .excluded(datalessMessage) }
            }
            return .local
        }
    }

    // The closure must stay synchronous: this policy belongs to the current OS thread.
    public static func withoutMaterialization<T>(_ body: () throws -> T) throws -> T {
        #if canImport(Darwin)
        let previous = getiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD)
        guard previous >= 0 else { throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno)) }
        if previous != IOPOL_MATERIALIZE_DATALESS_FILES_OFF {
            guard setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD,
                                IOPOL_MATERIALIZE_DATALESS_FILES_OFF) == 0 else {
                throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
            }
        }
        defer {
            if previous != IOPOL_MATERIALIZE_DATALESS_FILES_OFF {
                _ = setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD, previous)
            }
        }
        #endif
        return try body()
    }

    private static func knownRoots(home: URL) -> [URL] {
        ["Library/CloudStorage", "Library/Mobile Documents"].map {
            home.appendingPathComponent($0, isDirectory: true).standardizedFileURL
        }
    }

    private static func contains(_ path: String, root: String) -> Bool {
        path.utf8.elementsEqual(root.utf8) || root == "/" || path.utf8.starts(with: (root + "/").utf8)
    }

    private struct DirectoryIdentity: Hashable {
        let device: UInt64
        let inode: UInt64
    }

    private static func directoryIdentity(_ url: URL) -> DirectoryIdentity? {
        var info = stat()
        guard url.withUnsafeFileSystemRepresentation({ stat($0!, &info) }) == 0,
              info.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR) else { return nil }
        return DirectoryIdentity(device: UInt64(truncatingIfNeeded: info.st_dev), inode: UInt64(truncatingIfNeeded: info.st_ino))
    }

    private static func ubiquitousStatus(_ url: URL) throws -> Bool? {
        #if canImport(Darwin)
        var info = stat()
        let result = url.withUnsafeFileSystemRepresentation { lstat($0!, &info) }
        if result == 0, info.st_flags & UInt32(SF_DATALESS) != 0 {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(EDEADLK))
        }
        if result != 0, errno == EDEADLK { throw NSError(domain: NSPOSIXErrorDomain, code: Int(EDEADLK)) }
        return try url.resourceValues(forKeys: [.isUbiquitousItemKey]).isUbiquitousItem
        #else
        return nil
        #endif
    }

    private static func isDatalessError(_ error: Error) -> Bool {
        var current = error as NSError
        for _ in 0..<8 {
            if current.domain == NSPOSIXErrorDomain, current.code == Int(EDEADLK) { return true }
            guard let underlying = current.userInfo[NSUnderlyingErrorKey] as? NSError else { return false }
            current = underlying
        }
        return false
    }
}
