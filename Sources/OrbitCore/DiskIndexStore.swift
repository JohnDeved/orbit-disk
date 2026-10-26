import CryptoKit
import CSQLite
import Darwin
import Foundation

public struct IndexSnapshot: Sendable, Identifiable {
    public let id: String
    public let rootURL: URL
    public let databaseURL: URL
    public let scannedAt: Date
    public let elapsed: TimeInterval
    public let skippedCount: Int
    public let issues: [String]
    public let eventID: UInt64
    public let eventIdentity: String
}

public struct IndexSearchResult: Sendable {
    public let nodes: [DiskNode]
    public let totalCount: Int
}

public struct IndexReplacement: Sendable {
    public let path: String
    public let result: ScanResult?

    public init(path: String, result: ScanResult?) {
        self.path = path
        self.result = result
    }
}

public enum DiskIndexError: LocalizedError {
    case invalid(String)
    case database(String)
    case staleSnapshot

    public var errorDescription: String? {
        switch self {
        case .invalid(let message), .database(let message): return message
        case .staleSnapshot: return "The disk index changed. Reload this folder."
        }
    }
}

public struct DiskIndexStore: Sendable {
    public let directory: URL
    private static let version: Int64 = 3
    private static let applicationID: Int64 = 0x4F524254
    private static let mutationLock = NSLock()
    private static let nodeColumns = "path,name,is_dir,is_link,accessible,size,allocated,file_count,raw_alloc,file_identity"

    public init(directory: URL) { self.directory = directory.standardizedFileURL }

    public func savedSnapshot(for root: URL) -> IndexSnapshot? {
        let rootPath = Self.path(root)
        let file = databaseURL(for: root)
        do {
            let db = try IndexDatabase(file, writable: false)
            let snapshot = try readSnapshot(db, file: file)
            guard snapshot.rootURL.path == rootPath,
                  let node = try readNode(rootPath, db: db), node.isDirectory else { return nil }
            return snapshot
        } catch { return nil }
    }

    public func save(_ result: ScanResult, eventID: UInt64, eventIdentity: String,
                     cancellation: ScanCancellation = ScanCancellation()) throws -> IndexSnapshot {
        Self.mutationLock.lock()
        defer { Self.mutationLock.unlock() }
        try check(cancellation)
        guard result.root.isDirectory else { throw DiskIndexError.invalid("The index root must be a folder.") }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        let destination = databaseURL(for: result.root.url)
        let temporary = directory.appendingPathComponent(".\(UUID().uuidString).sqlite")
        defer { removeTemporary(temporary) }
        let descriptor = Darwin.open(temporary.path, O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else { throw DiskIndexError.database(String(cString: strerror(errno))) }
        close(descriptor)
        let snapshot = IndexSnapshot(id: UUID().uuidString, rootURL: result.root.url.standardizedFileURL,
                                     databaseURL: destination, scannedAt: Date(), elapsed: result.elapsed,
                                     skippedCount: result.skippedCount, issues: result.issues,
                                     eventID: eventID, eventIdentity: eventIdentity)
        let db = try IndexDatabase(temporary, writable: true, cancellation: cancellation)
        try db.execute("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=-8192;")
        try db.execute("PRAGMA application_id=\(Self.applicationID); PRAGMA user_version=\(Self.version); BEGIN IMMEDIATE;")
        try db.execute("""
            CREATE TABLE metadata (singleton INTEGER PRIMARY KEY CHECK(singleton=1), snapshot_id TEXT NOT NULL,
              root_path TEXT NOT NULL, scanned_at REAL NOT NULL, elapsed REAL NOT NULL, skipped INTEGER NOT NULL,
              issues TEXT NOT NULL, event_id TEXT NOT NULL, event_identity TEXT NOT NULL);
            CREATE TABLE nodes (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, parent INTEGER, name TEXT NOT NULL, search_name TEXT NOT NULL,
              is_dir INTEGER NOT NULL, is_link INTEGER NOT NULL, accessible INTEGER NOT NULL,
              size INTEGER NOT NULL, allocated INTEGER NOT NULL, file_count INTEGER NOT NULL,
              raw_alloc INTEGER NOT NULL, file_identity TEXT);
            """)
        try insertTree(result.root, parent: nil, db: db, cancellation: cancellation)
        try db.execute("""
            CREATE INDEX children_by_size ON nodes(parent,size DESC,name);
            CREATE INDEX nodes_by_size ON nodes(size DESC,id,search_name,parent);
            CREATE INDEX file_identities ON nodes(file_identity) WHERE is_dir=0 AND file_identity IS NOT NULL;
            """)
        try writeSnapshot(snapshot, db: db)
        try db.execute("COMMIT;")
        try db.close()
        try publish(temporary, to: destination, cancellation: cancellation)
        return snapshot
    }

    public func update(snapshot: IndexSnapshot, replacements: [IndexReplacement], eventID: UInt64,
                       eventIdentity: String, cancellation: ScanCancellation = ScanCancellation()) throws -> IndexSnapshot {
        Self.mutationLock.lock()
        defer { Self.mutationLock.unlock() }
        try check(cancellation)
        let root = snapshot.rootURL.path
        let paths = replacements.map { Self.path(URL(fileURLWithPath: $0.path)) }
        for replacement in replacements {
            if let result = replacement.result, result.skippedCount > 0 || !result.root.isAccessible {
                throw DiskIndexError.invalid("A changed folder could not be scanned completely. The saved index was preserved; check folder access and refresh.")
            }
        }
        for (index, path) in paths.enumerated() {
            guard Self.contains(path, in: root) else { throw DiskIndexError.invalid("An updated folder is outside the index.") }
            for previous in paths.prefix(index) where Self.contains(path, in: previous) || Self.contains(previous, in: path) {
                throw DiskIndexError.invalid("Updated folders overlap. Combine their refreshes first.")
            }
        }
        let temporary = directory.appendingPathComponent(".\(UUID().uuidString).sqlite")
        defer { removeTemporary(temporary) }
        if clonefile(snapshot.databaseURL.path, temporary.path, 0) != 0 {
            try? FileManager.default.removeItem(at: temporary)
            try FileManager.default.copyItem(at: snapshot.databaseURL, to: temporary)
        }
        let db = try IndexDatabase(temporary, writable: true, cancellation: cancellation)
        try validate(snapshot, db: db)
        try db.execute("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; BEGIN IMMEDIATE;")
        var identities = Set<String>()
        var affectedParents = Set<String>()
        var issues = snapshot.issues
        var skipped = snapshot.skippedCount
        for (replacement, path) in zip(replacements, paths) {
            try check(cancellation)
            guard path != root || replacement.result != nil else {
                throw DiskIndexError.invalid("The indexed folder no longer exists.")
            }
            let predicate = subtreePredicate(path)
            let oldIDs = try db.strings("SELECT DISTINCT file_identity FROM nodes WHERE is_dir=0 AND file_identity IS NOT NULL AND (\(predicate.sql))", predicate.values)
            identities.formUnion(oldIDs)
            try db.run("DELETE FROM nodes WHERE \(predicate.sql)", predicate.values)
            let parent = path == root ? nil : URL(fileURLWithPath: path).deletingLastPathComponent().path
            if let parent {
                guard try readNode(parent, db: db)?.isDirectory == true else {
                    throw DiskIndexError.invalid("The parent folder is missing from the index. Refresh its parent first.")
                }
                addAncestors(parent, root: root, to: &affectedParents)
            }
            if let result = replacement.result {
                guard Self.path(result.root.url) == path else { throw DiskIndexError.invalid("An updated scan has the wrong root.") }
                try insertTree(result.root, parent: parent, db: db, cancellation: cancellation)
                let newIDs = try db.strings("SELECT DISTINCT file_identity FROM nodes WHERE is_dir=0 AND file_identity IS NOT NULL AND (\(predicate.sql))", predicate.values)
                identities.formUnion(newIDs)
                skipped += result.skippedCount
                for issue in result.issues where !issues.contains(issue) && issues.count < 100 { issues.append(issue) }
                if path == root { skipped = result.skippedCount; issues = result.issues }
            }
        }
        for identity in identities {
            try check(cancellation)
            let linkedPaths = try db.strings("SELECT path FROM nodes WHERE is_dir=0 AND file_identity=? ORDER BY path", [.text(identity)])
            guard let owner = linkedPaths.first else { continue }
            try db.run("UPDATE nodes SET allocated=CASE WHEN path=? THEN raw_alloc ELSE 0 END WHERE is_dir=0 AND file_identity=?", [.text(owner), .text(identity)])
            for path in linkedPaths { addAncestors(URL(fileURLWithPath: path).deletingLastPathComponent().path, root: root, to: &affectedParents) }
        }
        for parent in affectedParents.sorted(by: { $0.count > $1.count }) {
            try check(cancellation)
            try db.run("""
                UPDATE nodes SET size=(SELECT COALESCE(SUM(size),0) FROM nodes WHERE parent=(SELECT id FROM nodes WHERE path=?)),
                  allocated=(SELECT COALESCE(SUM(allocated),0) FROM nodes WHERE parent=(SELECT id FROM nodes WHERE path=?)),
                  file_count=(SELECT COALESCE(SUM(file_count),0) FROM nodes WHERE parent=(SELECT id FROM nodes WHERE path=?))
                WHERE path=? AND is_dir=1
                """, Array(repeating: .text(parent), count: 4))
        }
        let next = IndexSnapshot(id: UUID().uuidString, rootURL: snapshot.rootURL, databaseURL: snapshot.databaseURL,
                                 scannedAt: Date(), elapsed: replacements.reduce(0) { $0 + ($1.result?.elapsed ?? 0) },
                                 skippedCount: skipped, issues: issues, eventID: eventID, eventIdentity: eventIdentity)
        try writeSnapshot(next, db: db)
        try db.execute("COMMIT;")
        try db.close()
        try publish(temporary, to: snapshot.databaseURL, cancellation: cancellation)
        return next
    }

    public func loadFolder(path: String, snapshot: IndexSnapshot) throws -> DiskNode {
        let db = try open(snapshot)
        let path = Self.path(URL(fileURLWithPath: path))
        guard let folder = try readNode(path, db: db), folder.isDirectory else {
            throw DiskIndexError.invalid("This folder is no longer in the index.")
        }
        var budget = 2_000
        let children = try readChildren(path, limit: nil, db: db).map { try preview($0, depth: 1, budget: &budget, db: db) }
        return replacingChildren(folder, children)
    }

    public func node(path: String, snapshot: IndexSnapshot) throws -> DiskNode? {
        try readNode(Self.path(URL(fileURLWithPath: path)), db: open(snapshot))
    }

    public func ancestors(path: String, snapshot: IndexSnapshot) throws -> [DiskNode] {
        let db = try open(snapshot)
        var path = Self.path(URL(fileURLWithPath: path))
        guard Self.contains(path, in: snapshot.rootURL.path) else { return [] }
        var result: [DiskNode] = []
        while let node = try readNode(path, db: db) {
            result.append(node)
            if path == snapshot.rootURL.path { break }
            let parent = URL(fileURLWithPath: path).deletingLastPathComponent().path
            if parent == path { break }
            path = parent
        }
        return result.reversed()
    }

    public func search(query: String, snapshot: IndexSnapshot, limit: Int = 300,
                       cancellation: ScanCancellation = ScanCancellation()) throws -> IndexSearchResult {
        try check(cancellation)
        let needle = Self.fold(query.trimmingCharacters(in: .whitespacesAndNewlines))
        guard !needle.isEmpty else { return IndexSearchResult(nodes: [], totalCount: 0) }
        let db = try open(snapshot, cancellation: cancellation)
        let values: [IndexValue] = [.text(needle)]
        let count = try db.integer("SELECT COUNT(*) FROM nodes WHERE parent IS NOT NULL AND instr(search_name,?)>0", values)
        let nodes = try readNodes("SELECT \(Self.nodeColumns) FROM nodes WHERE parent IS NOT NULL AND instr(search_name,?)>0 ORDER BY size DESC,id LIMIT ?",
                                  values + [.integer(Int64(max(0, min(limit, 10_000))))], db: db)
        try check(cancellation)
        return IndexSearchResult(nodes: nodes, totalCount: Int(count))
    }

    public func invalidate(for root: URL) throws {
        Self.mutationLock.lock()
        defer { Self.mutationLock.unlock() }
        let url = databaseURL(for: root)
        if FileManager.default.fileExists(atPath: url.path) { try FileManager.default.removeItem(at: url) }
    }

    private func databaseURL(for root: URL) -> URL {
        let digest = SHA256.hash(data: Data(Self.path(root).utf8)).map { String(format: "%02x", $0) }.joined()
        return directory.appendingPathComponent(digest + ".sqlite")
    }

    private static func path(_ url: URL) -> String { url.standardizedFileURL.path }
    private static func fold(_ value: String) -> String {
        value.folding(options: [.caseInsensitive, .diacriticInsensitive], locale: Locale(identifier: "en_US_POSIX"))
    }
    private static func contains(_ path: String, in root: String) -> Bool {
        path == root || root == "/" || path.hasPrefix(root + "/")
    }
    private func check(_ cancellation: ScanCancellation) throws {
        if cancellation.isCancelled { throw CancellationError() }
    }
    private func subtreePredicate(_ path: String) -> (sql: String, values: [IndexValue]) {
        path == "/" ? ("1", []) : ("path=? OR (path>=? AND path<?)", [.text(path), .text(path + "/"), .text(path + "0")])
    }
    private func addAncestors(_ path: String, root: String, to result: inout Set<String>) {
        var current = path
        while Self.contains(current, in: root) {
            result.insert(current)
            if current == root { break }
            let parent = URL(fileURLWithPath: current).deletingLastPathComponent().path
            if parent == current { break }
            current = parent
        }
    }
    private func open(_ snapshot: IndexSnapshot, cancellation: ScanCancellation? = nil) throws -> IndexDatabase {
        let db = try IndexDatabase(snapshot.databaseURL, writable: false, cancellation: cancellation)
        try validate(snapshot, db: db)
        return db
    }
    private func validate(_ snapshot: IndexSnapshot, db: IndexDatabase) throws {
        let current = try readSnapshot(db, file: snapshot.databaseURL)
        guard current.id == snapshot.id, current.rootURL.path == snapshot.rootURL.path else { throw DiskIndexError.staleSnapshot }
    }
    private func readSnapshot(_ db: IndexDatabase, file: URL) throws -> IndexSnapshot {
        guard try db.integer("PRAGMA application_id") == Self.applicationID,
              try db.integer("PRAGMA user_version") == Self.version else { throw DiskIndexError.invalid("This disk index has an unsupported format.") }
        let row = try db.statement("SELECT snapshot_id,root_path,scanned_at,elapsed,skipped,issues,event_id,event_identity FROM metadata WHERE singleton=1")
        guard try row.step(), let id = row.text(0), !id.isEmpty, let path = row.text(1), path.hasPrefix("/"),
              let issuesText = row.text(5), let eventText = row.text(6), let eventID = UInt64(eventText),
              let identity = row.text(7), row.integer(4) >= 0,
              row.real(2).isFinite, row.real(3).isFinite, row.real(3) >= 0,
              let data = issuesText.data(using: .utf8), let issues = try? JSONDecoder().decode([String].self, from: data) else {
            throw DiskIndexError.invalid("This disk index is incomplete or damaged.")
        }
        return IndexSnapshot(id: id, rootURL: URL(fileURLWithPath: path, isDirectory: true), databaseURL: file,
                             scannedAt: Date(timeIntervalSince1970: row.real(2)), elapsed: row.real(3),
                             skippedCount: Int(row.integer(4)), issues: issues, eventID: eventID, eventIdentity: identity)
    }
    private func writeSnapshot(_ snapshot: IndexSnapshot, db: IndexDatabase) throws {
        let issues = String(decoding: try JSONEncoder().encode(snapshot.issues), as: UTF8.self)
        try db.run("INSERT OR REPLACE INTO metadata VALUES(1,?,?,?,?,?,?,?,?)", [
            .text(snapshot.id), .text(snapshot.rootURL.path), .real(snapshot.scannedAt.timeIntervalSince1970),
            .real(snapshot.elapsed), .integer(Int64(snapshot.skippedCount)), .text(issues),
            .text(String(snapshot.eventID)), .text(snapshot.eventIdentity)
        ])
    }
    private func insertTree(_ root: DiskNode, parent: String?, db: IndexDatabase, cancellation: ScanCancellation) throws {
        let statement = try db.statement("INSERT INTO nodes(path,parent,name,search_name,is_dir,is_link,accessible,size,allocated,file_count,raw_alloc,file_identity) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")
        let parentID = try parent.map { try db.integer("SELECT id FROM nodes WHERE path=?", [.text($0)]) }
        var pending: [(DiskNode, String?, Int64?)] = [(root, parent, parentID)]
        while let (node, parent, parentID) = pending.popLast() {
            try check(cancellation)
            let path = Self.path(node.url)
            guard node.size >= 0, node.allocatedSize >= 0, node.fileCount >= 0,
                  parent == nil || URL(fileURLWithPath: path, isDirectory: node.isDirectory).deletingLastPathComponent().path == parent else {
                throw DiskIndexError.invalid("A scanned file has invalid metadata or parentage.")
            }
            try statement.bind([.text(path), parentID.map(IndexValue.integer) ?? .null, .text(node.name), .text(Self.fold(node.name)),
                                .integer(node.isDirectory ? 1 : 0), .integer(node.isSymbolicLink ? 1 : 0),
                                .integer(node.isAccessible ? 1 : 0), .integer(node.size), .integer(node.allocatedSize),
                                .integer(Int64(node.fileCount)), .integer(node.rawAllocatedSize), node.fileIdentity.map(IndexValue.text) ?? .null])
            _ = try statement.step()
            let nodeID = sqlite3_last_insert_rowid(db.handle)
            try statement.reset()
            pending.append(contentsOf: node.children.map { ($0, path, nodeID) })
        }
    }
    private func readNode(_ path: String, db: IndexDatabase) throws -> DiskNode? {
        try readNodes("SELECT \(Self.nodeColumns) FROM nodes WHERE path=?", [.text(path)], db: db).first
    }
    private func readChildren(_ parent: String, limit: Int?, db: IndexDatabase) throws -> [DiskNode] {
        var values: [IndexValue] = [.text(parent)]
        if let limit { values.append(.integer(Int64(limit))) }
        return try readNodes("SELECT \(Self.nodeColumns) FROM nodes WHERE parent=(SELECT id FROM nodes WHERE path=?) ORDER BY size DESC,name" + (limit == nil ? "" : " LIMIT ?"), values, db: db)
    }
    private func readNodes(_ sql: String, _ values: [IndexValue], db: IndexDatabase) throws -> [DiskNode] {
        let statement = try db.statement(sql)
        try statement.bind(values)
        var nodes: [DiskNode] = []
        while try statement.step() {
            guard let path = statement.text(0), let name = statement.text(1), statement.integer(5) >= 0,
                  statement.integer(6) >= 0, statement.integer(7) >= 0, statement.integer(8) >= 0 else {
                throw DiskIndexError.invalid("A disk index entry is damaged.")
            }
            let isDirectory = statement.integer(2) != 0
            nodes.append(DiskNode(url: URL(fileURLWithPath: path, isDirectory: isDirectory), name: name, isDirectory: isDirectory,
                                  isSymbolicLink: statement.integer(3) != 0, size: statement.integer(5),
                                  allocatedSize: statement.integer(6), fileCount: Int(statement.integer(7)),
                                  isAccessible: statement.integer(4) != 0, rawAllocatedSize: statement.integer(8),
                                  fileIdentity: statement.text(9)))
        }
        return nodes
    }
    private func preview(_ node: DiskNode, depth: Int, budget: inout Int, db: IndexDatabase) throws -> DiskNode {
        guard node.isDirectory, depth < 4, budget > 0 else { return node }
        let children = try readChildren(node.id, limit: min(96, budget), db: db)
        budget -= children.count
        let hydrated = try children.map { try preview($0, depth: depth + 1, budget: &budget, db: db) }
        return replacingChildren(node, hydrated)
    }
    private func replacingChildren(_ node: DiskNode, _ children: [DiskNode]) -> DiskNode {
        DiskNode(url: node.url, name: node.name, isDirectory: node.isDirectory, isSymbolicLink: node.isSymbolicLink,
                 size: node.size, allocatedSize: node.allocatedSize, fileCount: node.fileCount, children: children,
                 isAccessible: node.isAccessible, rawAllocatedSize: node.rawAllocatedSize, fileIdentity: node.fileIdentity)
    }
    private func publish(_ temporary: URL, to destination: URL, cancellation: ScanCancellation) throws {
        try check(cancellation)
        guard rename(temporary.path, destination.path) == 0 else { throw DiskIndexError.database(String(cString: strerror(errno))) }
    }
    private func removeTemporary(_ url: URL) {
        for suffix in ["", "-journal", "-wal", "-shm"] { try? FileManager.default.removeItem(atPath: url.path + suffix) }
    }
}

private enum IndexValue {
    case text(String), integer(Int64), real(Double), null
}

private final class IndexDatabase {
    private(set) var handle: OpaquePointer?
    let cancellation: ScanCancellation?

    init(_ url: URL, writable: Bool, cancellation: ScanCancellation? = nil) throws {
        self.cancellation = cancellation
        let flags = (writable ? SQLITE_OPEN_READWRITE : SQLITE_OPEN_READONLY) | SQLITE_OPEN_NOMUTEX
        let status = sqlite3_open_v2(url.path, &handle, flags, nil)
        guard status == SQLITE_OK else {
            let message = handle.map { String(cString: sqlite3_errmsg($0)) } ?? "Could not open the disk index."
            if let handle { sqlite3_close(handle) }
            handle = nil
            throw DiskIndexError.database(message)
        }
        sqlite3_busy_timeout(handle, 2_000)
        if let cancellation {
            sqlite3_progress_handler(handle, 1_000, { pointer in
                guard let pointer else { return 0 }
                return Unmanaged<ScanCancellation>.fromOpaque(pointer).takeUnretainedValue().isCancelled ? 1 : 0
            }, Unmanaged.passUnretained(cancellation).toOpaque())
        }
    }
    deinit { if let handle { sqlite3_close_v2(handle) } }
    func error() -> Error {
        if cancellation?.isCancelled == true { return CancellationError() }
        return DiskIndexError.database(handle.map { String(cString: sqlite3_errmsg($0)) } ?? "The disk index is closed.")
    }
    func close() throws {
        guard let handle else { return }
        guard sqlite3_close(handle) == SQLITE_OK else { throw error() }
        self.handle = nil
    }
    func execute(_ sql: String) throws {
        guard sqlite3_exec(handle, sql, nil, nil, nil) == SQLITE_OK else { throw error() }
    }
    func statement(_ sql: String) throws -> IndexStatement { try IndexStatement(sql, db: self) }
    func run(_ sql: String, _ values: [IndexValue]) throws {
        let statement = try statement(sql)
        try statement.bind(values)
        _ = try statement.step()
    }
    func integer(_ sql: String, _ values: [IndexValue] = []) throws -> Int64 {
        let statement = try statement(sql)
        try statement.bind(values)
        guard try statement.step() else { throw DiskIndexError.invalid("The disk index is missing metadata.") }
        return statement.integer(0)
    }
    func strings(_ sql: String, _ values: [IndexValue]) throws -> [String] {
        let statement = try statement(sql)
        try statement.bind(values)
        var result: [String] = []
        while try statement.step() { if let value = statement.text(0) { result.append(value) } }
        return result
    }
}

private final class IndexStatement {
    let db: IndexDatabase
    var handle: OpaquePointer?
    init(_ sql: String, db: IndexDatabase) throws {
        self.db = db
        guard sqlite3_prepare_v2(db.handle, sql, -1, &handle, nil) == SQLITE_OK else { throw db.error() }
    }
    deinit { sqlite3_finalize(handle) }
    func bind(_ values: [IndexValue]) throws {
        for (offset, value) in values.enumerated() {
            let index = Int32(offset + 1)
            let status: Int32
            switch value {
            case .text(let text): status = text.withCString { sqlite3_bind_text(handle, index, $0, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self)) }
            case .integer(let number): status = sqlite3_bind_int64(handle, index, number)
            case .real(let number): status = sqlite3_bind_double(handle, index, number)
            case .null: status = sqlite3_bind_null(handle, index)
            }
            guard status == SQLITE_OK else { throw db.error() }
        }
    }
    func step() throws -> Bool {
        let status = sqlite3_step(handle)
        if status == SQLITE_ROW { return true }
        guard status == SQLITE_DONE else { throw db.error() }
        return false
    }
    func reset() throws {
        guard sqlite3_reset(handle) == SQLITE_OK, sqlite3_clear_bindings(handle) == SQLITE_OK else { throw db.error() }
    }
    func text(_ index: Int32) -> String? { sqlite3_column_text(handle, index).map { String(cString: $0) } }
    func integer(_ index: Int32) -> Int64 { sqlite3_column_int64(handle, index) }
    func real(_ index: Int32) -> Double { sqlite3_column_double(handle, index) }
}
