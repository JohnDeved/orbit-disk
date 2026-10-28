import CSQLite
import Darwin
import Foundation

// Short listing snapshots let the writer checkpoint while discovery continues.
final class DiscoveryCacheReader {
    struct Listing {
        let records: [FileMetadata]?
        let revision: Int64
        let generation: Int64
    }
    let generation: Int64
    private let db: OpaquePointer
    private let cancellation: ScanCancellation
    private let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
    #if ORBIT_TESTING
    var testAfterStateRead: (() throws -> Void)?
    var testAfterRecordRead: (() throws -> Void)?
    #endif

    init(path: String, rootIdentity: DirectoryIdentity, cancellation: ScanCancellation) throws {
        var info = stat()
        guard lstat(path, &info) == 0, info.st_uid == geteuid(), info.st_nlink == 1,
              info.st_mode & S_IFMT == S_IFREG else { throw StageReuseSource.Failure.unsuitable }
        guard let parent = realpath(URL(fileURLWithPath: path).deletingLastPathComponent().path, nil) else {
            throw StageReuseSource.Failure.unsuitable
        }
        let resolved = String(cString: parent) + "/" + URL(fileURLWithPath: path).lastPathComponent
        free(parent)
        var actual = stat()
        guard lstat(resolved, &actual) == 0, actual.st_dev == info.st_dev, actual.st_ino == info.st_ino else {
            throw StageReuseSource.Failure.unsuitable
        }
        var opened: OpaquePointer?
        guard sqlite3_open_v2(resolved, &opened, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX | SQLITE_OPEN_NOFOLLOW, nil) == SQLITE_OK,
              let opened else {
            if let opened { sqlite3_close(opened) }
            throw StageReuseSource.Failure.unsuitable
        }
        let savedGeneration: Int64
        do {
            guard lstat(path, &actual) == 0, actual.st_dev == info.st_dev, actual.st_ino == info.st_ino else {
                throw StageReuseSource.Failure.unsuitable
            }
            sqlite3_busy_timeout(opened, 0)
            guard sqlite3_exec(opened, "BEGIN", nil, nil, nil) == SQLITE_OK else { throw StageReuseSource.Failure.unsuitable }
            defer { sqlite3_exec(opened, "ROLLBACK", nil, nil, nil) }
            let full = try StageReuseSource.statement(opened, "SELECT generation FROM full_refresh")
            defer { sqlite3_finalize(full) }
            guard sqlite3_step(full) == SQLITE_ROW else { throw StageReuseSource.Failure.unsuitable }
            savedGeneration = sqlite3_column_int64(full, 0)
            let root = try StageReuseSource.statement(opened, "SELECT dev,inode FROM nodes WHERE parent IS NULL")
            defer { sqlite3_finalize(root) }
            guard sqlite3_step(root) == SQLITE_ROW,
                  InventorySchema.identity(root, device: 0, inode: 1) == "\(rootIdentity.device):\(rootIdentity.inode)" else {
                throw StageReuseSource.Failure.unsuitable
            }
        } catch { sqlite3_close_v2(opened); throw error }
        db = opened; generation = savedGeneration; self.cancellation = cancellation
    }

    deinit { sqlite3_close_v2(db) }

    func listing(_ directory: ListingStage.QueuedDirectory) throws -> Listing? {
        if cancellation.isCancelled { throw CancellationError() }
        guard sqlite3_exec(db, "BEGIN", nil, nil, nil) == SQLITE_OK else { throw StageReuseSource.Failure.unsuitable }
        defer { sqlite3_exec(db, "ROLLBACK", nil, nil, nil) }
        let state = try StageReuseSource.statement(db, """
            SELECT d.pending,COALESCE(dirty.pending,0),COALESCE(dirty.revision,0),f.generation,f.pending,d.full_generation
            FROM directories d CROSS JOIN full_refresh f LEFT JOIN dirty ON dirty.parent=d.path
            WHERE d.path=? AND d.device=? AND d.inode=?
            """)
        defer { sqlite3_finalize(state) }
        directory.path.withUnsafeBytes { _ = sqlite3_bind_blob(state, 1, $0.baseAddress, Int32($0.count), transient) }
        sqlite3_bind_int64(state, 2, Int64(directory.device))
        sqlite3_bind_int64(state, 3, Int64(bitPattern: directory.inode))
        guard sqlite3_step(state) == SQLITE_ROW else { return nil }
        let revision = sqlite3_column_int64(state, 2), generation = sqlite3_column_int64(state, 3)
        #if ORBIT_TESTING
        try testAfterStateRead?()
        #endif
        guard sqlite3_column_int(state, 0) == 0, sqlite3_column_int(state, 1) == 0,
              sqlite3_column_int(state, 4) == 0 || sqlite3_column_int64(state, 5) == generation else {
            return Listing(records: nil, revision: revision, generation: generation)
        }
        let rows = try StageReuseSource.statement(db, """
            SELECT name,is_dir,is_link,accessible,size,raw_alloc,dev,inode,is_mount,links,search_name
            FROM nodes WHERE parent=(SELECT id FROM directory_paths WHERE path=CAST(? AS TEXT))
            """)
        defer { sqlite3_finalize(rows) }
        directory.path.withUnsafeBytes { _ = sqlite3_bind_blob(rows, 1, $0.baseAddress, Int32($0.count), transient) }
        var records: [FileMetadata] = []
        while true {
            if cancellation.isCancelled { throw CancellationError() }
            let status = sqlite3_step(rows)
            if status == SQLITE_DONE { break }
            guard status == SQLITE_ROW else { throw StageReuseSource.Failure.unsuitable }
            records.append(try StageReusableListing.record(rows))
            #if ORBIT_TESTING
            try testAfterRecordRead?()
            #endif
        }
        return Listing(records: records, revision: revision, generation: generation)
    }
}

final class DiscoveryReuse {
    private final class Provider {
        private let lock = NSLock()
        private let lookup: (URL, StageCheckpoint) throws -> StageReuseCandidate?
        private var candidates: [Data: StageReuseCandidate] = [:]

        init(_ lookup: @escaping (URL, StageCheckpoint) throws -> StageReuseCandidate?) { self.lookup = lookup }

        func candidate(for path: Data, root: Data, checkpoint: StageCheckpoint) throws -> StageReuseCandidate? {
            lock.lock(); defer { lock.unlock() }
            var ancestor = path
            while ancestor != root {
                if let candidate = candidates[ancestor] { return candidate }
                let parent = PendingInventoryRead.parent(ancestor)
                if parent == ancestor { break }
                ancestor = parent
            }
            guard path != root, let text = String(data: path, encoding: .utf8),
                  let candidate = try lookup(URL(fileURLWithPath: text, isDirectory: true), checkpoint),
                  Data(candidate.snapshot.rootURL.path.utf8) == path else { return nil }
            candidates[path] = candidate
            return candidate
        }
    }

    private let provider: Provider
    private let cache: URL
    private let root: URL
    private let identity: String
    private let checkpoint: StageCheckpoint
    private let cancellation: ScanCancellation
    private var sources: [Data: StageReuseSource] = [:]

    init(provider: @escaping (URL, StageCheckpoint) throws -> StageReuseCandidate?, cache: URL,
         root: URL, identity: DirectoryIdentity, checkpoint: StageCheckpoint, cancellation: ScanCancellation) {
        self.provider = Provider(provider); self.cache = cache; self.root = root; self.checkpoint = checkpoint
        self.identity = "\(identity.device):\(identity.inode)"; self.cancellation = cancellation
    }

    private init(provider: Provider, cache: URL, root: URL, identity: String,
                 checkpoint: StageCheckpoint, cancellation: ScanCancellation) {
        self.provider = provider; self.cache = cache; self.root = root; self.identity = identity
        self.checkpoint = checkpoint; self.cancellation = cancellation
    }

    func clone(cancellation: ScanCancellation) -> DiscoveryReuse {
        DiscoveryReuse(provider: provider, cache: cache, root: root, identity: identity,
                       checkpoint: checkpoint, cancellation: cancellation)
    }

    func listing(_ directory: ListingStage.QueuedDirectory) throws -> StageReusableListing? {
        let rootPath = Data(root.path.utf8)
        var ancestor = directory.path
        while ancestor != rootPath {
            if let source = sources[ancestor] { return try source.listing(directory, allowSmall: true) }
            let parent = PendingInventoryRead.parent(ancestor)
            if parent == ancestor { break }
            ancestor = parent
        }
        guard let candidate = try provider.candidate(for: directory.path, root: rootPath, checkpoint: checkpoint),
              Data(candidate.checkpoint.rootPath.utf8) == rootPath,
              candidate.checkpoint.identity == checkpoint.identity,
              candidate.checkpoint.eventID <= min(checkpoint.eventID, candidate.snapshot.eventID) else { return nil }
        let source = try StageReuseSource(candidate, cache: cache, parentRoot: root,
                                          parentIdentity: identity, cancellation: cancellation, waitForLocks: false)
        guard let listing = try source.listing(directory, allowSmall: true) else { return nil }
        sources[Data(candidate.snapshot.rootURL.path.utf8)] = source
        return listing
    }
}
