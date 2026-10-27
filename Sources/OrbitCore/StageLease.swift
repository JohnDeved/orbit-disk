import CryptoKit
import CSQLite
import Darwin
import Foundation

// Retain this lease until every SQLite connection to the stage is closed.
final class StageLease {
    enum Failure: Error { case unsafe(String), busy, incompatible }
    let databasePath: String
    private let dirFD: Int32
    private let lockFD: Int32
    private let databaseFD: Int32
    private let name: String
    private let lockName: String
    private let expectedLock: stat
    private let directoryPath: String
    private let expectedDirectory: stat
    private let expectedDatabase: stat

    static func key(root: URL) throws -> String {
        let root = root.standardizedFileURL
        var info = stat()
        guard lstat(root.path, &info) == 0, (info.st_mode & S_IFMT) == S_IFDIR else {
            throw Failure.unsafe("root identity")
        }
        var bytes = Data(root.path.utf8)
        bytes.append(0)
        bytes.append(contentsOf: "\(info.st_dev):\(info.st_ino)".utf8)
        return SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
    }

    init(cache: URL, root: URL) throws {
        let baseFD = open(cache.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard baseFD >= 0 else { throw Failure.unsafe("cache directory") }
        defer { close(baseFD) }
        if mkdirat(baseFD, ".orbit-stages", 0o700) != 0 && errno != EEXIST { throw Failure.unsafe("create stage directory") }
        let dir = openat(baseFD, ".orbit-stages", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard dir >= 0 else { throw Failure.unsafe("open stage directory") }
        var keepDir = false
        defer { if !keepDir { close(dir) } }
        var info = stat()
        guard fstat(dir, &info) == 0, info.st_uid == geteuid(), (info.st_mode & 0o777) == 0o700 else {
            throw Failure.unsafe("stage directory permissions")
        }
        let digest = try Self.key(root: root)
        let lockName = digest + ".lock"
        let fileName = digest + ".sqlite"
        let lock = openat(dir, lockName, O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard lock >= 0 else { throw Failure.unsafe("open lock") }
        var keepLock = false
        defer { if !keepLock { close(lock) } }
        var lockInfo = stat()
        guard fstat(lock, &lockInfo) == 0, (lockInfo.st_mode & S_IFMT) == S_IFREG,
              lockInfo.st_uid == geteuid(), (lockInfo.st_mode & 0o777) == 0o600, lockInfo.st_nlink == 1 else {
            throw Failure.unsafe("lock identity")
        }
        guard flock(lock, LOCK_EX | LOCK_NB) == 0 else { throw Failure.busy }
        let file = openat(dir, fileName, O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard file >= 0 else { throw Failure.unsafe("open database") }
        var keepFile = false
        defer { if !keepFile { close(file) } }
        var dbInfo = stat()
        guard fstat(file, &dbInfo) == 0, (dbInfo.st_mode & S_IFMT) == S_IFREG,
              dbInfo.st_uid == geteuid(), (dbInfo.st_mode & 0o777) == 0o600, dbInfo.st_nlink == 1 else {
            throw Failure.unsafe("database identity")
        }
        databasePath = cache.appendingPathComponent(".orbit-stages", isDirectory: true).appendingPathComponent(fileName).path
        directoryPath = cache.appendingPathComponent(".orbit-stages", isDirectory: true).path
        expectedDirectory = info; expectedDatabase = dbInfo
        dirFD = dir; lockFD = lock; databaseFD = file; name = fileName
        self.lockName = lockName; expectedLock = lockInfo
        keepDir = true; keepLock = true; keepFile = true
        do {
            try validatePaths()
            if dbInfo.st_size != 0 { try validateSchema() }
            try validatePaths()
        } catch {
            keepDir = false; keepLock = false; keepFile = false
            throw error
        }
    }
    deinit { close(databaseFD); close(lockFD); close(dirFD) }

    func validatePaths() throws {
        var actualDirectory = stat(), actualDatabase = stat(), heldDirectory = stat(), heldDatabase = stat(), actualLock = stat()
        guard lstat(directoryPath, &actualDirectory) == 0, fstat(dirFD, &heldDirectory) == 0,
              (actualDirectory.st_mode & S_IFMT) == S_IFDIR,
              actualDirectory.st_dev == expectedDirectory.st_dev, actualDirectory.st_ino == expectedDirectory.st_ino,
              heldDirectory.st_dev == expectedDirectory.st_dev, heldDirectory.st_ino == expectedDirectory.st_ino,
              actualDirectory.st_uid == geteuid(), (actualDirectory.st_mode & 0o777) == 0o700,
              fstatat(dirFD, lockName, &actualLock, AT_SYMLINK_NOFOLLOW) == 0,
              (actualLock.st_mode & S_IFMT) == S_IFREG, actualLock.st_uid == geteuid(), actualLock.st_nlink == 1,
              (actualLock.st_mode & 0o777) == 0o600, actualLock.st_dev == expectedLock.st_dev, actualLock.st_ino == expectedLock.st_ino,
              fstatat(dirFD, name, &actualDatabase, AT_SYMLINK_NOFOLLOW) == 0, fstat(databaseFD, &heldDatabase) == 0,
              (actualDatabase.st_mode & S_IFMT) == S_IFREG, actualDatabase.st_uid == geteuid(),
              (actualDatabase.st_mode & 0o777) == 0o600, actualDatabase.st_nlink == 1,
              actualDatabase.st_dev == expectedDatabase.st_dev, actualDatabase.st_ino == expectedDatabase.st_ino,
              heldDatabase.st_dev == expectedDatabase.st_dev, heldDatabase.st_ino == expectedDatabase.st_ino else {
            throw Failure.unsafe("stage replaced")
        }
        try validateSidecars()
    }

    func removeStageFiles() throws {
        try validatePaths()
        for suffix in ["-wal", "-shm", "-journal", ""] {
            let file = name + suffix
            var info = stat()
            if fstatat(dirFD, file, &info, AT_SYMLINK_NOFOLLOW) != 0 {
                guard errno == ENOENT else { throw Failure.unsafe("stage cleanup lookup") }
                continue
            }
            guard (info.st_mode & S_IFMT) == S_IFREG, info.st_uid == geteuid(),
                  info.st_nlink == 1, (info.st_mode & 0o777) == 0o600 else {
                throw Failure.unsafe("unsafe stage cleanup")
            }
            if suffix.isEmpty {
                guard info.st_dev == expectedDatabase.st_dev && info.st_ino == expectedDatabase.st_ino else { throw Failure.unsafe("stage replaced") }
            }
            guard unlinkat(dirFD, file, 0) == 0 else { throw Failure.unsafe("stage cleanup") }
        }
    }

    private func validateSidecars() throws {
        for suffix in ["-wal", "-shm", "-journal"] {
            var info = stat()
            if fstatat(dirFD, name + suffix, &info, AT_SYMLINK_NOFOLLOW) != 0 {
                guard errno == ENOENT else { throw Failure.unsafe("stage sidecar lookup") }
                continue
            }
            guard (info.st_mode & S_IFMT) == S_IFREG, info.st_uid == geteuid(),
                  info.st_nlink == 1, (info.st_mode & 0o777) == 0o600 else {
                throw Failure.unsafe("unsafe stage sidecar")
            }
        }
    }

    private func validateSchema() throws {
        var db: OpaquePointer?
        guard sqlite3_open_v2(databasePath, &db, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK else {
            if let db { sqlite3_close(db) }; throw Failure.incompatible
        }
        defer { sqlite3_close(db) }
        func integer(_ sql: String) throws -> Int64 {
            var statement: OpaquePointer?
            guard sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK, let statement else { throw Failure.incompatible }
            defer { sqlite3_finalize(statement) }
            guard sqlite3_step(statement) == SQLITE_ROW else { throw Failure.incompatible }
            return sqlite3_column_int64(statement, 0)
        }
        let applicationID = try integer("PRAGMA application_id")
        let version = try integer("PRAGMA user_version")
        let stageTables = try integer("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('scan','nodes','directories')")
        let userObjects = try integer("SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
        let empty = applicationID == 0 && version == 0 && userObjects == 0
        guard empty || (applicationID == 0x4F524254 && version == 3 && stageTables == 3) else {
            throw Failure.incompatible
        }
    }
}
