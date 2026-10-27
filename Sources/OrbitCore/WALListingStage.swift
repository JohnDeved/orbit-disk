import CSQLite
import Foundation
import Darwin

// Stores the published node schema alongside the private scan queue.
final class ListingStage {
    private var db: OpaquePointer?
    private var indexRoot: Data?
    private var queueIndexesReady = false
    #if ORBIT_TESTING
    static var testQueueMigrationProgress: (@Sendable () -> Void)?
    static var testAfterFirstQueueIndex: (@Sendable () -> Void)?
    #endif
    let databasePath: String
    let mirror: Bool
    private let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
    enum Failure: Error { case sqlite(String) }

    init(path: String, mirror: Bool = false) throws {
        databasePath = path
        self.mirror = mirror
        guard sqlite3_open_v2(path, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nil) == SQLITE_OK else {
            throw Failure.sqlite("open")
        }
        sqlite3_busy_timeout(db, 5000)
        try exec("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
        try exec("BEGIN IMMEDIATE")
        do {
            try exec("CREATE TABLE IF NOT EXISTS sealed (singleton INTEGER PRIMARY KEY CHECK(singleton=1),snapshot_id TEXT NOT NULL,published INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS stage_issues (path BLOB PRIMARY KEY,message TEXT NOT NULL) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS listing_issues (parent BLOB PRIMARY KEY,count INTEGER NOT NULL,messages TEXT NOT NULL) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS blocked_directories (path BLOB PRIMARY KEY) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS dirty_links (dev INTEGER NOT NULL,inode INTEGER NOT NULL,PRIMARY KEY(dev,inode)) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS link_candidates (dev INTEGER NOT NULL,inode INTEGER NOT NULL,parent BLOB NOT NULL,name BLOB NOT NULL,PRIMARY KEY(dev,inode,parent,name)) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS link_rebuild (dev INTEGER NOT NULL,inode INTEGER NOT NULL,PRIMARY KEY(dev,inode)) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS owners (dev INTEGER NOT NULL,inode INTEGER NOT NULL,parent BLOB NOT NULL,name BLOB NOT NULL,PRIMARY KEY(dev,inode)) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS epochs (parent BLOB PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS dirty (parent BLOB PRIMARY KEY, revision INTEGER NOT NULL, pending INTEGER NOT NULL) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS scan (root BLOB PRIMARY KEY, identity BLOB NOT NULL, event_id INTEGER NOT NULL, expected_snapshot TEXT NOT NULL) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS directories (path BLOB PRIMARY KEY, device INTEGER NOT NULL, inode INTEGER NOT NULL, pending INTEGER NOT NULL, full_generation INTEGER NOT NULL DEFAULT 0, logical INTEGER NOT NULL DEFAULT 0, allocated INTEGER NOT NULL DEFAULT 0, files INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS full_refresh (singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation INTEGER NOT NULL, pending INTEGER NOT NULL); INSERT OR IGNORE INTO full_refresh VALUES(1,0,0); CREATE TABLE IF NOT EXISTS prune (path BLOB PRIMARY KEY, parent BLOB NOT NULL, name BLOB NOT NULL) WITHOUT ROWID; CREATE TABLE IF NOT EXISTS trim (parent BLOB PRIMARY KEY, epoch INTEGER NOT NULL, revision INTEGER NOT NULL, full_generation INTEGER NOT NULL) WITHOUT ROWID;")
            try exec("CREATE TABLE IF NOT EXISTS scan_generation (root BLOB PRIMARY KEY, generation TEXT NOT NULL) WITHOUT ROWID")
            try installMirror()
            try exec("COMMIT")
        } catch {
            try? exec("ROLLBACK")
            throw error
        }
    }

    func close() {
        if let db { sqlite3_close_v2(db); self.db = nil }
    }

    func prepareQueueIndexes(cancellation: ScanCancellation) throws {
        if queueIndexesReady { return }
        guard !cancellation.isCancelled else { throw CancellationError() }
        sqlite3_progress_handler(db, 1000, { context in
            guard let context else { return 1 }
            #if ORBIT_TESTING
            ListingStage.testQueueMigrationProgress?()
            #endif
            return Unmanaged<ScanCancellation>.fromOpaque(context).takeUnretainedValue().isCancelled ? 1 : 0
        }, Unmanaged.passUnretained(cancellation).toOpaque())
        defer { sqlite3_progress_handler(db, 0, nil, nil) }
        do {
            try exec("BEGIN IMMEDIATE")
            try exec("CREATE INDEX IF NOT EXISTS pending_directories ON directories(path) WHERE pending=1")
            #if ORBIT_TESTING
            Self.testAfterFirstQueueIndex?()
            #endif
            try exec("CREATE INDEX IF NOT EXISTS pending_dirty ON dirty(parent) WHERE pending=1")
            guard !cancellation.isCancelled else { throw CancellationError() }
            try exec("COMMIT")
            queueIndexesReady = true
        } catch {
            sqlite3_progress_handler(db, 0, nil, nil)
            try? exec("ROLLBACK")
            if cancellation.isCancelled { throw CancellationError() }
            throw error
        }
    }

    // Replay parent paths are disk-backed so a large deletion does not grow the worker heap.
    func resetReplayParents() throws {
        try exec("CREATE TABLE IF NOT EXISTS replay_parents(path BLOB PRIMARY KEY) WITHOUT ROWID; DELETE FROM replay_parents")
    }
    func addReplayParents(_ paths: [Data]) throws {
        try exec("BEGIN IMMEDIATE")
        do {
            let statement = try prepare("INSERT OR IGNORE INTO replay_parents VALUES(?)")
            defer { sqlite3_finalize(statement) }
            for path in paths { sqlite3_reset(statement); sqlite3_clear_bindings(statement); bind(path, 1, statement); try step(statement) }
            try exec("COMMIT")
        } catch { try? exec("ROLLBACK"); throw error }
    }
    func replayParents(after: Data?, limit: Int) throws -> [Data] {
        let statement = try prepare(after == nil ? "SELECT path FROM replay_parents ORDER BY path LIMIT ?" : "SELECT path FROM replay_parents WHERE path>? ORDER BY path LIMIT ?")
        defer { sqlite3_finalize(statement) }
        if let after { bind(after, 1, statement); sqlite3_bind_int(statement, 2, Int32(limit)) }
        else { sqlite3_bind_int(statement, 1, Int32(limit)) }
        var result: [Data] = []
        while true {
            let status = sqlite3_step(statement)
            if status == SQLITE_DONE { break }
            guard status == SQLITE_ROW, let bytes = sqlite3_column_blob(statement, 0) else { throw Failure.sqlite("replay parent page") }
            result.append(Data(bytes: bytes, count: Int(sqlite3_column_bytes(statement, 0))))
        }
        return result
    }
    func isFreshForRemovalAnchor() throws -> Bool {
        let statement = try prepare("SELECT 1 FROM nodes WHERE parent IS NOT NULL UNION ALL SELECT 1 FROM epochs UNION ALL SELECT 1 FROM dirty UNION ALL SELECT 1 FROM prune UNION ALL SELECT 1 FROM trim UNION ALL SELECT 1 FROM sealed LIMIT 1")
        defer { sqlite3_finalize(statement) }
        let result = sqlite3_step(statement)
        guard result == SQLITE_ROW || result == SQLITE_DONE else { throw Failure.sqlite("fresh stage check") }
        return result == SQLITE_DONE
    }
    func hasPendingDirectories() throws -> Bool {
        let statement = try prepare("SELECT 1 FROM directories WHERE pending=1 LIMIT 1")
        defer { sqlite3_finalize(statement) }
        let result = sqlite3_step(statement)
        guard result == SQLITE_ROW || result == SQLITE_DONE else { throw Failure.sqlite("pending directories") }
        return result == SQLITE_ROW
    }
    func hasReplayDirtyWork() throws -> Bool {
        let statement = try prepare("SELECT 1 FROM dirty WHERE pending=1 UNION ALL SELECT 1 FROM dirty_links UNION ALL SELECT 1 FROM prune UNION ALL SELECT 1 FROM trim UNION ALL SELECT 1 FROM full_refresh WHERE pending=1 LIMIT 1")
        defer { sqlite3_finalize(statement) }
        let result = sqlite3_step(statement)
        guard result == SQLITE_ROW || result == SQLITE_DONE else { throw Failure.sqlite("replay work") }
        return result == SQLITE_ROW
    }

    func removalCursor() throws -> RemovalCursor? {
        let exists = try prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='orbit_removal_cursor'")
        defer { sqlite3_finalize(exists) }
        guard sqlite3_step(exists) == SQLITE_ROW else { throw Failure.sqlite("removal checkpoint") }
        if sqlite3_column_int(exists, 0) == 0 { return nil }
        let row = try prepare("SELECT identity,sequence FROM orbit_removal_cursor WHERE singleton=1")
        defer { sqlite3_finalize(row) }
        guard sqlite3_step(row) == SQLITE_ROW, let raw = sqlite3_column_text(row, 0) else { throw Failure.sqlite("removal checkpoint") }
        let identity = String(cString: raw), sequence = sqlite3_column_int64(row, 1)
        guard UUID(uuidString: identity) != nil, sequence >= 0 else { throw Failure.sqlite("invalid removal checkpoint") }
        return RemovalCursor(identity: identity, sequence: sequence)
    }

    func writeRemovalCursor(_ cursor: RemovalCursor) throws {
        try exec("CREATE TABLE IF NOT EXISTS orbit_removal_cursor(singleton INTEGER PRIMARY KEY CHECK(singleton=1),identity TEXT NOT NULL,sequence INTEGER NOT NULL)")
        let row = try prepare("INSERT OR REPLACE INTO orbit_removal_cursor VALUES(1,?,?)")
        defer { sqlite3_finalize(row) }
        bindText(cursor.identity, 1, row); sqlite3_bind_int64(row, 2, cursor.sequence); try step(row)
    }
    deinit { close() }

    private func installMirror() throws {
        try exec("""
            PRAGMA application_id=1330790996; PRAGMA user_version=3;
            CREATE TABLE IF NOT EXISTS metadata (singleton INTEGER PRIMARY KEY CHECK(singleton=1), snapshot_id TEXT NOT NULL,
              root_path TEXT NOT NULL, scanned_at REAL NOT NULL, elapsed REAL NOT NULL, skipped INTEGER NOT NULL,
              issues TEXT NOT NULL, event_id TEXT NOT NULL, event_identity TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS nodes (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, parent INTEGER, name TEXT NOT NULL DEFAULT '', search_name TEXT NOT NULL DEFAULT '',
              is_dir INTEGER NOT NULL DEFAULT 0, is_link INTEGER NOT NULL DEFAULT 0, accessible INTEGER NOT NULL DEFAULT 1,
              size INTEGER NOT NULL DEFAULT 0, allocated INTEGER NOT NULL DEFAULT 0, file_count INTEGER NOT NULL DEFAULT 0,
              raw_alloc INTEGER NOT NULL DEFAULT 0, file_identity TEXT,
              st_parent BLOB, st_name BLOB, dev INTEGER, inode INTEGER, directory INTEGER, st_is_link INTEGER,
              is_mount INTEGER, st_size INTEGER, st_allocated INTEGER, links INTEGER, epoch INTEGER,
              st_path BLOB, charged INTEGER, folded BLOB, identity BLOB, UNIQUE(st_parent,st_name));
            CREATE INDEX IF NOT EXISTS children_by_size ON nodes(parent,size DESC,name);
            CREATE INDEX IF NOT EXISTS nodes_by_size ON nodes(size DESC,id,search_name,parent);
            CREATE INDEX IF NOT EXISTS file_identities ON nodes(file_identity) WHERE is_dir=0 AND file_identity IS NOT NULL;
            CREATE TRIGGER IF NOT EXISTS node_directory_totals AFTER UPDATE OF logical,allocated,files ON directories BEGIN
              UPDATE nodes SET size=NEW.logical,allocated=NEW.allocated,file_count=NEW.files WHERE path=CAST(NEW.path AS TEXT);
            END;
            """)
    }

    func initializeMirrorRoot(url: URL, identity: DirectoryIdentity) throws {

        let statement = try prepare("INSERT OR IGNORE INTO nodes(path,parent,name,search_name,is_dir,is_link,accessible,size,allocated,file_count,raw_alloc,file_identity) VALUES(?,NULL,?,?,1,0,1,0,0,0,0,?)")
        defer { sqlite3_finalize(statement) }
        let name = url.lastPathComponent.isEmpty ? url.path : url.lastPathComponent
        bindText(url.path, 1, statement); bindText(name, 2, statement)
        bindText(name.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: Locale(identifier: "en_US_POSIX")), 3, statement)
        bindText("\(identity.device):\(identity.inode)", 4, statement)
        try step(statement)
    }

    // Private partial snapshot for a reader; final publication still requires sealMirror.
    func previewMirror(rootURL: URL, checkpoint: StageCheckpoint) throws -> IndexSnapshot {
        guard Data(rootURL.path.utf8) == indexRoot else { throw Failure.sqlite("invalid mirror root") }
        let existing = try prepare("SELECT snapshot_id,scanned_at FROM metadata WHERE singleton=1")
        let result = sqlite3_step(existing)
        if result == SQLITE_ROW, let idBytes = sqlite3_column_text(existing, 0) {
            let id = String(cString: idBytes), time = sqlite3_column_double(existing, 1)
            sqlite3_finalize(existing)
            return IndexSnapshot(id: id, rootURL: rootURL, databaseURL: URL(fileURLWithPath: databasePath), scannedAt: Date(timeIntervalSince1970: time), elapsed: 0, skippedCount: 0, issues: [], eventID: checkpoint.eventID, eventIdentity: checkpoint.identity)
        }
        sqlite3_finalize(existing)
        guard result == SQLITE_DONE else { throw Failure.sqlite("preview metadata") }
        let id = UUID().uuidString, now = Date()
        let statement = try prepare("INSERT INTO metadata VALUES(1,?,?,?,?,?,?,?,?)")
        defer { sqlite3_finalize(statement) }
        bindText(id, 1, statement); bindText(rootURL.path, 2, statement)
        sqlite3_bind_double(statement, 3, now.timeIntervalSince1970); sqlite3_bind_double(statement, 4, 0)
        sqlite3_bind_int(statement, 5, 0); bindText("[]", 6, statement)
        bindText(String(checkpoint.eventID), 7, statement); bindText(checkpoint.identity, 8, statement)
        try step(statement)
        return IndexSnapshot(id: id, rootURL: rootURL, databaseURL: URL(fileURLWithPath: databasePath), scannedAt: now, elapsed: 0, skippedCount: 0, issues: [], eventID: checkpoint.eventID, eventIdentity: checkpoint.identity)
    }

    func sealMirror(rootURL: URL, checkpoint: StageCheckpoint, elapsed: TimeInterval) throws -> IndexSnapshot {
        guard Data(rootURL.path.utf8) == indexRoot else { throw Failure.sqlite("invalid mirror root") }
        try exec("BEGIN IMMEDIATE")
        do {
            let remaining = try prepare("SELECT 1 FROM directories WHERE pending=1 UNION ALL SELECT 1 FROM dirty_links UNION ALL SELECT 1 FROM prune UNION ALL SELECT 1 FROM trim UNION ALL SELECT 1 FROM full_refresh WHERE pending=1 LIMIT 1")
            defer { sqlite3_finalize(remaining) }
            guard sqlite3_step(remaining) == SQLITE_DONE else { throw Failure.sqlite("stage has pending work") }
            guard let original = try scanStart(root: Data(rootURL.path.utf8)),
                  original.identity == checkpoint.identity, original.eventID == checkpoint.eventID else {
                throw Failure.sqlite("original checkpoint changed")
            }
            let issuesQuery = try prepare("SELECT message FROM stage_issues ORDER BY path LIMIT 100")
            defer { sqlite3_finalize(issuesQuery) }
            var issues: [String] = []
            while true {
                let result = sqlite3_step(issuesQuery)
                if result == SQLITE_DONE { break }
                guard result == SQLITE_ROW, let message = sqlite3_column_text(issuesQuery, 0) else { throw Failure.sqlite("invalid issue") }
                issues.append(String(cString: message))
            }
            if issues.count < 100 {
                let listing = try prepare("SELECT messages FROM listing_issues ORDER BY parent")
                defer { sqlite3_finalize(listing) }
                while issues.count < 100 {
                    let result = sqlite3_step(listing)
                    if result == SQLITE_DONE { break }
                    guard result == SQLITE_ROW, let value = sqlite3_column_text(listing, 0) else { throw Failure.sqlite("invalid listing issue") }
                    let encoded = Data(String(cString: value).utf8)
                    let entries = try JSONDecoder().decode([String].self, from: encoded)
                    issues.append(contentsOf: entries.prefix(100 - issues.count))
                }
            }
            let issueCount = try prepare("SELECT (SELECT COUNT(*) FROM stage_issues)+(SELECT COALESCE(SUM(count),0) FROM listing_issues)")
            defer { sqlite3_finalize(issueCount) }
            guard sqlite3_step(issueCount) == SQLITE_ROW else { throw Failure.sqlite("issue count") }
            let skipped = sqlite3_column_int64(issueCount, 0)
            let issuesJSON = String(decoding: try JSONEncoder().encode(issues), as: UTF8.self)
            let existingID = try prepare("SELECT snapshot_id FROM metadata WHERE singleton=1")
            defer { sqlite3_finalize(existingID) }
            let idResult = sqlite3_step(existingID)
            let id: String
            if idResult == SQLITE_ROW, let value = sqlite3_column_text(existingID, 0) { id = String(cString: value) }
            else if idResult == SQLITE_DONE { id = UUID().uuidString }
            else { throw Failure.sqlite("invalid preview snapshot id") }
            let now = Date()
            let statement = try prepare("INSERT OR REPLACE INTO metadata VALUES(1,?,?,?,?,?,?,?,?)")
            defer { sqlite3_finalize(statement) }
            bindText(id, 1, statement); bindText(rootURL.path, 2, statement)
            sqlite3_bind_double(statement, 3, now.timeIntervalSince1970); sqlite3_bind_double(statement, 4, elapsed)
            sqlite3_bind_int64(statement, 5, skipped); bindText(issuesJSON, 6, statement)
            bindText(String(checkpoint.eventID), 7, statement); bindText(checkpoint.identity, 8, statement)
            try step(statement)
            let sealed = try prepare("INSERT OR REPLACE INTO sealed(singleton,snapshot_id,published) VALUES(1,?,0)")
            defer { sqlite3_finalize(sealed) }
            bindText(id, 1, sealed); try step(sealed)
            try exec("COMMIT")
            return IndexSnapshot(id: id, rootURL: rootURL, databaseURL: URL(fileURLWithPath: databasePath),
                scannedAt: now, elapsed: elapsed, skippedCount: Int(skipped), issues: issues, eventID: checkpoint.eventID,
                eventIdentity: checkpoint.identity)
        } catch { try? exec("ROLLBACK"); throw error }
    }

    // The caller must exclude new writes until clonefile has completed.
    func checkpointForClone(timeoutMS: Int32 = 5_000, cancellation: ScanCancellation = ScanCancellation()) throws {
        let deadline = ProcessInfo.processInfo.systemUptime + Double(timeoutMS) / 1_000
        sqlite3_busy_timeout(db, min(100, timeoutMS))
        defer { sqlite3_busy_timeout(db, 5_000) }
        while true {
            if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
            let status = sqlite3_wal_checkpoint_v2(db, nil, SQLITE_CHECKPOINT_TRUNCATE, nil, nil)
            if status == SQLITE_OK { break }
            guard status == SQLITE_BUSY && ProcessInfo.processInfo.systemUptime < deadline else {
                throw Failure.sqlite("checkpoint failed: \(sqlite3_extended_errcode(db))")
            }
        }
        if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
        let change = try prepare("PRAGMA journal_mode=DELETE")
        defer { sqlite3_finalize(change) }
        guard sqlite3_step(change) == SQLITE_ROW, let mode = sqlite3_column_text(change, 0),
              String(cString: mode).lowercased() == "delete" else { throw Failure.sqlite("could not close WAL") }
    }

    func resumeWAL() throws {
        let change = try prepare("PRAGMA journal_mode=WAL")
        defer { sqlite3_finalize(change) }
        guard sqlite3_step(change) == SQLITE_ROW, let mode = sqlite3_column_text(change, 0),
              String(cString: mode).lowercased() == "wal" else { throw Failure.sqlite("could not restore WAL") }
    }

    private func bindText(_ text: String, _ index: Int32, _ statement: OpaquePointer) {
        text.withCString { _ = sqlite3_bind_text(statement, index, $0, -1, transient) }
    }

    private func exec(_ sql: String) throws {
        guard sqlite3_exec(db, sql, nil, nil, nil) == SQLITE_OK else {
            throw Failure.sqlite("\(sqlite3_extended_errcode(db)): \(String(cString: sqlite3_errmsg(db)))")
        }
    }
    private func prepare(_ sql: String) throws -> OpaquePointer {
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK, let stmt else {
            throw Failure.sqlite("\(sqlite3_extended_errcode(db)): \(String(cString: sqlite3_errmsg(db)))")
        }
        return stmt
    }
    private func bind(_ data: Data, _ index: Int32, _ statement: OpaquePointer) {
        data.withUnsafeBytes { bytes in
            _ = sqlite3_bind_blob(statement, index, bytes.baseAddress, Int32(bytes.count), transient)
        }
    }
    private func step(_ statement: OpaquePointer) throws {
        guard sqlite3_step(statement) == SQLITE_DONE else { throw Failure.sqlite("\(sqlite3_extended_errcode(db)): \(String(cString: sqlite3_errmsg(db)))") }
    }

    func scanStart(root: Data) throws -> (identity: String, eventID: UInt64)? {
        let statement = try prepare("SELECT identity,event_id FROM scan WHERE root=?")
        defer { sqlite3_finalize(statement) }
        bind(root, 1, statement)
        let result = sqlite3_step(statement)
        if result == SQLITE_DONE { return nil }
        guard result == SQLITE_ROW, let bytes = sqlite3_column_blob(statement, 0) else { throw Failure.sqlite("invalid scan checkpoint") }
        let data = Data(bytes: bytes, count: Int(sqlite3_column_bytes(statement, 0)))
        guard let identity = String(data: data, encoding: .utf8) else { throw Failure.sqlite("invalid scan identity") }
        return (identity, UInt64(bitPattern: sqlite3_column_int64(statement, 1)))
    }

    func preserveScanStart(root: Data, identity: String, eventID: UInt64, expected: SnapshotExpectation? = nil, generation: String? = nil) throws {
        try exec("BEGIN IMMEDIATE")
        do {
        let other = try prepare("SELECT 1 FROM scan WHERE root<>? LIMIT 1")
        defer { sqlite3_finalize(other) }
        bind(root, 1, other)
        guard sqlite3_step(other) == SQLITE_DONE else { throw Failure.sqlite("stage belongs to another root") }
        let statement = try prepare("INSERT OR IGNORE INTO scan(root,identity,event_id,expected_snapshot) VALUES(?,?,?,?)")
        defer { sqlite3_finalize(statement) }
        bind(root, 1, statement); bind(Data(identity.utf8), 2, statement)
        sqlite3_bind_int64(statement, 3, Int64(bitPattern: eventID))
        bindText(expected?.encoded ?? "unspecified", 4, statement)
        try step(statement)
        let check = try prepare("SELECT identity,event_id,expected_snapshot FROM scan WHERE root=?")
        defer { sqlite3_finalize(check) }
        bind(root, 1, check)
        guard sqlite3_step(check) == SQLITE_ROW else { throw Failure.sqlite("missing scan checkpoint") }
        let size = Int(sqlite3_column_bytes(check, 0))
        guard let bytes = sqlite3_column_blob(check, 0),
              Data(bytes: bytes, count: size) == Data(identity.utf8),
              UInt64(bitPattern: sqlite3_column_int64(check, 1)) == eventID,
              expected == nil || sqlite3_column_text(check, 2).map({ String(cString: $0) }) == expected?.encoded else {
            throw Failure.sqlite("scan-start checkpoint mismatch")
        }
        if let generation { try preserveGeneration(root: root, generation: generation) }
        try exec("COMMIT")
        indexRoot = root
        } catch { try? exec("ROLLBACK"); throw error }
    }

    func completedSnapshot(root: Data) throws -> (id: String, published: Bool)? {
        let statement = try prepare("""
            SELECT metadata.snapshot_id,sealed.published FROM metadata JOIN sealed ON sealed.singleton=1 AND sealed.snapshot_id=metadata.snapshot_id WHERE root_path=CAST(? AS TEXT)
              AND NOT EXISTS(SELECT 1 FROM directories WHERE pending=1)
              AND NOT EXISTS(SELECT 1 FROM dirty_links)
              AND NOT EXISTS(SELECT 1 FROM prune)
              AND NOT EXISTS(SELECT 1 FROM trim)
              AND NOT EXISTS(SELECT 1 FROM full_refresh WHERE pending=1)
            """)
        defer { sqlite3_finalize(statement) }
        bind(root, 1, statement)
        let result = sqlite3_step(statement)
        if result == SQLITE_DONE { return nil }
        guard result == SQLITE_ROW, let id = sqlite3_column_text(statement, 0) else { throw Failure.sqlite("invalid completed snapshot") }
        return (String(cString: id), sqlite3_column_int(statement, 1) != 0)
    }

    func markPublished(id: String) throws {
        try exec("BEGIN IMMEDIATE")
        do {
            let mark = try prepare("UPDATE sealed SET published=1 WHERE singleton=1 AND snapshot_id=?")
            defer { sqlite3_finalize(mark) }
            bindText(id, 1, mark); try step(mark)
            guard sqlite3_changes(db) == 1 else { throw Failure.sqlite("sealed snapshot changed") }
            try exec("COMMIT")
        } catch { try? exec("ROLLBACK"); throw error }
    }

    func scanExpectation(root: Data) throws -> SnapshotExpectation? {
        let statement = try prepare("SELECT expected_snapshot FROM scan WHERE root=?")
        defer { sqlite3_finalize(statement) }
        bind(root, 1, statement)
        let result = sqlite3_step(statement)
        if result == SQLITE_DONE { return nil }
        guard result == SQLITE_ROW, let value = sqlite3_column_text(statement, 0) else { throw Failure.sqlite("invalid scan expectation") }
        let encoded = String(cString: value)
        if encoded == "unspecified" { return nil }
        guard let expected = SnapshotExpectation(encoded: encoded) else { throw Failure.sqlite("invalid scan expectation") }
        return expected
    }

    func scanGeneration(root: Data) throws -> String? {
        let query = try prepare("SELECT generation FROM scan_generation WHERE root=?")
        defer { sqlite3_finalize(query) }
        bind(root, 1, query)
        let status = sqlite3_step(query)
        if status == SQLITE_DONE { return nil }
        guard status == SQLITE_ROW, let value = sqlite3_column_text(query, 0) else { throw Failure.sqlite("invalid scan generation") }
        return String(cString: value)
    }

    func preserveGeneration(root: Data, generation: String) throws {
        let insert = try prepare("INSERT OR IGNORE INTO scan_generation(root,generation) VALUES(?,?)")
        defer { sqlite3_finalize(insert) }
        bind(root, 1, insert); bindText(generation, 2, insert); try step(insert)
        guard try scanGeneration(root: root) == generation else { throw SnapshotExpectation.Failure.changed }
    }

    func rebaseExpectation(root: Data, from: SnapshotExpectation, to: SnapshotExpectation) throws {
        let update = try prepare("UPDATE scan SET expected_snapshot=? WHERE root=? AND expected_snapshot=?")
        defer { sqlite3_finalize(update) }
        bindText(to.encoded, 1, update); bind(root, 2, update); bindText(from.encoded, 3, update); try step(update)
        guard sqlite3_changes(db) == 1 else { throw SnapshotExpectation.Failure.changed }
    }

    func begin(parent: Data) throws -> Int64 {
        try exec("BEGIN IMMEDIATE")
        do {
            let update = try prepare("INSERT INTO epochs(parent,value) VALUES(?,1) ON CONFLICT(parent) DO UPDATE SET value=value+1")
            defer { sqlite3_finalize(update) }
            bind(parent, 1, update)
            try step(update)
            let select = try prepare("SELECT value FROM epochs WHERE parent=?")
            defer { sqlite3_finalize(select) }
            bind(parent, 1, select)
            guard sqlite3_step(select) == SQLITE_ROW else { throw Failure.sqlite("missing epoch") }
            let epoch = sqlite3_column_int64(select, 0)
            try exec("COMMIT")
            return epoch
        } catch { try? exec("ROLLBACK"); throw error }
    }

    enum Reconcile: Error { case pruneRequired }

    func append(parent: Data, epoch: Int64, records: [FileMetadata], rootDevice: Int32? = nil) throws {
        if let rootDevice {
            var prefix = parent
            if prefix.last != 47 { prefix.append(47) }
            var upper = prefix
            upper[upper.index(before: upper.endIndex)] = 48
            let exists = try prepare("SELECT 1 FROM directories WHERE path>=? AND path<? LIMIT 1")
            bind(prefix, 1, exists); bind(upper, 2, exists)
            let state = sqlite3_step(exists)
            sqlite3_finalize(exists)
            guard state == SQLITE_ROW || state == SQLITE_DONE else { throw Failure.sqlite("directory preflight range") }
            let query = try prepare("SELECT device,inode FROM directories WHERE path=?")
            defer { sqlite3_finalize(query) }
            for record in state == SQLITE_ROW ? records : [] {
                var child = parent
                if child.last != 47 { child.append(47) }
                child.append(contentsOf: record.name.utf8)
                sqlite3_reset(query); sqlite3_clear_bindings(query); bind(child, 1, query)
                let status = sqlite3_step(query)
                if status == SQLITE_ROW {
                    let device = Int32(sqlite3_column_int64(query, 0)), inode = UInt64(bitPattern: sqlite3_column_int64(query, 1))
                    if !record.isDirectory || record.isMountPoint || record.device != rootDevice || device != record.device || inode != record.inode {
                        try startPrune(path: child, parent: parent, name: Data(record.name.utf8))
                        throw Reconcile.pruneRequired
                    }
                } else if status != SQLITE_DONE { throw Failure.sqlite("directory preflight") }
            }
        }
        try exec("BEGIN IMMEDIATE")
        do {
            let parentCheck = try prepare("SELECT id FROM nodes WHERE path=CAST(? AS TEXT) AND is_dir=1")
            defer { sqlite3_finalize(parentCheck) }
            bind(parent, 1, parentCheck)
            guard sqlite3_step(parentCheck) == SQLITE_ROW else { throw Failure.sqlite("missing node parent") }
            let parentID = sqlite3_column_int64(parentCheck, 0)
            let insert = try prepare("""
                INSERT INTO nodes(st_parent,st_name,dev,inode,directory,st_is_link,is_mount,st_size,st_allocated,links,epoch,st_path,charged,folded,identity,
                    path,parent,name,search_name,is_dir,is_link,accessible,size,allocated,file_count,raw_alloc,file_identity)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,
                    CAST(?12 AS TEXT),?16,CAST(?2 AS TEXT),CAST(?14 AS TEXT),?5,?6,
                    CASE WHEN ?5=1 AND (?7=1 OR ?3<>(SELECT device FROM directories WHERE path=(SELECT root FROM scan LIMIT 1))) THEN 0 ELSE 1 END,
                    CASE WHEN ?5=1 THEN COALESCE((SELECT logical FROM directories WHERE path=?12),0) ELSE ?8 END,
                    CASE WHEN ?5=1 THEN COALESCE((SELECT allocated FROM directories WHERE path=?12),0) ELSE ?13 END,
                    CASE WHEN ?5=1 THEN COALESCE((SELECT files FROM directories WHERE path=?12),0) ELSE 1 END,
                    CASE WHEN ?5=1 THEN 0 ELSE ?9 END,CAST(?15 AS TEXT))
                ON CONFLICT(st_parent,st_name) DO UPDATE SET
                    charged=CASE WHEN nodes.dev=excluded.dev AND nodes.inode=excluded.inode AND nodes.directory=excluded.directory THEN nodes.charged ELSE 0 END,
                    dev=excluded.dev,inode=excluded.inode,directory=excluded.directory,st_is_link=excluded.st_is_link,is_mount=excluded.is_mount,
                    st_size=excluded.st_size,st_allocated=excluded.st_allocated,links=excluded.links,epoch=excluded.epoch,st_path=excluded.st_path,folded=excluded.folded,identity=excluded.identity,
                    path=excluded.path,parent=excluded.parent,name=excluded.name,search_name=excluded.search_name,is_dir=excluded.is_dir,is_link=excluded.is_link,
                    accessible=excluded.accessible,size=excluded.size,
                    allocated=CASE WHEN excluded.directory=1 THEN excluded.allocated WHEN nodes.dev=excluded.dev AND nodes.inode=excluded.inode AND nodes.directory=excluded.directory THEN nodes.charged ELSE 0 END,
                    file_count=excluded.file_count,raw_alloc=excluded.raw_alloc,file_identity=excluded.file_identity
                """)
            let existing = try prepare("SELECT dev,inode,directory,st_size,charged,st_allocated FROM nodes WHERE st_parent=? AND st_name=?")
            let dirtyLink = try prepare("INSERT OR IGNORE INTO dirty_links VALUES(?,?)")
            let rebuildLink = try prepare("INSERT OR IGNORE INTO link_rebuild VALUES(?,?)")
            let candidate = try prepare("INSERT OR IGNORE INTO link_candidates VALUES(?,?,?,?)")
            let singleOwner = try prepare("INSERT OR IGNORE INTO owners VALUES(?,?,?,?)")
            let clearIssue = try prepare("DELETE FROM stage_issues WHERE path=?")
            let mountIssue = try prepare("INSERT OR REPLACE INTO stage_issues(path,message) VALUES(?,?)")
            defer { for statement in [insert,existing,dirtyLink,rebuildLink,candidate,singleOwner,clearIssue,mountIssue] { sqlite3_finalize(statement) } }
            var registerInsert: OpaquePointer?, registerCheck: OpaquePointer?
            defer { sqlite3_finalize(registerInsert); sqlite3_finalize(registerCheck) }
            let hasIssuesQuery = try prepare("SELECT 1 FROM stage_issues LIMIT 1")
            let issueState = sqlite3_step(hasIssuesQuery)
            sqlite3_finalize(hasIssuesQuery)
            guard issueState == SQLITE_ROW || issueState == SQLITE_DONE else { throw Failure.sqlite("issue preflight") }
            var mayHaveIssues = issueState == SQLITE_ROW
            var nameForCandidate = Data()
            func markLink(_ device: Int64, _ inode: Int64, rebuild: Bool) throws {
                sqlite3_reset(dirtyLink); sqlite3_clear_bindings(dirtyLink)
                sqlite3_bind_int64(dirtyLink, 1, device); sqlite3_bind_int64(dirtyLink, 2, inode)
                try step(dirtyLink)
                if rebuild {
                    sqlite3_reset(rebuildLink); sqlite3_clear_bindings(rebuildLink)
                    sqlite3_bind_int64(rebuildLink, 1, device); sqlite3_bind_int64(rebuildLink, 2, inode); try step(rebuildLink)
                } else {
                    sqlite3_reset(candidate); sqlite3_clear_bindings(candidate)
                    sqlite3_bind_int64(candidate, 1, device); sqlite3_bind_int64(candidate, 2, inode)
                    bind(parent, 3, candidate); bind(nameForCandidate, 4, candidate); try step(candidate)
                }
            }
            var logicalDelta: Int64 = 0, allocatedDelta: Int64 = 0, fileDelta: Int64 = 0
            for record in records {
                let name = Data(record.name.utf8)
                nameForCandidate = name
                sqlite3_reset(existing); sqlite3_clear_bindings(existing)
                bind(parent, 1, existing); bind(name, 2, existing)
                let old = sqlite3_step(existing)
                var oldSize: Int64 = 0, oldCharge: Int64 = 0, oldCount: Int64 = 0
                var unchangedIdentity = false
                if old == SQLITE_ROW && sqlite3_column_int(existing, 2) == 0 {
                    let oldDevice = sqlite3_column_int64(existing, 0), oldInode = sqlite3_column_int64(existing, 1)
                    oldSize = sqlite3_column_int64(existing, 3); oldCharge = sqlite3_column_int64(existing, 4); oldCount = 1
                    unchangedIdentity = !record.isDirectory && oldDevice == Int64(record.device) && oldInode == Int64(bitPattern: record.inode)
                    if !unchangedIdentity || sqlite3_column_int64(existing, 5) != record.allocatedSize {
                        try markLink(oldDevice, oldInode, rebuild: true)
                    }
                } else if old != SQLITE_ROW && old != SQLITE_DONE { throw Failure.sqlite("existing link query") }
                var firstCharge: Int64 = 0
                if !record.isDirectory {
                    if old == SQLITE_DONE && record.linkCount == 1 {
                        sqlite3_reset(singleOwner); sqlite3_clear_bindings(singleOwner)
                        sqlite3_bind_int64(singleOwner, 1, Int64(record.device))
                        sqlite3_bind_int64(singleOwner, 2, Int64(bitPattern: record.inode))
                        bind(parent, 3, singleOwner); bind(name, 4, singleOwner)
                        try step(singleOwner)
                        if sqlite3_changes(db) == 1 { firstCharge = record.allocatedSize }
                        else { try markLink(Int64(record.device), Int64(bitPattern: record.inode), rebuild: true) }
                    } else if !unchangedIdentity { try markLink(Int64(record.device), Int64(bitPattern: record.inode), rebuild: false) }
                }
                logicalDelta += (record.isDirectory ? 0 : record.size) - oldSize
                fileDelta += (record.isDirectory ? 0 : 1) - oldCount
                allocatedDelta += (unchangedIdentity ? oldCharge : firstCharge) - oldCharge
                var fullPath = parent
                if fullPath.last != 47 { fullPath.append(47) }
                fullPath.append(name)
                sqlite3_reset(insert); sqlite3_clear_bindings(insert)
                bind(parent, 1, insert); bind(name, 2, insert); sqlite3_bind_int64(insert, 16, parentID)
                sqlite3_bind_int64(insert, 3, Int64(record.device)); sqlite3_bind_int64(insert, 4, Int64(bitPattern: record.inode))
                sqlite3_bind_int(insert, 5, record.isDirectory ? 1 : 0)
                sqlite3_bind_int(insert, 6, record.isSymbolicLink ? 1 : 0)
                sqlite3_bind_int(insert, 7, record.isMountPoint ? 1 : 0)
                sqlite3_bind_int64(insert, 8, record.size); sqlite3_bind_int64(insert, 9, record.allocatedSize)
                sqlite3_bind_int64(insert, 10, Int64(record.linkCount)); sqlite3_bind_int64(insert, 11, epoch)
                if mayHaveIssues {
                    sqlite3_reset(clearIssue); sqlite3_clear_bindings(clearIssue); bind(fullPath, 1, clearIssue); try step(clearIssue)
                }
                if let rootDevice, record.isDirectory && (record.isMountPoint || record.device != rootDevice) {
                    mayHaveIssues = true
                    sqlite3_reset(mountIssue); sqlite3_clear_bindings(mountIssue); bind(fullPath, 1, mountIssue)
                    bindText("\(String(decoding: fullPath, as: UTF8.self)): nested volume skipped", 2, mountIssue); try step(mountIssue)
                }
                bind(fullPath, 12, insert); sqlite3_bind_int64(insert, 13, firstCharge)
                let folded = record.name.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: Locale(identifier: "en_US_POSIX"))
                bind(Data(folded.utf8), 14, insert)
                bind(Data("\(record.device):\(record.inode)".utf8), 15, insert)
                try step(insert)
                if let rootDevice, record.isDirectory, !record.isMountPoint, record.device == rootDevice {
                    if registerInsert == nil { registerInsert = try prepare("INSERT OR IGNORE INTO directories(path,device,inode,pending) VALUES(?,?,?,1)") }
                    if registerCheck == nil { registerCheck = try prepare("SELECT device,inode FROM directories WHERE path=?") }
                    try registerDirectory(path: fullPath, device: record.device, inode: record.inode,
                                          insert: registerInsert!, check: registerCheck!)
                }
            }
            if rootDevice != nil { try adjustAncestors(parent: parent, logical: logicalDelta, allocated: allocatedDelta, files: fileDelta) }
            try exec("COMMIT")
        } catch { try? exec("ROLLBACK"); throw error }
    }

    // A completed enumeration schedules bounded trimming; acknowledgements happen after trimming.
    func finish(parent: Data, epoch: Int64, expectedRevision: Int64, expectedFullGeneration: Int64? = nil, issueCount: Int = 0, issues: [String] = []) throws -> Bool {
        try exec("BEGIN IMMEDIATE")
        do {
            let full = try fullRefreshState()
            guard try dirtyRevision(parent: parent) == expectedRevision,
                  expectedFullGeneration == nil || (full.generation == expectedFullGeneration && !full.pending) else {
                try exec("ROLLBACK")
                return false
            }
            let clearIssues = try prepare("DELETE FROM listing_issues WHERE parent=?")
            defer { sqlite3_finalize(clearIssues) }
            bind(parent, 1, clearIssues); try step(clearIssues)
            if issueCount > 0 {
                let recordIssues = try prepare("INSERT INTO listing_issues(parent,count,messages) VALUES(?,?,?)")
                defer { sqlite3_finalize(recordIssues) }
                bind(parent, 1, recordIssues); sqlite3_bind_int64(recordIssues, 2, Int64(issueCount))
                bindText(String(decoding: try JSONEncoder().encode(Array(issues.prefix(100))), as: UTF8.self), 3, recordIssues)
                try step(recordIssues)
            }
            let statement = try prepare("INSERT OR REPLACE INTO trim(parent,epoch,revision,full_generation) VALUES(?,?,?,?)")
            defer { sqlite3_finalize(statement) }
            bind(parent, 1, statement); sqlite3_bind_int64(statement, 2, epoch)
            sqlite3_bind_int64(statement, 3, expectedRevision); sqlite3_bind_int64(statement, 4, expectedFullGeneration ?? -1)
            try step(statement)
            try exec("COMMIT")
            return true
        } catch { try? exec("ROLLBACK"); throw error }
    }

    func hasTrim() throws -> Bool {
        let statement = try prepare("SELECT 1 FROM trim LIMIT 1")
        defer { sqlite3_finalize(statement) }
        let result = sqlite3_step(statement)
        guard result == SQLITE_ROW || result == SQLITE_DONE else { throw Failure.sqlite("trim query") }
        return result == SQLITE_ROW
    }

    func trimOneChunk() throws -> Bool {
        let select = try prepare("SELECT parent,epoch,revision,full_generation FROM trim ORDER BY parent LIMIT 1")
        defer { sqlite3_finalize(select) }
        let result = sqlite3_step(select)
        if result == SQLITE_DONE { return false }
        guard result == SQLITE_ROW, let bytes = sqlite3_column_blob(select, 0) else { throw Failure.sqlite("invalid trim marker") }
        let parent = Data(bytes: bytes, count: Int(sqlite3_column_bytes(select, 0)))
        let epoch = sqlite3_column_int64(select, 1)
        let revision = sqlite3_column_int64(select, 2)
        let generation = sqlite3_column_int64(select, 3)
        try exec("BEGIN IMMEDIATE")
        do {
            let full = try fullRefreshState()
            if try dirtyRevision(parent: parent) != revision || (generation >= 0 && (full.pending || full.generation != generation)) {
                let cancel = try prepare("DELETE FROM trim WHERE parent=?")
                defer { sqlite3_finalize(cancel) }
                bind(parent, 1, cancel); try step(cancel)
                try exec("COMMIT")
                return true
            }
            let stale = try prepare("SELECT st_name FROM nodes WHERE st_parent=? AND epoch<>? AND directory=1 LIMIT 1")
            defer { sqlite3_finalize(stale) }
            bind(parent, 1, stale); sqlite3_bind_int64(stale, 2, epoch)
            let staleResult = sqlite3_step(stale)
            if staleResult == SQLITE_ROW {
                guard let nameBytes = sqlite3_column_blob(stale, 0) else { throw Failure.sqlite("invalid stale directory") }
                let name = Data(bytes: nameBytes, count: Int(sqlite3_column_bytes(stale, 0)))
                var path = parent
                if path.last != 47 { path.append(47) }
                path.append(name)
                try insertPrune(path: path, parent: parent, name: name)
                try exec("COMMIT")
                return true
            }
            guard staleResult == SQLITE_DONE else { throw Failure.sqlite("stale directory query") }
            let dirty = try prepare("INSERT OR IGNORE INTO dirty_links SELECT dev,inode FROM nodes WHERE (st_parent,st_name) IN (SELECT st_parent,st_name FROM nodes WHERE st_parent=? AND epoch<>? AND directory=0 ORDER BY st_parent,st_name LIMIT 128)")
            defer { sqlite3_finalize(dirty) }
            bind(parent, 1, dirty); sqlite3_bind_int64(dirty, 2, epoch); try step(dirty)
            try exec("INSERT OR IGNORE INTO link_rebuild SELECT dev,inode FROM dirty_links")
            let sums = try prepare("SELECT COALESCE(SUM(st_size),0),COALESCE(SUM(charged),0),COUNT(*) FROM (SELECT st_size,charged FROM nodes WHERE st_parent=? AND epoch<>? AND directory=0 ORDER BY st_parent,st_name LIMIT 128)")
            defer { sqlite3_finalize(sums) }
            bind(parent, 1, sums); sqlite3_bind_int64(sums, 2, epoch)
            guard sqlite3_step(sums) == SQLITE_ROW else { throw Failure.sqlite("trim totals") }
            let removedSize = sqlite3_column_int64(sums, 0), removedCharge = sqlite3_column_int64(sums, 1), removedFiles = sqlite3_column_int64(sums, 2)
            let remove = try prepare("DELETE FROM nodes WHERE (st_parent,st_name) IN (SELECT st_parent,st_name FROM nodes WHERE st_parent=? AND epoch<>? AND directory=0 ORDER BY st_parent,st_name LIMIT 128)")
            defer { sqlite3_finalize(remove) }
            bind(parent, 1, remove); sqlite3_bind_int64(remove, 2, epoch)
            try step(remove)
            if sqlite3_changes(db) > 0 {
                try adjustAncestors(parent: parent, logical: -removedSize, allocated: -removedCharge, files: -removedFiles)
            } else {
                let ack = try prepare("UPDATE dirty SET pending=0 WHERE parent=? AND revision=?")
                defer { sqlite3_finalize(ack) }
                bind(parent, 1, ack); sqlite3_bind_int64(ack, 2, revision); try step(ack)
                let completed = try prepare("UPDATE directories SET pending=0 WHERE path=?")
                defer { sqlite3_finalize(completed) }
                bind(parent, 1, completed); try step(completed)
                let clear = try prepare("DELETE FROM trim WHERE parent=?")
                defer { sqlite3_finalize(clear) }
                bind(parent, 1, clear); try step(clear)
            }
            try exec("COMMIT")
            return true
        } catch { try? exec("ROLLBACK"); throw error }
    }

    func markDirty(parent: Data) throws {
        try exec("BEGIN IMMEDIATE")
        do {
            guard try directory(path: parent) != nil else { throw Failure.sqlite("unknown changed directory") }
            let statement = try prepare("INSERT INTO dirty(parent,revision,pending) VALUES(?,1,1) ON CONFLICT(parent) DO UPDATE SET revision=revision+1,pending=1")
            defer { sqlite3_finalize(statement) }
            bind(parent, 1, statement)
            try step(statement)
            let pending = try prepare("UPDATE directories SET pending=1 WHERE path=?")
            defer { sqlite3_finalize(pending) }
            bind(parent, 1, pending)
            try step(pending)
            try exec("COMMIT")
        } catch { try? exec("ROLLBACK"); throw error }
    }

    private func insertPrune(path: Data, parent: Data, name: Data) throws {
        let statement = try prepare("INSERT OR IGNORE INTO prune(path,parent,name) VALUES(?,?,?)")
        defer { sqlite3_finalize(statement) }
        bind(path, 1, statement); bind(parent, 2, statement); bind(name, 3, statement)
        try step(statement)
    }

    func startPrune(path: Data, parent: Data, name: Data) throws {
        try exec("BEGIN IMMEDIATE")
        do {
            try insertPrune(path: path, parent: parent, name: name)
            try exec("COMMIT")
        } catch { try? exec("ROLLBACK"); throw error }
    }

    func invalidateDirectory(path: Data, parent: Data, name: Data) throws {
        try exec("BEGIN IMMEDIATE")
        do {
            guard try directory(path: parent) != nil else { throw Failure.sqlite("missing parent for retry") }
            try insertPrune(path: path, parent: parent, name: name)
            let revision = try prepare("INSERT INTO dirty(parent,revision,pending) VALUES(?,1,1) ON CONFLICT(parent) DO UPDATE SET revision=revision+1,pending=1")
            defer { sqlite3_finalize(revision) }
            bind(parent, 1, revision); try step(revision)
            let pending = try prepare("UPDATE directories SET pending=1 WHERE path=?")
            defer { sqlite3_finalize(pending) }
            bind(parent, 1, pending); try step(pending)
            try exec("COMMIT")
        } catch { try? exec("ROLLBACK"); throw error }
    }

    func blockDirectory(path: Data, parent: Data, name: Data, message: String) throws {
        try exec("BEGIN IMMEDIATE")
        do {
            let blocked = try prepare("INSERT OR REPLACE INTO blocked_directories(path) VALUES(?)")
            defer { sqlite3_finalize(blocked) }
            bind(path, 1, blocked); try step(blocked)
            let issue = try prepare("INSERT OR REPLACE INTO stage_issues(path,message) VALUES(?,?)")
            defer { sqlite3_finalize(issue) }
            bind(path, 1, issue); bindText(message, 2, issue); try step(issue)
            try insertPrune(path: path, parent: parent, name: name)
            try exec("COMMIT")
        } catch { try? exec("ROLLBACK"); throw error }
    }

    func fullRefreshState() throws -> (generation: Int64, pending: Bool) {
        let statement = try prepare("SELECT generation,pending FROM full_refresh WHERE singleton=1")
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW else { throw Failure.sqlite("missing full refresh state") }
        return (sqlite3_column_int64(statement, 0), sqlite3_column_int(statement, 1) != 0)
    }

    func markFullRefresh() throws {
        try exec("UPDATE full_refresh SET generation=generation+1,pending=1 WHERE singleton=1")
    }

    func markFullRefreshChunk() throws -> Bool {
        guard try fullRefreshState().pending else { return false }
        try exec("BEGIN IMMEDIATE")
        do {
            let state = try fullRefreshState()
            guard state.pending else { try exec("COMMIT"); return false }
            let update = try prepare("UPDATE directories SET pending=1,full_generation=? WHERE path IN (SELECT path FROM directories WHERE full_generation<? LIMIT 128)")
            defer { sqlite3_finalize(update) }
            sqlite3_bind_int64(update, 1, state.generation); sqlite3_bind_int64(update, 2, state.generation)
            try step(update)
            if sqlite3_changes(db) == 0 {
                let finish = try prepare("UPDATE full_refresh SET pending=0 WHERE singleton=1 AND generation=?")
                defer { sqlite3_finalize(finish) }
                sqlite3_bind_int64(finish, 1, state.generation)
                try step(finish)
            }
            try exec("COMMIT")
            return true
        } catch { try? exec("ROLLBACK"); throw error }
    }

    func hasPrune() throws -> Bool {
        let statement = try prepare("SELECT 1 FROM prune LIMIT 1")
        defer { sqlite3_finalize(statement) }
        let result = sqlite3_step(statement)
        guard result == SQLITE_ROW || result == SQLITE_DONE else { throw Failure.sqlite("prune query") }
        return result == SQLITE_ROW
    }

    // One transaction removes at most 128 descendant rows; the marker survives a crash.
    func pruneOneChunk() throws -> Bool {
        let select = try prepare("SELECT path,parent,name FROM prune ORDER BY path LIMIT 1")
        defer { sqlite3_finalize(select) }
        let result = sqlite3_step(select)
        if result == SQLITE_DONE { return false }
        guard result == SQLITE_ROW,
              let pathBytes = sqlite3_column_blob(select, 0),
              let parentBytes = sqlite3_column_blob(select, 1),
              let nameBytes = sqlite3_column_blob(select, 2) else { throw Failure.sqlite("invalid prune marker") }
        let path = Data(bytes: pathBytes, count: Int(sqlite3_column_bytes(select, 0)))
        let parent = Data(bytes: parentBytes, count: Int(sqlite3_column_bytes(select, 1)))
        let name = Data(bytes: nameBytes, count: Int(sqlite3_column_bytes(select, 2)))
        var low = path; low.append(47)
        var high = path; high.append(48)
        try exec("BEGIN IMMEDIATE")
        do {
            let dirty = try prepare("INSERT OR IGNORE INTO dirty_links SELECT dev,inode FROM nodes WHERE directory=0 AND (st_parent,st_name) IN (SELECT st_parent,st_name FROM nodes WHERE st_parent=? OR (st_parent>=? AND st_parent<?) ORDER BY st_parent,st_name LIMIT 128)")
            defer { sqlite3_finalize(dirty) }
            bind(path, 1, dirty); bind(low, 2, dirty); bind(high, 3, dirty); try step(dirty)
            try exec("INSERT OR IGNORE INTO link_rebuild SELECT dev,inode FROM dirty_links")
            let sums = try prepare("SELECT st_parent,st_size,charged,directory FROM nodes WHERE st_parent=? OR (st_parent>=? AND st_parent<?) ORDER BY st_parent,st_name LIMIT 128")
            defer { sqlite3_finalize(sums) }
            bind(path, 1, sums); bind(low, 2, sums); bind(high, 3, sums)
            var deltas: [Data: (Int64, Int64, Int64)] = [:]
            while true {
                let status = sqlite3_step(sums)
                if status == SQLITE_DONE { break }
                guard status == SQLITE_ROW, let parentBytes = sqlite3_column_blob(sums, 0) else { throw Failure.sqlite("prune totals") }
                if sqlite3_column_int(sums, 3) != 0 { continue }
                let parent = Data(bytes: parentBytes, count: Int(sqlite3_column_bytes(sums, 0)))
                let previous = deltas[parent] ?? (0, 0, 0)
                deltas[parent] = (previous.0 + sqlite3_column_int64(sums, 1), previous.1 + sqlite3_column_int64(sums, 2), previous.2 + 1)
            }
            let removeRows = try prepare("DELETE FROM nodes WHERE (st_parent,st_name) IN (SELECT st_parent,st_name FROM nodes WHERE st_parent=? OR (st_parent>=? AND st_parent<?) ORDER BY st_parent,st_name LIMIT 128)")
            defer { sqlite3_finalize(removeRows) }
            bind(path, 1, removeRows); bind(low, 2, removeRows); bind(high, 3, removeRows)
            try step(removeRows)
            let removed = sqlite3_changes(db)
            for (parent, delta) in deltas { try adjustAncestors(parent: parent, logical: -delta.0, allocated: -delta.1, files: -delta.2) }
            if removed == 0 {
                let removeDirs = try prepare("DELETE FROM directories WHERE path IN (SELECT path FROM directories WHERE path=? OR (path>=? AND path<?) LIMIT 128)")
                defer { sqlite3_finalize(removeDirs) }
                bind(path, 1, removeDirs); bind(low, 2, removeDirs); bind(high, 3, removeDirs)
                try step(removeDirs)
                if sqlite3_changes(db) == 0 {
                    let descendantIssues = try prepare("DELETE FROM stage_issues WHERE path IN (SELECT path FROM stage_issues WHERE path>=? AND path<? LIMIT 128)")
                    defer { sqlite3_finalize(descendantIssues) }
                    bind(low, 1, descendantIssues); bind(high, 2, descendantIssues); try step(descendantIssues)
                    let removedIssues = sqlite3_changes(db)
                    let listingIssues = try prepare("DELETE FROM listing_issues WHERE parent IN (SELECT parent FROM listing_issues WHERE parent=? OR (parent>=? AND parent<?) LIMIT 128)")
                    defer { sqlite3_finalize(listingIssues) }
                    bind(path, 1, listingIssues); bind(low, 2, listingIssues); bind(high, 3, listingIssues); try step(listingIssues)
                    let removedListingIssues = sqlite3_changes(db)
                    let descendantBlocked = try prepare("DELETE FROM blocked_directories WHERE path IN (SELECT path FROM blocked_directories WHERE path>=? AND path<? LIMIT 128)")
                    defer { sqlite3_finalize(descendantBlocked) }
                    bind(low, 1, descendantBlocked); bind(high, 2, descendantBlocked); try step(descendantBlocked)
                    if removedIssues > 0 || removedListingIssues > 0 || sqlite3_changes(db) > 0 { try exec("COMMIT"); return true }
                    let blocked = try prepare("SELECT 1 FROM blocked_directories WHERE path=?")
                    defer { sqlite3_finalize(blocked) }
                    bind(path, 1, blocked)
                    let blockedResult = sqlite3_step(blocked)
                    guard blockedResult == SQLITE_ROW || blockedResult == SQLITE_DONE else { throw Failure.sqlite("blocked directory query") }
                    if blockedResult == SQLITE_ROW {
                        let mark = try prepare("UPDATE nodes SET accessible=0,size=0,allocated=0,file_count=0 WHERE st_parent=? AND st_name=?")
                        defer { sqlite3_finalize(mark) }
                        bind(parent, 1, mark); bind(name, 2, mark); try step(mark)
                        let clearBlocked = try prepare("DELETE FROM blocked_directories WHERE path=?")
                        defer { sqlite3_finalize(clearBlocked) }
                        bind(path, 1, clearBlocked); try step(clearBlocked)
                    } else {
                        let removeNode = try prepare("DELETE FROM nodes WHERE st_parent=? AND st_name=?")
                        defer { sqlite3_finalize(removeNode) }
                        bind(parent, 1, removeNode); bind(name, 2, removeNode); try step(removeNode)
                        let clearIssue = try prepare("DELETE FROM stage_issues WHERE path=?")
                        defer { sqlite3_finalize(clearIssue) }
                        bind(path, 1, clearIssue); try step(clearIssue)
                    }
                    let removeMarker = try prepare("DELETE FROM prune WHERE path=?")
                    defer { sqlite3_finalize(removeMarker) }
                    bind(path, 1, removeMarker)
                    try step(removeMarker)
                }
            }
            try exec("COMMIT")
            return true
        } catch { try? exec("ROLLBACK"); throw error }
    }

    struct QueuedDirectory {
        let path: Data
        let device: Int32
        let inode: UInt64
    }

    func registerDirectory(path: Data, device: Int32, inode: UInt64) throws {
        let statement = try prepare("INSERT OR IGNORE INTO directories(path,device,inode,pending) VALUES(?,?,?,1)")
        defer { sqlite3_finalize(statement) }
        let check = try prepare("SELECT device,inode FROM directories WHERE path=?")
        defer { sqlite3_finalize(check) }
        try registerDirectory(path: path, device: device, inode: inode, insert: statement, check: check)
    }

    private func registerDirectory(path: Data, device: Int32, inode: UInt64, insert: OpaquePointer, check: OpaquePointer) throws {
        sqlite3_reset(insert); sqlite3_clear_bindings(insert)
        bind(path, 1, insert); sqlite3_bind_int64(insert, 2, Int64(device))
        sqlite3_bind_int64(insert, 3, Int64(bitPattern: inode)); try step(insert)
        sqlite3_reset(check); sqlite3_clear_bindings(check); bind(path, 1, check)
        let status = sqlite3_step(check)
        guard status == SQLITE_ROW || status == SQLITE_DONE else { throw Failure.sqlite("directory lookup") }
        let matches = status == SQLITE_ROW && Int32(sqlite3_column_int64(check, 0)) == device
            && UInt64(bitPattern: sqlite3_column_int64(check, 1)) == inode
        sqlite3_reset(check)
        guard matches else {
            throw Failure.sqlite("directory identity changed; subtree reconciliation required")
        }
    }

    func directory(path: Data) throws -> QueuedDirectory? {
        let statement = try prepare("SELECT device,inode FROM directories WHERE path=?")
        defer { sqlite3_finalize(statement) }
        bind(path, 1, statement)
        let result = sqlite3_step(statement)
        if result == SQLITE_DONE { return nil }
        guard result == SQLITE_ROW else { throw Failure.sqlite("directory lookup") }
        return QueuedDirectory(path: path, device: Int32(sqlite3_column_int64(statement, 0)), inode: UInt64(bitPattern: sqlite3_column_int64(statement, 1)))
    }

    func nextDirectory() throws -> QueuedDirectory? {
        func first(_ sql: String) throws -> QueuedDirectory? {
            let statement = try prepare(sql)
            defer { sqlite3_finalize(statement) }
            let result = sqlite3_step(statement)
            if result == SQLITE_DONE { return nil }
            guard result == SQLITE_ROW, let bytes = sqlite3_column_blob(statement, 0) else { throw Failure.sqlite("queue lookup") }
            return QueuedDirectory(path: Data(bytes: bytes, count: Int(sqlite3_column_bytes(statement, 0))),
                device: Int32(sqlite3_column_int64(statement, 1)), inode: UInt64(bitPattern: sqlite3_column_int64(statement, 2)))
        }
        if let dirty = try first("SELECT directories.path,directories.device,directories.inode FROM dirty JOIN directories ON directories.path=dirty.parent WHERE dirty.pending=1 AND directories.pending=1 ORDER BY dirty.parent LIMIT 1") { return dirty }
        return try first("SELECT path,device,inode FROM directories WHERE pending=1 ORDER BY path LIMIT 1")
    }

    func dirtyRevision(parent: Data) throws -> Int64 {
        let statement = try prepare("SELECT revision FROM dirty WHERE parent=?")
        defer { sqlite3_finalize(statement) }
        bind(parent, 1, statement)
        let result = sqlite3_step(statement)
        if result == SQLITE_DONE { return 0 }
        guard result == SQLITE_ROW else { throw Failure.sqlite("dirty revision") }
        return sqlite3_column_int64(statement, 0)
    }

    func isDirty(parent: Data) throws -> Bool {
        let statement = try prepare("SELECT pending FROM dirty WHERE parent=?")
        defer { sqlite3_finalize(statement) }
        bind(parent, 1, statement)
        let result = sqlite3_step(statement)
        if result == SQLITE_DONE { return false }
        guard result == SQLITE_ROW else { throw Failure.sqlite("dirty state") }
        return sqlite3_column_int(statement, 0) != 0
    }

    private func adjustAncestors(parent: Data, logical: Int64, allocated: Int64, files: Int64) throws {
        guard logical != 0 || allocated != 0 || files != 0 else { return }
        guard let root = indexRoot else { throw Failure.sqlite("root not initialized") }
        let statement = try prepare("UPDATE directories SET logical=logical+?,allocated=allocated+?,files=files+? WHERE path=?")
        defer { sqlite3_finalize(statement) }
        var path = parent
        while true {
            sqlite3_reset(statement); sqlite3_clear_bindings(statement)
            sqlite3_bind_int64(statement, 1, logical); sqlite3_bind_int64(statement, 2, allocated)
            sqlite3_bind_int64(statement, 3, files); bind(path, 4, statement)
            try step(statement)
            guard sqlite3_changes(db) == 1 else { throw Failure.sqlite("missing ancestor") }
            if path == root { return }
            guard let slash = path.lastIndex(of: 47) else { throw Failure.sqlite("ancestor escapes root") }
            path = slash == 0 ? Data([47]) : Data(path.prefix(upTo: slash))
        }
    }

    func folderTotals(path: Data) throws -> (logical: Int64, allocated: Int64, count: Int64)? {
        let statement = try prepare("SELECT logical,allocated,files FROM directories WHERE path=?")
        defer { sqlite3_finalize(statement) }
        bind(path, 1, statement)
        let result = sqlite3_step(statement)
        if result == SQLITE_DONE { return nil }
        guard result == SQLITE_ROW else { throw Failure.sqlite("folder totals") }
        return (sqlite3_column_int64(statement, 0), sqlite3_column_int64(statement, 1), sqlite3_column_int64(statement, 2))
    }

    func reconcileLinksChunk(cancellation: ScanCancellation? = nil) throws -> Bool {
        let pending = try prepare("SELECT 1 FROM dirty_links LIMIT 1")
        let status = sqlite3_step(pending)
        sqlite3_finalize(pending)
        guard status == SQLITE_ROW || status == SQLITE_DONE else { throw Failure.sqlite("dirty link state") }
        if status == SQLITE_DONE { return false }
        try exec("BEGIN IMMEDIATE")
        do {
            let select = try prepare("SELECT dev,inode FROM dirty_links ORDER BY dev,inode LIMIT 128")
            var identities: [(Int64, Int64)] = []
            while true {
                let status = sqlite3_step(select)
                if status == SQLITE_DONE { break }
                guard status == SQLITE_ROW else { sqlite3_finalize(select); throw Failure.sqlite("dirty link query") }
                identities.append((sqlite3_column_int64(select, 0), sqlite3_column_int64(select, 1)))
            }
            sqlite3_finalize(select)
            guard !identities.isEmpty else { try exec("COMMIT"); return false }
            let old = try prepare("SELECT parent,name FROM owners WHERE dev=? AND inode=?")
            let values = try prepare("SELECT charged,st_allocated FROM nodes WHERE st_parent=? AND st_name=? AND dev=? AND inode=?")
            let clear = try prepare("UPDATE nodes SET charged=0,allocated=0 WHERE st_parent=? AND st_name=? AND dev=? AND inode=?")
            let next = try prepare("SELECT st_parent,st_name FROM nodes WHERE file_identity=? AND is_dir=0 AND directory=0")
            let candidate = try prepare("SELECT c.parent,c.name FROM link_candidates c JOIN nodes n ON n.st_parent=c.parent AND n.st_name=c.name AND n.dev=c.dev AND n.inode=c.inode AND n.directory=0 WHERE c.dev=? AND c.inode=?")
            let rebuild = try prepare("SELECT 1 FROM link_rebuild WHERE dev=? AND inode=?")
            let clearCandidates = try prepare("DELETE FROM link_candidates WHERE dev=? AND inode=?")
            let clearRebuild = try prepare("DELETE FROM link_rebuild WHERE dev=? AND inode=?")
            let charge = try prepare("UPDATE nodes SET charged=st_allocated,allocated=st_allocated WHERE st_parent=? AND st_name=? AND dev=? AND inode=?")
            let setOwner = try prepare("INSERT OR REPLACE INTO owners VALUES(?,?,?,?)")
            let removeOwner = try prepare("DELETE FROM owners WHERE dev=? AND inode=?")
            let done = try prepare("DELETE FROM dirty_links WHERE dev=? AND inode=?")
            defer { for statement in [old, values, clear, next, candidate, rebuild, clearCandidates, clearRebuild, charge, setOwner, removeOwner, done] { sqlite3_finalize(statement) } }
            func identity(_ statement: OpaquePointer, _ device: Int64, _ inode: Int64) {
                sqlite3_reset(statement); sqlite3_clear_bindings(statement)
                sqlite3_bind_int64(statement, 1, device); sqlite3_bind_int64(statement, 2, inode)
            }
            func pair(_ statement: OpaquePointer) throws -> (Data, Data)? {
                let status = sqlite3_step(statement)
                if status == SQLITE_DONE { return nil }
                guard status == SQLITE_ROW, let parent = sqlite3_column_blob(statement, 0),
                      let name = sqlite3_column_blob(statement, 1) else { throw Failure.sqlite("invalid link owner") }
                return (Data(bytes: parent, count: Int(sqlite3_column_bytes(statement, 0))),
                        Data(bytes: name, count: Int(sqlite3_column_bytes(statement, 1))))
            }
            func currentValues(_ parent: Data, _ name: Data, _ device: Int64, _ inode: Int64) throws -> (Int64, Int64)? {
                sqlite3_reset(values); sqlite3_clear_bindings(values)
                bind(parent, 1, values); bind(name, 2, values)
                sqlite3_bind_int64(values, 3, device); sqlite3_bind_int64(values, 4, inode)
                let status = sqlite3_step(values)
                if status == SQLITE_DONE { return nil }
                guard status == SQLITE_ROW else { throw Failure.sqlite("link charge query") }
                return (sqlite3_column_int64(values, 0), sqlite3_column_int64(values, 1))
            }
            for (device, inode) in identities {
                identity(old, device, inode)
                let oldOwner = try pair(old)
                if let (parent, name) = oldOwner {
                    let previous = try currentValues(parent, name, device, inode)?.0 ?? 0
                    sqlite3_reset(clear); sqlite3_clear_bindings(clear)
                    bind(parent, 1, clear); bind(name, 2, clear)
                    sqlite3_bind_int64(clear, 3, device); sqlite3_bind_int64(clear, 4, inode); try step(clear)
                    try adjustAncestors(parent: parent, logical: 0, allocated: -previous, files: 0)
                }
                var winner: (Data, Data)?
                var inspected = 0
                identity(rebuild, device, inode)
                let rebuildStatus = sqlite3_step(rebuild)
                guard rebuildStatus == SQLITE_ROW || rebuildStatus == SQLITE_DONE else { throw Failure.sqlite("link rebuild query") }
                identity(candidate, device, inode)
                let firstCandidate = try pair(candidate)
                let useFullScan = rebuildStatus == SQLITE_ROW || firstCandidate == nil
                if !useFullScan, let (parent, name) = oldOwner,
                   try currentValues(parent, name, device, inode) != nil { winner = (parent, name) }
                let listing = useFullScan ? next : candidate
                if useFullScan {
                    sqlite3_reset(next); sqlite3_clear_bindings(next)
                    bindText("\(Int32(truncatingIfNeeded: device)):\(UInt64(bitPattern: inode))", 1, next)
                }
                var item = useFullScan ? try pair(listing) : firstCandidate
                while let candidate = item {
                    if inspected & 255 == 0, cancellation?.isCancelled == true { throw CancellationError() }
                    inspected += 1
                    if let current = winner { if HardLinkPathOrder.precedes(HardLinkPathOrder.path(candidate.0, candidate.1), HardLinkPathOrder.path(current.0, current.1)) { winner = candidate } }
                    else { winner = candidate }
                    item = try pair(listing)
                }
                if let (parent, name) = winner {
                    guard let (previous, allocation) = try currentValues(parent, name, device, inode) else { throw Failure.sqlite("missing next link owner") }
                    sqlite3_reset(charge); sqlite3_clear_bindings(charge)
                    bind(parent, 1, charge); bind(name, 2, charge)
                    sqlite3_bind_int64(charge, 3, device); sqlite3_bind_int64(charge, 4, inode); try step(charge)
                    try adjustAncestors(parent: parent, logical: 0, allocated: allocation - previous, files: 0)
                    identity(setOwner, device, inode); bind(parent, 3, setOwner); bind(name, 4, setOwner); try step(setOwner)
                } else { identity(removeOwner, device, inode); try step(removeOwner) }
                identity(clearCandidates, device, inode); try step(clearCandidates)
                identity(clearRebuild, device, inode); try step(clearRebuild)
                identity(done, device, inode); try step(done)
            }
            try exec("COMMIT")
            return true
        } catch { try? exec("ROLLBACK"); throw error }
    }

    func charge(path: Data) throws -> Int64? {
        let statement = try prepare("SELECT charged FROM nodes WHERE st_path=?")
        defer { sqlite3_finalize(statement) }
        bind(path, 1, statement)
        let result = sqlite3_step(statement)
        if result == SQLITE_DONE { return nil }
        guard result == SQLITE_ROW else { throw Failure.sqlite("charge lookup") }
        return sqlite3_column_int64(statement, 0)
    }

    func fileTotals() throws -> (logical: Int64, allocated: Int64, count: Int64) {
        let statement = try prepare("SELECT COALESCE(SUM(st_size),0),COALESCE(SUM(charged),0),COUNT(*) FROM nodes WHERE directory=0")
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW else { throw Failure.sqlite("file totals") }
        return (sqlite3_column_int64(statement, 0), sqlite3_column_int64(statement, 1), sqlite3_column_int64(statement, 2))
    }

    func testCrashDuringTransaction(parent: Data) throws -> Never {
        try exec("BEGIN IMMEDIATE")
        let statement = try prepare("INSERT INTO nodes(st_parent,st_name,dev,inode,directory,st_is_link,is_mount,st_size,st_allocated,links,epoch,st_path,charged,folded,identity,path) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,CAST(? AS TEXT))")
        bind(parent, 1, statement); bind(Data("uncommitted".utf8), 2, statement)
        for index in 3...11 { sqlite3_bind_int64(statement, Int32(index), 0) }
        var path = parent; if path.last != 47 { path.append(47) }; path.append(contentsOf: "uncommitted".utf8)
        bind(path, 12, statement); bind(path, 16, statement); sqlite3_bind_int64(statement, 13, 0)
        bind(Data("uncommitted".utf8), 14, statement); bind(Data("0:0".utf8), 15, statement)
        try step(statement)
        kill(getpid(), SIGKILL)
        fatalError("SIGKILL returned")
    }

    func count(parent: Data) throws -> Int {
        let statement = try prepare("SELECT COUNT(*) FROM nodes WHERE st_parent=?")
        defer { sqlite3_finalize(statement) }
        bind(parent, 1, statement)
        guard sqlite3_step(statement) == SQLITE_ROW else { throw Failure.sqlite("count") }
        return Int(sqlite3_column_int64(statement, 0))
    }
}
