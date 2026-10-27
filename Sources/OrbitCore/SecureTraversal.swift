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
                var info = stat()
                guard fstat(current, &info) == 0 else { throw SecureTraversalError.filesystem(errno) }
                guard info.st_dev == identity.device,
                      DirectoryIdentity(device: info.st_dev, inode: info.st_ino) == component.expected else {
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
