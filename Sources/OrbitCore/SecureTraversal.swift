import Darwin
import Foundation

struct DirectoryIdentity: Hashable {
    let device: Int32
    let inode: UInt64
}

enum SecureTraversalError: Error {
    case invalidComponent
    case filesystem(Int32)
    case identityChanged
}

struct DirectoryComponent {
    let name: String
    let expected: DirectoryIdentity
}

final class SecureRoot {
    private let descriptor: Int32
    let identity: DirectoryIdentity
    let scope: VolumeTraversalScope
    let path: Data

    init(url: URL) throws {
        let fd = url.withUnsafeFileSystemRepresentation { path in
            open(path!, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        }
        guard fd >= 0 else { throw SecureTraversalError.filesystem(errno) }
        var info = stat()
        guard fstat(fd, &info) == 0 else {
            let failure = errno
            close(fd)
            throw SecureTraversalError.filesystem(failure)
        }
        descriptor = fd
        identity = DirectoryIdentity(device: info.st_dev, inode: info.st_ino)
        let rootURL = url.standardizedFileURL
        path = Data(rootURL.path.utf8)
        scope = VolumeTraversalScope(rootURL: rootURL, rootIdentity: identity)
    }

    deinit { close(descriptor) }

    func validate(url: URL) throws {
        var info = stat()
        guard url.withUnsafeFileSystemRepresentation({ lstat($0!, &info) }) == 0,
              info.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR),
              DirectoryIdentity(device: info.st_dev, inode: info.st_ino) == identity else {
            throw SecureTraversalError.identityChanged
        }
    }

    // Every component must match the metadata that originally put it on the queue.
    func openDirectory(_ components: [DirectoryComponent]) throws -> Int32 {
        var current = openat(descriptor, ".", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard current >= 0 else { throw SecureTraversalError.filesystem(errno) }
        do {
            var path = self.path
            for component in components {
                guard !component.name.isEmpty, component.name != ".", component.name != "..",
                      !component.name.contains("/"), !component.name.utf8.contains(0) else {
                    throw SecureTraversalError.invalidComponent
                }
                let next = component.name.withCString {
                    openat(current, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                }
                guard next >= 0 else { throw SecureTraversalError.filesystem(errno) }
                close(current)
                current = next
                path = HardLinkPathOrder.path(path, Data(component.name.utf8))
                var info = stat()
                guard fstat(current, &info) == 0 else { throw SecureTraversalError.filesystem(errno) }
                let actual = DirectoryIdentity(device: info.st_dev, inode: info.st_ino)
                guard actual == component.expected, scope.allowsIdentity(path: path, identity: actual) else {
                    throw SecureTraversalError.identityChanged
                }
            }
            return current
        } catch {
            close(current)
            throw error
        }
    }
}

// Confined to one discovery worker. Cached descriptors are never trusted without
// checking their current parent/name link against the queued directory identity.
final class SecureDirectoryCursor {
    private struct Cached {
        let component: DirectoryComponent
        let descriptor: Int32
    }

    private let root: SecureRoot
    private let rootURL: URL
    private let maximumCachedDirectories: Int
    private var rootDescriptor: Int32
    private var cached: [Cached] = []
    #if ORBIT_TESTING
    var testCachedDescriptors: [Int32] {
        rootDescriptor < 0 ? [] : [rootDescriptor] + cached.map(\.descriptor)
    }
    private(set) var testFallbackCount = 0
    #endif

    init(root: SecureRoot, rootURL: URL, maximumCachedDirectories: Int = 32) throws {
        try root.validate(url: rootURL)
        self.root = root
        self.rootURL = rootURL
        self.maximumCachedDirectories = max(1, min(maximumCachedDirectories, 64))
        self.rootDescriptor = try root.openDirectory([])
    }

    deinit { close() }

    func close() {
        discard(after: 0)
        if rootDescriptor >= 0 {
            Darwin.close(rootDescriptor)
            rootDescriptor = -1
        }
    }

    func openDirectory(_ components: [DirectoryComponent]) throws -> Int32 {
        guard rootDescriptor >= 0 else { throw SecureTraversalError.filesystem(EBADF) }
        do {
            try root.validate(url: rootURL)
            if components.count >= maximumCachedDirectories {
                #if ORBIT_TESTING
                testFallbackCount += 1
                #endif
                return try root.openDirectory(components)
            }
            for component in components {
                guard !component.name.isEmpty, component.name != ".", component.name != "..",
                      !component.name.contains("/"), !component.name.utf8.contains(0) else {
                    throw SecureTraversalError.invalidComponent
                }
            }
            var prefix = 0
            while prefix < min(cached.count, components.count),
                  cached[prefix].component.expected == components[prefix].expected,
                  cached[prefix].component.name.utf8.elementsEqual(components[prefix].name.utf8) {
                prefix += 1
            }
            discard(after: prefix)
            var parent = rootDescriptor
            var path = root.path
            for offset in 0..<prefix {
                let component = components[offset]
                path = HardLinkPathOrder.path(path, Data(component.name.utf8))
                var info = stat()
                let status = component.name.withCString { fstatat(parent, $0, &info, AT_SYMLINK_NOFOLLOW) }
                guard status == 0 else { throw SecureTraversalError.filesystem(errno) }
                try validate(info, path: path, expected: component.expected)
                parent = cached[offset].descriptor
            }
            for component in components.dropFirst(prefix) {
                path = HardLinkPathOrder.path(path, Data(component.name.utf8))
                let next = component.name.withCString {
                    openat(parent, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                }
                guard next >= 0 else { throw SecureTraversalError.filesystem(errno) }
                do {
                    var info = stat()
                    guard fstat(next, &info) == 0 else { throw SecureTraversalError.filesystem(errno) }
                    try validate(info, path: path, expected: component.expected)
                } catch {
                    Darwin.close(next)
                    throw error
                }
                cached.append(Cached(component: component, descriptor: next))
                parent = next
            }
            // dup() would share the cached descriptor's enumeration offset.
            let result = openat(parent, ".", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard result >= 0 else { throw SecureTraversalError.filesystem(errno) }
            return result
        } catch {
            discard(after: 0)
            throw error
        }
    }

    private func validate(_ info: stat, path: Data, expected: DirectoryIdentity) throws {
        let actual = DirectoryIdentity(device: info.st_dev, inode: info.st_ino)
        guard info.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR), actual == expected,
              root.scope.allowsIdentity(path: path, identity: actual) else {
            throw SecureTraversalError.identityChanged
        }
    }

    private func discard(after count: Int) {
        while cached.count > count { Darwin.close(cached.removeLast().descriptor) }
    }
}
