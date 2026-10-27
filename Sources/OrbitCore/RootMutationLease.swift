import CryptoKit
import Darwin
import Foundation

// Every index writer for a given cache and root must hold this cooperative lock.
final class RootMutationLease {
    enum Failure: Error { case unsafe(String), lock(Int32) }
    private let dirFD: Int32
    private let fd: Int32
    private let lockName: String
    private let directoryPath: String
    private let directoryIdentity: stat
    private let lockIdentity: stat

    init(directory: URL, root: URL, cancellation: ScanCancellation = ScanCancellation()) throws {
        let parent = open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard parent >= 0 else { throw Failure.unsafe("cache directory") }
        defer { close(parent) }
        if mkdirat(parent, ".orbit-locks", 0o700) != 0 && errno != EEXIST { throw Failure.unsafe("create lock directory") }
        let dir = openat(parent, ".orbit-locks", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard dir >= 0 else { throw Failure.unsafe("open lock directory") }
        var retainDir = false
        defer { if !retainDir { close(dir) } }
        var info = stat()
        guard fstat(dir, &info) == 0, info.st_uid == geteuid(), (info.st_mode & 0o777) == 0o700 else {
            throw Failure.unsafe("lock directory permissions")
        }
        let name = SHA256.hash(data: Data(root.standardizedFileURL.path.utf8)).map { String(format: "%02x", $0) }.joined() + ".lock"
        let lock = openat(dir, name, O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard lock >= 0 else { throw Failure.unsafe("open root lock") }
        var retainLock = false
        defer { if !retainLock { close(lock) } }
        var lockInfo = stat()
        guard fstat(lock, &lockInfo) == 0, (lockInfo.st_mode & S_IFMT) == S_IFREG,
              lockInfo.st_uid == geteuid(), lockInfo.st_nlink == 1, (lockInfo.st_mode & 0o777) == 0o600 else {
            throw Failure.unsafe("root lock identity")
        }
        while flock(lock, LOCK_EX | LOCK_NB) != 0 {
            let failure = errno
            if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
            guard failure == EWOULDBLOCK || failure == EINTR else { throw Failure.lock(failure) }
            usleep(10_000)
        }
        dirFD = dir; fd = lock; lockName = name
        directoryPath = directory.appendingPathComponent(".orbit-locks", isDirectory: true).path
        directoryIdentity = info; lockIdentity = lockInfo
        retainDir = true; retainLock = true
        do { try validate() } catch { retainDir = false; retainLock = false; throw error }
    }
    deinit { close(fd); close(dirFD) }
    func invalidationGeneration() throws -> String {
        try validate()
        let name = lockName + ".generation"
        let marker = openat(dirFD, name, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        if marker < 0 {
            if errno == ENOENT { return "0" }
            throw Failure.unsafe("invalidation generation")
        }
        defer { close(marker) }
        var info = stat(), pathInfo = stat()
        guard fstat(marker, &info) == 0, fstatat(dirFD, name, &pathInfo, AT_SYMLINK_NOFOLLOW) == 0,
              (info.st_mode & S_IFMT) == S_IFREG, info.st_uid == geteuid(), info.st_nlink == 1,
              (info.st_mode & 0o777) == 0o600, info.st_dev == pathInfo.st_dev, info.st_ino == pathInfo.st_ino else {
            throw Failure.unsafe("invalidation generation identity")
        }
        var bytes = [UInt8](repeating: 0, count: 64)
        let count = bytes.withUnsafeMutableBytes { read(marker, $0.baseAddress, $0.count) }
        guard count > 0, count < bytes.count,
              let generation = String(bytes: bytes.prefix(count), encoding: .utf8), UUID(uuidString: generation) != nil else {
            throw Failure.unsafe("invalidation generation contents")
        }
        return generation
    }

    func invalidateGeneration() throws {
        _ = try invalidationGeneration()
        let name = lockName + ".generation", temporary = ".generation-" + UUID().uuidString
        let marker = openat(dirFD, temporary, O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard marker >= 0 else { throw Failure.unsafe("create invalidation generation") }
        defer { close(marker); _ = unlinkat(dirFD, temporary, 0) }
        let bytes = Array(UUID().uuidString.utf8)
        let written = bytes.withUnsafeBytes { write(marker, $0.baseAddress, $0.count) }
        guard written == bytes.count, fsync(marker) == 0 else { throw Failure.unsafe("write invalidation generation") }
        try validate()
        guard renameat(dirFD, temporary, dirFD, name) == 0, fsync(dirFD) == 0 else {
            throw Failure.unsafe("commit invalidation generation")
        }
    }
    func validate() throws {
        var directory = stat(), file = stat()
        guard lstat(directoryPath, &directory) == 0, (directory.st_mode & S_IFMT) == S_IFDIR,
              directory.st_dev == directoryIdentity.st_dev, directory.st_ino == directoryIdentity.st_ino,
              directory.st_uid == geteuid(), (directory.st_mode & 0o777) == 0o700,
              fstatat(dirFD, lockName, &file, AT_SYMLINK_NOFOLLOW) == 0, (file.st_mode & S_IFMT) == S_IFREG,
              file.st_dev == lockIdentity.st_dev, file.st_ino == lockIdentity.st_ino,
              file.st_uid == geteuid(), file.st_nlink == 1, (file.st_mode & 0o777) == 0o600 else {
            throw Failure.unsafe("root lock replaced")
        }
    }
}
