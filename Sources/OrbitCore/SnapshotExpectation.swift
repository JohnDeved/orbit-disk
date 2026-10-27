import CryptoKit
import CSQLite
import Darwin
import Foundation

public enum SnapshotExpectation: Equatable {
    case absent
    case matching(String)
    case invalid(String)
    enum Failure: Error { case unreadable, changed }
    var encoded: String {
        switch self { case .absent: return "absent"; case .matching(let id): return "id:" + id; case .invalid(let fingerprint): return "invalid:" + fingerprint }
    }
    init?(encoded: String) {
        if encoded == "absent" { self = .absent }
        else if encoded.hasPrefix("id:"), encoded.count > 3 { self = .matching(String(encoded.dropFirst(3))) }
        else if encoded.hasPrefix("invalid:"), encoded.count > 8 { self = .invalid(String(encoded.dropFirst(8))) }
        else { return nil }
    }

    // Call only while holding the root mutation lease.
    static func capture(directory: URL, root: URL) throws -> Self {
        let digest = SHA256.hash(data: Data(root.standardizedFileURL.path.utf8)).map { String(format: "%02x", $0) }.joined()
        let path = directory.appendingPathComponent(digest + ".sqlite").path
        var info = stat()
        if lstat(path, &info) != 0 {
            guard errno == ENOENT else { throw Failure.unreadable }
            return .absent
        }
        guard (info.st_mode & S_IFMT) == S_IFREG, info.st_uid == geteuid(), info.st_nlink == 1 else { throw Failure.unreadable }
        if let snapshot = DiskIndexStore(directory: directory).savedSnapshot(for: root) { return .matching(snapshot.id) }
        // Retain an unreadable owned cache until its replacement is ready. The
        // fingerprint detects replacement or mutation while mapping proceeds.
        return .invalid("\(info.st_dev):\(info.st_ino):\(info.st_size):\(info.st_mtimespec.tv_sec):\(info.st_mtimespec.tv_nsec):\(info.st_ctimespec.tv_sec):\(info.st_ctimespec.tv_nsec)")
    }
    func verify(directory: URL, root: URL) throws {
        guard try Self.capture(directory: directory, root: root) == self else { throw Failure.changed }
    }

    // The caller holds the mutation lease. Updates clone the snapshot, retaining
    // its sealed-stage ID even if the snapshot's own ID has since changed.
    static func descendsFromStage(_ id: String, directory: URL, root: URL) throws -> Bool {
        guard let snapshot = DiskIndexStore(directory: directory).savedSnapshot(for: root) else { return false }
        var db: OpaquePointer?
        guard sqlite3_open_v2(snapshot.databaseURL.path, &db, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else {
            if let db { sqlite3_close(db) }
            throw Failure.unreadable
        }
        defer { sqlite3_close(db) }
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, "SELECT 1 FROM sqlite_master WHERE type='table' AND name='sealed'", -1, &stmt, nil) == SQLITE_OK else { throw Failure.unreadable }
        let tableResult = sqlite3_step(stmt)
        sqlite3_finalize(stmt)
        guard tableResult == SQLITE_ROW else {
            guard tableResult == SQLITE_DONE else { throw Failure.unreadable }
            return false
        }
        guard sqlite3_prepare_v2(db, "SELECT snapshot_id FROM sealed WHERE singleton=1", -1, &stmt, nil) == SQLITE_OK else { throw Failure.unreadable }
        defer { sqlite3_finalize(stmt) }
        let result = sqlite3_step(stmt)
        guard result == SQLITE_ROW, let value = sqlite3_column_text(stmt, 0) else {
            guard result == SQLITE_DONE else { throw Failure.unreadable }
            return false
        }
        return Data(String(cString: value).utf8) == Data(id.utf8)
    }
}
