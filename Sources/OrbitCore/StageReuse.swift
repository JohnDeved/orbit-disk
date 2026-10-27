import CSQLite
import Darwin
import Foundation

public struct StageReuseCandidate: Sendable {
    public let snapshot: IndexSnapshot
    public let checkpoint: StageCheckpoint
    public init(snapshot: IndexSnapshot, checkpoint: StageCheckpoint) {
        self.snapshot = snapshot; self.checkpoint = checkpoint
    }
}

// A read transaction pins a published version while another mapping replaces its file.
final class StageReuseSource {
    enum Failure: Error { case unsuitable, changed }
    let snapshot: IndexSnapshot
    let checkpoint: StageCheckpoint
    let path: Data
    private var db: OpaquePointer?
    private let cancellation: ScanCancellation
    private let journal: RemovalJournal
    private let parentRoot: URL
    private let sourceBinding: Data
    private let parentBinding: Data
    private var cursor: RemovalCursor
    private(set) var rejected = false
    private let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

    init(_ candidate: StageReuseCandidate, cache: URL, parentRoot: URL, parentIdentity: String,
         cancellation: ScanCancellation) throws {
        let snapshot = candidate.snapshot
        self.snapshot = snapshot; checkpoint = candidate.checkpoint
        path = Data(snapshot.rootURL.path.utf8)
        self.cancellation = cancellation; self.parentRoot = parentRoot
        journal = try RemovalJournal(cache: cache)
        guard snapshot.skippedCount == 0,
              Data(snapshot.rootURL.path.utf8) != Data(parentRoot.path.utf8),
              try MappingCoverage.canReach(root: parentRoot, target: snapshot.rootURL,
                  expectedRootIdentity: parentIdentity, cancellation: cancellation) else { throw Failure.unsuitable }
        var before = stat(), after = stat()
        guard lstat(snapshot.databaseURL.path, &before) == 0, (before.st_mode & S_IFMT) == S_IFREG,
              before.st_uid == geteuid(), before.st_nlink == 1, (before.st_mode & 0o777) == 0o600 else { throw Failure.unsuitable }
        guard let resolved = realpath(snapshot.databaseURL.deletingLastPathComponent().path, nil) else { throw Failure.unsuitable }
        let databasePath = String(cString: resolved) + "/" + snapshot.databaseURL.lastPathComponent
        free(resolved)
        guard lstat(databasePath, &after) == 0, before.st_dev == after.st_dev, before.st_ino == after.st_ino else { throw Failure.unsuitable }
        var opened: OpaquePointer?
        guard sqlite3_open_v2(databasePath, &opened,
            SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX | SQLITE_OPEN_NOFOLLOW, nil) == SQLITE_OK, let opened else {
            if let opened { sqlite3_close(opened) }; throw Failure.unsuitable
        }
        db = opened
        do {
            guard lstat(snapshot.databaseURL.path, &after) == 0, before.st_dev == after.st_dev,
                  before.st_ino == after.st_ino else { throw Failure.unsuitable }
            sqlite3_busy_timeout(opened, 50)
            sqlite3_progress_handler(opened, 1_000, { context in
                guard let context else { return 1 }
                return Unmanaged<ScanCancellation>.fromOpaque(context).takeUnretainedValue().isCancelled || Task<Never, Never>.isCancelled ? 1 : 0
            }, Unmanaged.passUnretained(cancellation).toOpaque())
            guard sqlite3_exec(opened, "BEGIN", nil, nil, nil) == SQLITE_OK else { throw Failure.unsuitable }
            let metadata = try Self.statement(opened, "SELECT snapshot_id,root_path,event_id,event_identity,skipped FROM metadata WHERE singleton=1")
            defer { sqlite3_finalize(metadata) }
            guard sqlite3_step(metadata) == SQLITE_ROW,
                  Self.text(metadata, 0) == snapshot.id, Self.text(metadata, 1).map({ Data($0.utf8) }) == path,
                  Self.text(metadata, 2) == String(snapshot.eventID), Self.text(metadata, 3) == snapshot.eventIdentity,
                  sqlite3_column_int64(metadata, 4) == 0 else { throw Failure.unsuitable }
            let root = try Self.statement(opened, "SELECT file_identity FROM nodes WHERE parent IS NULL AND is_dir=1 AND accessible=1")
            defer { sqlite3_finalize(root) }
            guard sqlite3_step(root) == SQLITE_ROW, let identity = Self.text(root, 0),
                  try MappingCoverage.canReach(root: parentRoot, target: snapshot.rootURL,
                    expectedRootIdentity: parentIdentity, expectedTargetIdentity: identity, cancellation: cancellation) else { throw Failure.unsuitable }
            let savedCursor = try Self.statement(opened, "SELECT identity,sequence FROM orbit_removal_cursor WHERE singleton=1")
            defer { sqlite3_finalize(savedCursor) }
            guard sqlite3_step(savedCursor) == SQLITE_ROW, let identity = Self.text(savedCursor, 0),
                  UUID(uuidString: identity) != nil, sqlite3_column_int64(savedCursor, 1) >= 0 else { throw Failure.unsuitable }
            cursor = RemovalCursor(identity: identity, sequence: sqlite3_column_int64(savedCursor, 1))
            let bindings = try journal.withFence(cancellation: cancellation) { fence in
                (fence.recoveryRootIdentity(snapshot.rootURL), fence.recoveryRootIdentity(parentRoot))
            }
            guard let source = bindings.0, let parent = bindings.1 else { throw Failure.unsuitable }
            sourceBinding = source; parentBinding = parent
        } catch {
            sqlite3_close_v2(opened); db = nil
            if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
            throw error
        }
    }

    deinit { if let db { sqlite3_close_v2(db) } }

    func contains(_ directory: Data) -> Bool {
        directory == path || (directory.starts(with: path) && directory.dropFirst(path.count).first == 47)
    }

    func listing(_ directory: ListingStage.QueuedDirectory, allowSmall: Bool = false) throws -> StageReusableListing? {
        guard !rejected, contains(directory.path), let db else { return nil }
        do {
            let parent = try Self.statement(db, "SELECT id,file_identity FROM nodes WHERE path=CAST(? AS TEXT) AND is_dir=1 AND accessible=1")
            defer { sqlite3_finalize(parent) }
            directory.path.withUnsafeBytes { _ = sqlite3_bind_blob(parent, 1, $0.baseAddress, Int32($0.count), transient) }
            guard sqlite3_step(parent) == SQLITE_ROW,
                  Self.text(parent, 1) == "\(directory.device):\(directory.inode)" else { return nil }
            let parentID = sqlite3_column_int64(parent, 0)
            if !allowSmall {
                let count = try Self.statement(db, "SELECT COUNT(*) FROM (SELECT 1 FROM nodes WHERE parent=? LIMIT 32)")
                defer { sqlite3_finalize(count) }
                sqlite3_bind_int64(count, 1, parentID)
                guard sqlite3_step(count) == SQLITE_ROW, sqlite3_column_int(count, 0) >= 32 else { return nil }
            }
            let recovered = try journal.recover(root: snapshot.rootURL, cursor: cursor, cancellation: cancellation)
            guard recovered.removals.isEmpty, recovered.affectedParents.isEmpty, !recovered.overlayOverflow,
                  !recovered.hasLiveOperation, !recovered.requiresFullRefresh else { throw Failure.changed }
            cursor = recovered.cursor
            let rows = try Self.statement(db, "SELECT path,name,is_dir,is_link,accessible,size,raw_alloc,file_identity FROM nodes WHERE parent=?")
            sqlite3_bind_int64(rows, 1, parentID)
            return StageReusableListing(source: self, rows: rows, directory: directory.path)
        } catch {
            if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
            rejected = true
            return nil
        }
    }

    func commit<T>(_ action: () throws -> T) throws -> T {
        do {
            return try journal.withFence(cancellation: cancellation) { fence in
                guard fence.identity == cursor.identity, fence.highWater == cursor.sequence,
                      fence.recoveryRootIdentity(snapshot.rootURL) == sourceBinding,
                      fence.recoveryRootIdentity(parentRoot) == parentBinding else { throw Failure.changed }
                return try action()
            }
        } catch Failure.changed { rejected = true; throw Failure.changed }
    }

    static func text(_ statement: OpaquePointer, _ column: Int32) -> String? {
        guard let value = sqlite3_column_text(statement, column) else { return nil }
        return String(bytes: UnsafeBufferPointer(start: value, count: Int(sqlite3_column_bytes(statement, column))), encoding: .utf8)
    }
    static func statement(_ db: OpaquePointer, _ sql: String) throws -> OpaquePointer {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK, let statement else { throw Failure.unsuitable }
        return statement
    }
}

final class StageReusableListing {
    let source: StageReuseSource
    private let rows: OpaquePointer
    private let directory: Data
    private var finished = false
    init(source: StageReuseSource, rows: OpaquePointer, directory: Data) {
        self.source = source; self.rows = rows; self.directory = directory
    }
    deinit { sqlite3_finalize(rows) }
    func page(limit: Int) throws -> [FileMetadata] {
        if finished { return [] }
        var records: [FileMetadata] = []
        while records.count < limit {
            let status = sqlite3_step(rows)
            if status == SQLITE_DONE { finished = true; return records }
            guard status == SQLITE_ROW, let path = StageReuseSource.text(rows, 0), let name = StageReuseSource.text(rows, 1),
                  !name.isEmpty, name != ".", name != "..", !name.contains("/"), !name.utf8.contains(0),
                  let identity = StageReuseSource.text(rows, 7) else { throw StageReuseSource.Failure.unsuitable }
            var expected = directory
            if expected.last != 47 { expected.append(47) }
            expected.append(contentsOf: name.utf8)
            let parts = identity.split(separator: ":")
            guard Data(path.utf8) == expected, parts.count == 2, let device = Int32(parts[0]), let inode = UInt64(parts[1]),
                  sqlite3_column_int(rows, 4) == 1, sqlite3_column_int64(rows, 5) >= 0,
                  sqlite3_column_int64(rows, 6) >= 0 else { throw StageReuseSource.Failure.unsuitable }
            let isDirectory = sqlite3_column_int(rows, 2) != 0
            records.append(FileMetadata(name: name, device: device, inode: inode, isDirectory: isDirectory,
                isSymbolicLink: sqlite3_column_int(rows, 3) != 0, isMountPoint: false, linkCount: 1,
                size: isDirectory ? 0 : sqlite3_column_int64(rows, 5), allocatedSize: isDirectory ? 0 : sqlite3_column_int64(rows, 6)))
        }
        return records
    }
}
