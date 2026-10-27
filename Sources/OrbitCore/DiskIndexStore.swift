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
    public let lockDirectory: URL
    #if ORBIT_TESTING
    package static var testDisablePreparedSearch = false
    package static var testSearchIndexByteLimit: Int?
    package static var testSearchSQLiteProgress: (@Sendable () -> Void)?
    package static var testEqualSizeSearchSkip: (@Sendable () -> Void)?
    package static var testBeforePublish: (@Sendable () -> Void)?
    package static var testBeforeRecoveryPublish: (@Sendable () -> Void)?
    package static var testDuringRefreshAncestors: (@Sendable () -> Void)?
    package static var testBeforeUpdatePublish: (@Sendable () -> Void)?
    #endif
    private static let version = InventorySchema.version
    private static let applicationID = InventorySchema.applicationID
    private static let mutationLock = NSLock()
    private static let searchIndexes = PreparedNameIndexes()
    private static let nodeColumns = InventorySchema.pathSQL() + ",n.name,n.is_dir,n.is_link,n.accessible,n.size,n.allocated,n.file_count,n.raw_alloc,n.dev,n.inode"
    private static let nodeFrom = " FROM nodes n JOIN directory_paths p ON p.id=COALESCE(n.parent,n.id)"
    private static let lookupIDSQL = "COALESCE((SELECT id FROM directory_paths WHERE path=?),(SELECT id FROM nodes WHERE parent=(SELECT id FROM directory_paths WHERE path=?) AND name=?))"

    private static func stagedFrom(indexHint: String = "") -> String {
        " FROM nodes n \(indexHint) JOIN directory_paths p ON p.id=COALESCE(n.parent,n.id) LEFT JOIN directory_paths own ON own.id=n.id LEFT JOIN directories d ON d.path=CAST(own.path AS BLOB) LEFT JOIN epochs e ON e.parent=d.path LEFT JOIN directories pd ON pd.path=CAST(p.path AS BLOB)"
    }

    private static func lookupValues(_ path: String) -> [IndexValue] {
        let url = URL(fileURLWithPath: path)
        return [.text(path), .text(url.deletingLastPathComponent().path), .text(url.lastPathComponent)]
    }

    public init(directory: URL) {
        self.directory = IndexLocations.canonicalCache(directory)
        self.lockDirectory = IndexLocations.locks(for: self.directory)
    }

    public func captureExpectedSnapshot(for root: URL, cancellation: ScanCancellation = ScanCancellation()) throws -> SnapshotExpectation {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        try FileManager.default.createDirectory(at: lockDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try IndexLocations.validateCache(directory)
        let lease = try RootMutationLease(directory: lockDirectory, root: root, cancellation: cancellation)
        defer { withExtendedLifetime(lease) {} }
        return try SnapshotExpectation.capture(directory: directory, root: root)
    }

    public func savedSnapshot(for root: URL) -> IndexSnapshot? {
        let rootPath = Self.path(root)
        let file = databaseURL(for: root)
        var presence = stat()
        guard lstat(file.path, &presence) == 0 else { return nil }
        do {
            let db = try IndexDatabase(file, writable: false)
            let snapshot = try readSnapshot(db, file: file)
            guard snapshot.rootURL.path == rootPath,
                  let node = try readNode(rootPath, db: db), node.isDirectory else { return nil }
            return snapshot
        } catch { return nil }
    }

    public func removalCursor(snapshot: IndexSnapshot, cancellation: ScanCancellation = ScanCancellation()) throws -> RemovalCursor? {
        let db = try open(snapshot, cancellation: cancellation)
        guard try db.integer("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='orbit_removal_cursor'") != 0 else { return nil }
        let row = try db.statement("SELECT identity,sequence FROM orbit_removal_cursor WHERE singleton=1")
        guard try row.step(), let identity = row.text(0), UUID(uuidString: identity) != nil, row.integer(1) >= 0 else {
            throw DiskIndexError.invalid("The map's removal checkpoint is damaged.")
        }
        return RemovalCursor(identity: identity, sequence: row.integer(1))
    }

    private func writeRemovalCursor(_ cursor: RemovalCursor, db: IndexDatabase) throws {
        try db.execute("CREATE TABLE IF NOT EXISTS orbit_removal_cursor(singleton INTEGER PRIMARY KEY CHECK(singleton=1),identity TEXT NOT NULL,sequence INTEGER NOT NULL)")
        try db.run("INSERT OR REPLACE INTO orbit_removal_cursor VALUES(1,?,?)", [.text(cursor.identity), .integer(cursor.sequence)])
    }

    package func recoveryWorkingCopy(of snapshot: IndexSnapshot, cancellation: ScanCancellation) throws -> IndexSnapshot {
        guard snapshot.databaseURL == databaseURL(for: snapshot.rootURL) else { throw DiskIndexError.staleSnapshot }
        try IndexLocations.validateCache(directory)
        let lease = try RootMutationLease(directory: lockDirectory, root: snapshot.rootURL, cancellation: cancellation)
        defer { withExtendedLifetime(lease) {} }
        Self.mutationLock.lock(); defer { Self.mutationLock.unlock() }
        let original = try open(snapshot, cancellation: cancellation)
        try original.close()
        let temporary = directory.appendingPathComponent(".\(UUID().uuidString).sqlite")
        if clonefile(snapshot.databaseURL.path, temporary.path, 0) != 0 {
            var info = stat()
            guard lstat(temporary.path, &info) != 0, errno == ENOENT else {
                throw DiskIndexError.database("The recovery copy destination already exists.")
            }
            try FileManager.default.copyItem(at: snapshot.databaseURL, to: temporary)
        }
        do {
            let copy = relocated(snapshot, to: temporary)
            let db = try open(copy, cancellation: cancellation); try db.close()
            try lease.validate()
            return copy
        } catch { removeTemporary(temporary); throw error }
    }

    package func publishRecoveryCopy(_ copy: IndexSnapshot, replacing original: IndexSnapshot,
                                    cursor: RemovalCursor, cancellation: ScanCancellation) throws -> IndexSnapshot {
        guard original.databaseURL == databaseURL(for: original.rootURL), copy.rootURL == original.rootURL,
              copy.databaseURL.deletingLastPathComponent() == directory,
              copy.databaseURL.lastPathComponent.hasPrefix("."), copy.databaseURL.pathExtension == "sqlite" else {
            throw DiskIndexError.invalid("The recovery copy is invalid.")
        }
        let lease = try RootMutationLease(directory: lockDirectory, root: original.rootURL, cancellation: cancellation)
        defer { withExtendedLifetime(lease) {} }
        Self.mutationLock.lock(); defer { Self.mutationLock.unlock() }
        let actual = try open(original, cancellation: cancellation); try actual.close()
        let prepared = try open(copy, cancellation: cancellation); try prepared.close()
        try RemovalJournal(cache: directory).withFence(cancellation: cancellation) { fence in
            guard fence.identity == cursor.identity && fence.highWater == cursor.sequence else {
                throw RemovalRecoveryError.changedDuringReconciliation
            }
            try lease.validate(); try fence.validate()
            #if ORBIT_TESTING
            Self.testBeforeRecoveryPublish?()
            #endif
            try publish(copy.databaseURL, to: original.databaseURL, cancellation: cancellation)
        }
        return relocated(copy, to: original.databaseURL)
    }

    package func discardRecoveryCopy(_ copy: IndexSnapshot) { removeTemporary(copy.databaseURL) }

    private func relocated(_ snapshot: IndexSnapshot, to url: URL) -> IndexSnapshot {
        IndexSnapshot(id: snapshot.id, rootURL: snapshot.rootURL, databaseURL: url, scannedAt: snapshot.scannedAt,
                      elapsed: snapshot.elapsed, skippedCount: snapshot.skippedCount, issues: snapshot.issues,
                      eventID: snapshot.eventID, eventIdentity: snapshot.eventIdentity)
    }

    public func save(_ result: ScanResult, eventID: UInt64, eventIdentity: String,
                     cancellation: ScanCancellation = ScanCancellation(), expectedSnapshot: SnapshotExpectation? = nil) throws -> IndexSnapshot {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        try FileManager.default.createDirectory(at: lockDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try IndexLocations.validateCache(directory)
        let rootLease = try RootMutationLease(directory: lockDirectory, root: result.root.url, cancellation: cancellation)
        defer { withExtendedLifetime(rootLease) {} }
        Self.mutationLock.lock()
        defer { Self.mutationLock.unlock() }
        try check(cancellation)
        if let expectedSnapshot { try expectedSnapshot.verify(directory: directory, root: result.root.url) }
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
        try db.execute(InventorySchema.createSQL)
        try insertTree(result.root, parent: nil, db: db, cancellation: cancellation)
        try writeSnapshot(snapshot, db: db)
        try db.execute("COMMIT;")
        try db.close()
        #if ORBIT_TESTING
        Self.testBeforePublish?()
        #endif
        try rootLease.validate()
        if let expectedSnapshot { try expectedSnapshot.verify(directory: directory, root: result.root.url) }
        try publish(temporary, to: destination, cancellation: cancellation)
        return snapshot
    }

    public func update(snapshot: IndexSnapshot, replacements: [IndexReplacement], eventID: UInt64,
                       eventIdentity: String, cancellation: ScanCancellation = ScanCancellation(),
                       removalCursor: RemovalCursor? = nil,
                       journalFenceCursor: RemovalCursor? = nil) throws -> IndexSnapshot {
        try FileManager.default.createDirectory(at: lockDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try IndexLocations.validateCache(directory)
        let rootLease = try RootMutationLease(directory: lockDirectory, root: snapshot.rootURL, cancellation: cancellation)
        defer { withExtendedLifetime(rootLease) {} }
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
        try Self.validateReplacementPaths(paths, root: root, cancellation: cancellation)
        let temporary = directory.appendingPathComponent(".\(UUID().uuidString).sqlite")
        defer { removeTemporary(temporary) }
        if clonefile(snapshot.databaseURL.path, temporary.path, 0) != 0 {
            try? FileManager.default.removeItem(at: temporary)
            try FileManager.default.copyItem(at: snapshot.databaseURL, to: temporary)
        }
        let db = try IndexDatabase(temporary, writable: true, cancellation: cancellation)
        try validate(snapshot, db: db)
        try db.registerHardLinkOrder()
        try db.execute("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; BEGIN IMMEDIATE;")
        try db.execute("CREATE TEMP TABLE refresh_identities (dev INTEGER, inode INTEGER, PRIMARY KEY(dev,inode)) WITHOUT ROWID; CREATE TEMP TABLE refresh_nodes(id INTEGER PRIMARY KEY);")
        var affectedParents = Set<String>()
        var issues = snapshot.issues
        var skipped = snapshot.skippedCount
        for (replacement, path) in zip(replacements, paths) {
            try check(cancellation)
            guard path != root || replacement.result != nil else {
                throw DiskIndexError.invalid("The indexed folder no longer exists.")
            }
            let predicate = subtreePredicate(path)
            // Keep the deletion set stable while directory-path delete triggers run.
            try db.execute("DELETE FROM refresh_nodes")
            try db.run("INSERT INTO refresh_nodes SELECT id FROM nodes WHERE \(predicate.sql)", predicate.values)
            try db.execute("INSERT OR IGNORE INTO refresh_identities SELECT dev,inode FROM nodes WHERE is_dir=0 AND dev IS NOT NULL AND inode IS NOT NULL AND id IN (SELECT id FROM refresh_nodes)")
            try db.execute("DELETE FROM nodes WHERE id IN (SELECT id FROM refresh_nodes)")
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
                try db.run("INSERT OR IGNORE INTO refresh_identities SELECT dev,inode FROM nodes WHERE is_dir=0 AND dev IS NOT NULL AND inode IS NOT NULL AND (\(predicate.sql))", predicate.values)
                skipped += result.skippedCount
                for issue in result.issues where !issues.contains(issue) && issues.count < 100 { issues.append(issue) }
                if path == root { skipped = result.skippedCount; issues = result.issues }
            }
        }
        try db.execute("""
            CREATE TEMP TABLE refresh_owners (dev INTEGER, inode INTEGER, path TEXT NOT NULL, PRIMARY KEY(dev,inode)) WITHOUT ROWID;
            INSERT INTO refresh_owners
              SELECT i.dev,i.inode,MIN(\(InventorySchema.pathSQL()) COLLATE ORBIT_HARDLINK_ORDER) FROM refresh_identities i
              CROSS JOIN nodes n ON n.dev=i.dev AND n.inode=i.inode AND n.is_dir=0
              JOIN directory_paths p ON p.id=COALESCE(n.parent,n.id) GROUP BY i.dev,i.inode;
            CREATE TEMP TABLE refresh_allocations (node_id INTEGER PRIMARY KEY, allocated INTEGER NOT NULL);
            INSERT INTO refresh_allocations
              SELECT n.id,CASE WHEN \(InventorySchema.pathSQL())=o.path THEN n.raw_alloc ELSE 0 END
              FROM refresh_owners o CROSS JOIN nodes n ON n.dev=o.dev AND n.inode=o.inode AND n.is_dir=0
              JOIN directory_paths p ON p.id=COALESCE(n.parent,n.id)
              WHERE n.allocated != CASE WHEN \(InventorySchema.pathSQL())=o.path THEN n.raw_alloc ELSE 0 END;
            """)
        do {
            let changedPaths = try db.statement("SELECT \(InventorySchema.pathSQL()) FROM refresh_allocations a JOIN nodes n ON n.id=a.node_id JOIN directory_paths p ON p.id=COALESCE(n.parent,n.id)")
            while try changedPaths.step() {
                #if ORBIT_TESTING
                Self.testDuringRefreshAncestors?()
                #endif
                try check(cancellation)
                guard let path = changedPaths.text(0) else { throw DiskIndexError.invalid("Invalid hard-link path.") }
                addAncestors(URL(fileURLWithPath: path).deletingLastPathComponent().path, root: root, to: &affectedParents)
            }
        }
        try db.execute("UPDATE nodes SET allocated=(SELECT allocated FROM refresh_allocations WHERE node_id=nodes.id) WHERE id IN (SELECT node_id FROM refresh_allocations);")
        for parent in affectedParents.sorted(by: { $0.count > $1.count }) {
            try check(cancellation)
            try db.run("""
                UPDATE nodes SET (size,allocated,file_count)=(SELECT COALESCE(SUM(size),0),
                  COALESCE(SUM(allocated),0),COALESCE(SUM(file_count),0) FROM nodes WHERE parent=(SELECT id FROM directory_paths WHERE path=?))
                WHERE id=(SELECT id FROM directory_paths WHERE path=?) AND is_dir=1
                """, Array(repeating: .text(parent), count: 2))
        }
        let next = IndexSnapshot(id: UUID().uuidString, rootURL: snapshot.rootURL, databaseURL: snapshot.databaseURL,
                                 scannedAt: Date(), elapsed: replacements.reduce(0) { $0 + ($1.result?.elapsed ?? 0) },
                                 skippedCount: skipped, issues: issues, eventID: eventID, eventIdentity: eventIdentity)
        try writeSnapshot(next, db: db)
        if let removalCursor { try writeRemovalCursor(removalCursor, db: db) }
        try db.execute("COMMIT;")
        try db.close()
        #if ORBIT_TESTING
        Self.testBeforeUpdatePublish?()
        #endif
        try rootLease.validate()
        if let fenceCursor = journalFenceCursor ?? removalCursor {
            try RemovalJournal(cache: directory).withFence(cancellation: cancellation) { fence in
                guard fence.identity == fenceCursor.identity && fence.highWater == fenceCursor.sequence else {
                    throw RemovalRecoveryError.changedDuringReconciliation
                }
                try fence.validate()
                try publish(temporary, to: snapshot.databaseURL, cancellation: cancellation)
            }
        } else {
            try publish(temporary, to: snapshot.databaseURL, cancellation: cancellation)
        }
        return next
    }

    public func removalNodes(_ removals: [ScanRemoval], snapshot: IndexSnapshot,
                             cancellation: ScanCancellation? = nil) throws -> [DiskNode] {
        let db = try open(snapshot, cancellation: cancellation)
        var nodes: [DiskNode] = []
        for removal in removals {
            try check(cancellation)
            if let node = try readNode(removal.path, db: db), node.fileIdentity == removal.identity { nodes.append(node) }
        }
        return nodes
    }

    public func stagedConfirmsMissingDirectory(_ path: String, snapshot: IndexSnapshot) throws -> Bool {
        let db = try open(snapshot)
        try db.execute("BEGIN")
        defer { try? db.execute("ROLLBACK") }
        let query = try db.statement("SELECT n.is_dir,COALESCE(d.pending,0) FROM nodes n LEFT JOIN directory_paths own ON own.id=n.id LEFT JOIN directories d ON d.path=CAST(own.path AS BLOB) WHERE n.id=\(Self.lookupIDSQL)")
        var current = Self.path(URL(fileURLWithPath: path))
        var first = true
        while true {
            try query.bind(Self.lookupValues(current))
            if try query.step() {
                let directory = query.integer(0) != 0, pending = query.integer(1) != 0
                return first ? !directory : (!directory || !pending)
            }
            try query.reset()
            if current == snapshot.rootURL.path || current == "/" { return false }
            let parent = URL(fileURLWithPath: current).deletingLastPathComponent().path
            if parent == current { return false }
            current = parent; first = false
        }
    }

    public func stagedSearch(query: String, snapshot: IndexSnapshot, revision: UInt64,
                             activeDirectory: (path: Data, identity: String)? = nil, removals: [ScanRemoval], cancellation: ScanCancellation, limit: Int = 300) throws -> (nodes: [DiskNode], count: Int, revision: UInt64, removalsChanged: Bool) {
        try Task.checkCancellation()
        let needle = Self.fold(query.trimmingCharacters(in: .whitespacesAndNewlines))
        guard !needle.isEmpty else { return ([], 0, revision, false) }
        // Pin the prefix before establishing the SQLite snapshot; a newer prefix could hide older rows.
        let prefixKey = PreparedNameIndexes.Key(snapshot, staged: true)
        let prefix = prefixKey.flatMap { Self.searchIndexes.lookup($0) }
        let candidates = try prefix?.index.matches(needle, stopAfterCandidateLimit: true) { try Task.checkCancellation(); try check(cancellation) }
        let db = try open(snapshot, cancellation: cancellation)
        db.checkTaskCancellation()
        try db.execute("BEGIN")
        defer { try? db.execute("ROLLBACK") }
        let sources = try stagedRemovalSources(removals, db: db, cancellation: cancellation)
        let projection = try DiskRemovalProjection(removals: removals, source: sources, cancellation: cancellation)
        var namePredicate = "instr(n.search_name,?)>0"
        var parameters: [IndexValue] = [.text(needle)]
        if let prefix, let ids = candidates?.ids, prefixKey == PreparedNameIndexes.Key(snapshot, staged: true),
           prefix.generation == (try db.integer("SELECT value FROM orbit_search_generation WHERE singleton=1")) {
            let narrowed = ids.isEmpty ? "n.id>?" : "(n.id IN (" + Array(repeating: "?", count: ids.count).joined(separator: ",") + ") OR n.id>?)"
            namePredicate = narrowed + " AND " + namePredicate
            parameters = ids.map(IndexValue.integer) + [.integer(prefix.index.highWaterID), .text(needle)]
        }
        let resultLimit = max(0, min(limit, 10_000))
        var verifiedCount: Int?
        if removals.isEmpty, prefix != nil, candidates?.ids == nil {
            func edge(_ order: String) throws -> Int64? {
                let row = try db.statement("SELECT n.size FROM (SELECT size,search_name,parent FROM nodes INDEXED BY nodes_by_size ORDER BY size \(order) LIMIT 4096) n WHERE n.parent IS NOT NULL AND \(namePredicate) LIMIT 1")
                try row.bind(parameters)
                return try row.step() ? row.integer(0) : nil
            }
            // All boundary ties still need normalized path ordering; avoid an extra full pass when every size ties.
            let high = resultLimit == 0 ? nil : try edge("DESC")
            let low = resultLimit == 0 ? nil : try edge("ASC")
            if resultLimit == 0 || (high != nil && low != nil && high != low) {
                let totals = try db.statement("SELECT COUNT(*),MIN(CAST(n.size AS INTEGER)),MIN(CAST(n.allocated AS INTEGER)),MIN(CAST(n.file_count AS INTEGER)),MIN(CAST(n.raw_alloc AS INTEGER)),MIN(typeof(n.size)='integer') FROM nodes n WHERE n.parent IS NOT NULL AND \(namePredicate)")
                try totals.bind(parameters)
                guard try totals.step(), (1...4).allSatisfy({ totals.integer(Int32($0)) >= 0 }) else {
                    throw DiskIndexError.invalid("A staged disk index entry is damaged.")
                }
                let count = Int(totals.integer(0))
                try Task.checkCancellation(); try check(cancellation)
                if resultLimit == 0 { return ([], count, revision, false) }
                if totals.integer(5) == 1 { verifiedCount = count }
            }
        }
        let sizeOrdered = verifiedCount != nil
        let indexHint = sizeOrdered ? "INDEXED BY nodes_by_size" : ""
        let order = sizeOrdered ? "ORDER BY n.size DESC" : ""
        let statement = try db.statement("""
            SELECT \(Self.nodeColumns),COALESCE(d.pending,0),CASE WHEN e.value IS NULL THEN 0 ELSE 1 END,COALESCE(pd.pending,0)
            \(Self.stagedFrom(indexHint: indexHint))
            WHERE n.parent IS NOT NULL AND \(namePredicate) \(order)
            """)
        try statement.bind(parameters)
        var result: [DiskNode] = [], count = verifiedCount ?? 0, scanned = 0
        var worstID = ""
        func worse(_ a: DiskNode, _ b: DiskNode) -> Bool {
            a.size == b.size ? b.id.utf8.lexicographicallyPrecedes(a.id.utf8) : a.size < b.size
        }
        func retain(_ node: DiskNode) {
            guard resultLimit > 0 else { return }
            if result.count < resultLimit {
                result.append(node); var child = result.count - 1
                while child > 0 { let parent = (child - 1) / 2; if !worse(result[child], result[parent]) { break }; result.swapAt(child, parent); child = parent }
            } else if worse(result[0], node) {
                result[0] = node; var parent = 0
                while true { let left = parent * 2 + 1; if left >= result.count { break }; let right = left + 1; let child = right < result.count && worse(result[right], result[left]) ? right : left; if !worse(result[child], result[parent]) { break }; result.swapAt(child, parent); parent = child }
            }
            if removals.isEmpty && result.count == resultLimit { worstID = result[0].id }
        }
        while try statement.step() {
            if scanned & 255 == 0 { try Task.checkCancellation(); try check(cancellation) }; scanned += 1
            guard let path = statement.text(0), let name = statement.text(1), statement.integer(5) >= 0,
                  statement.integer(6) >= 0, statement.integer(7) >= 0, statement.integer(8) >= 0 else {
                throw DiskIndexError.invalid("A staged disk index entry is damaged.")
            }
            if removals.isEmpty {
                if verifiedCount == nil { count += 1 }
                if resultLimit == 0 { continue }
                if result.count == resultLimit && statement.integer(5) < result[0].size {
                    if sizeOrdered { break }
                    continue
                }
            }
            let directory = statement.integer(2) != 0
            let url = URL(fileURLWithPath: path, isDirectory: directory)
            if removals.isEmpty, result.count == resultLimit, resultLimit > 0,
               statement.integer(5) == result[0].size,
               !url.path.utf8.lexicographicallyPrecedes(worstID.utf8) {
                #if ORBIT_TESTING
                Self.testEqualSizeSearchSkip?()
                #endif
                continue
            }
            let initialState: NodeScanState = !directory ? (statement.integer(13) == 0 ? .complete : .pending)
                : (statement.integer(11) == 0 ? .complete : (statement.integer(12) == 0 ? .pending : .scanning))
            let identity = statement.identity()
            var state = initialState
            if let activeDirectory, initialState == .pending, directory, activeDirectory.identity == identity, path.utf8.elementsEqual(activeDirectory.path) { state = .scanning }
            let raw = DiskNode(url: url, name: name,
                isDirectory: directory, isSymbolicLink: statement.integer(3) != 0, size: statement.integer(5),
                allocatedSize: statement.integer(6), fileCount: Int(statement.integer(7)), isAccessible: statement.integer(4) != 0,
                rawAllocatedSize: statement.integer(8), fileIdentity: identity, scanState: state)
            if removals.isEmpty { retain(raw) }
            else if let node = try projection.apply(raw, cancellation: cancellation) { count += 1; retain(node) }
        }
        result.sort { worse($1, $0) }
        try Task.checkCancellation()
        try check(cancellation)
        return (result, count, revision, false)
    }

    // All rows in the returned partial view come from one read transaction.
    public func stagedView(path: String?, snapshot: IndexSnapshot, revision: UInt64,
                           activeDirectory: (path: Data, identity: String)? = nil, removals: [ScanRemoval] = [], cancellation: ScanCancellation? = nil) throws -> ScanPreviewView? {
        try check(cancellation)
        let db = try open(snapshot, cancellation: cancellation)
        try db.execute("BEGIN")
        defer { try? db.execute("ROLLBACK") }
        let columns = Self.nodeColumns + ",COALESCE(d.pending,0),CASE WHEN e.value IS NULL THEN 0 ELSE 1 END,COALESCE(pd.pending,0)"
        let from = Self.stagedFrom()
        func rows(_ query: IndexStatement) throws -> [DiskNode] {
            var nodes: [DiskNode] = []
            while try query.step() {
                if nodes.count & 255 == 0 { try check(cancellation) }
                guard let path = query.text(0), let name = query.text(1), query.integer(5) >= 0,
                      query.integer(6) >= 0, query.integer(7) >= 0, query.integer(8) >= 0 else {
                    throw DiskIndexError.invalid("A staged disk index entry is damaged.")
                }
                let isDirectory = query.integer(2) != 0
                let initialState: NodeScanState = !isDirectory ? (query.integer(13) == 0 ? .complete : .pending)
                    : (query.integer(11) == 0 ? .complete : (query.integer(12) == 0 ? .pending : .scanning))
                let identity = query.identity()
                var state = initialState
                if let activeDirectory, initialState == .pending, isDirectory, activeDirectory.identity == identity, path.utf8.elementsEqual(activeDirectory.path) { state = .scanning }
                nodes.append(DiskNode(url: URL(fileURLWithPath: path, isDirectory: isDirectory), name: name,
                    isDirectory: isDirectory, isSymbolicLink: query.integer(3) != 0, size: query.integer(5),
                    allocatedSize: query.integer(6), fileCount: Int(query.integer(7)), isAccessible: query.integer(4) != 0,
                    rawAllocatedSize: query.integer(8), fileIdentity: identity, scanState: state))
            }
            return nodes
        }
        let one = try db.statement("SELECT \(columns)\(from) WHERE n.id=\(Self.lookupIDSQL)")
        func node(_ path: String) throws -> DiskNode? {
            try one.bind(Self.lookupValues(path))
            let found = try rows(one).first
            try one.reset()
            return found
        }
        let rootPath = snapshot.rootURL.path
        let target = path.map { Self.path(URL(fileURLWithPath: $0)) } ?? rootPath
        let rootBytes = Array(rootPath.utf8), targetBytes = Array(target.utf8)
        guard targetBytes == rootBytes || (rootBytes == [47] && targetBytes.first == 47)
                || (targetBytes.count > rootBytes.count && targetBytes.starts(with: rootBytes) && targetBytes[rootBytes.count] == 47),
              let root = try node(rootPath), let rawFolder = try node(target), rawFolder.isDirectory else { return nil }
        let childrenQuery = try db.statement("SELECT \(columns)\(from) WHERE n.parent=(SELECT id FROM directory_paths WHERE path=?) ORDER BY n.size DESC,n.name LIMIT ?")
        func children(_ parent: String, limit: Int) throws -> [DiskNode] {
            try childrenQuery.bind([.text(parent), .integer(Int64(limit))])
            let found = try rows(childrenQuery)
            try childrenQuery.reset()
            return found
        }
        var budget = 2_000
        func hydrate(_ entry: DiskNode, depth: Int) throws -> DiskNode {
            guard entry.isDirectory, depth < 4, budget > 0 else { return entry }
            let childNodes = try children(entry.id, limit: min(96, budget))
            budget -= childNodes.count
            let hydrated = try childNodes.map { try hydrate($0, depth: depth + 1) }
            return DiskNode(url: entry.url, name: entry.name, isDirectory: true, isSymbolicLink: entry.isSymbolicLink,
                size: entry.size, allocatedSize: entry.allocatedSize, fileCount: entry.fileCount, children: hydrated,
                isAccessible: entry.isAccessible, rawAllocatedSize: entry.rawAllocatedSize,
                fileIdentity: entry.fileIdentity, scanState: entry.scanState)
        }
        let direct = try children(target, limit: Int.max)
        let hydrated = try direct.map { try hydrate($0, depth: 1) }
        let folder = DiskNode(url: rawFolder.url, name: rawFolder.name, isDirectory: true,
            isSymbolicLink: rawFolder.isSymbolicLink, size: rawFolder.size, allocatedSize: rawFolder.allocatedSize,
            fileCount: rawFolder.fileCount, children: hydrated, isAccessible: rawFolder.isAccessible,
            rawAllocatedSize: rawFolder.rawAllocatedSize, fileIdentity: rawFolder.fileIdentity, scanState: rawFolder.scanState)
        var ancestors = [folder], current = target
        while Array(current.utf8) != rootBytes {
            let parent = URL(fileURLWithPath: current).deletingLastPathComponent().path
            guard Array(parent.utf8) != Array(current.utf8), let item = try node(parent) else { return nil }
            ancestors.append(item); current = parent
        }
        let token = cancellation ?? ScanCancellation()
        let source = try stagedRemovalSources(removals, db: db, cancellation: token)
        let projection = try DiskRemovalProjection(removals: removals, source: source, cancellation: token)
        guard let projectedFolder = try projection.apply(folder, cancellation: token), let projectedRoot = try projection.apply(root, cancellation: token) else { return nil }
        let projectedAncestors = try ancestors.reversed().compactMap { try projection.apply($0, cancellation: token) }
        guard projectedAncestors.count == ancestors.count else { return nil }
        try check(cancellation)
        return ScanPreviewView(root: targetBytes == rootBytes ? projectedFolder : projectedRoot, folder: projectedFolder, ancestors: projectedAncestors,
                               revision: revision, isListing: rawFolder.scanState != .complete)
    }

    private func stagedRemovalSources(_ removals: [ScanRemoval], db: IndexDatabase,
                                      cancellation: ScanCancellation) throws -> [DiskNode] {
        guard !removals.isEmpty else { return [] }
        let query = try db.statement("SELECT \(Self.nodeColumns)\(Self.nodeFrom) WHERE n.id=\(Self.lookupIDSQL)")
        var nodes: [DiskNode] = []
        var seen = Set<Data>()
        for removal in removals {
            try check(cancellation)
            try query.bind(Self.lookupValues(removal.path))
            let node = try readNodes(query, cancellation: cancellation).first
            try query.reset()
            if let node, node.fileIdentity == removal.identity,
               seen.insert(Data(removal.path.utf8)).inserted { nodes.append(node) }
        }
        return nodes
    }

    public func loadFolder(path: String, snapshot: IndexSnapshot, cancellation: ScanCancellation? = nil) throws -> DiskNode {
        try check(cancellation)
        let db = try open(snapshot, cancellation: cancellation)
        let path = Self.path(URL(fileURLWithPath: path))
        let candidate = try readNode(path, db: db)
        try check(cancellation)
        guard let folder = candidate, folder.isDirectory else {
            throw DiskIndexError.invalid("This folder is no longer in the index.")
        }
        var budget = 2_000
        let previewQuery = try db.statement("SELECT \(Self.nodeColumns)\(Self.nodeFrom) WHERE n.parent=(SELECT id FROM directory_paths WHERE path=?) ORDER BY n.size DESC,n.name LIMIT ?")
        let directChildren = try readChildren(path, limit: nil, db: db)
        var children: [DiskNode] = []
        children.reserveCapacity(directChildren.count)
        for (index, child) in directChildren.enumerated() {
            if index & 255 == 0 { try check(cancellation) }
            children.append(try preview(child, depth: 1, budget: &budget, query: previewQuery))
        }
        try check(cancellation)
        return replacingChildren(folder, children)
    }

    public func node(path: String, snapshot: IndexSnapshot, cancellation: ScanCancellation? = nil) throws -> DiskNode? {
        try check(cancellation)
        let node = try readNode(Self.path(URL(fileURLWithPath: path)), db: open(snapshot, cancellation: cancellation))
        try check(cancellation)
        return node
    }

    public func ancestors(path: String, snapshot: IndexSnapshot, cancellation: ScanCancellation? = nil) throws -> [DiskNode] {
        try check(cancellation)
        let db = try open(snapshot, cancellation: cancellation)
        try check(cancellation)
        var path = Self.path(URL(fileURLWithPath: path))
        guard Self.contains(path, in: snapshot.rootURL.path) else { return [] }
        var result: [DiskNode] = []
        while let node = try readNode(path, db: db) {
            try check(cancellation)
            result.append(node)
            if path == snapshot.rootURL.path { break }
            let parent = URL(fileURLWithPath: path).deletingLastPathComponent().path
            if parent == path { break }
            path = parent
        }
        try check(cancellation)
        return result.reversed()
    }

    /// Prepare a disposable search accelerator from a completed saved map, without crawling files.
    @discardableResult
    public func prepareSearch(snapshot: IndexSnapshot, cancellation: ScanCancellation = ScanCancellation()) throws -> Bool {
        try Task.checkCancellation(); try check(cancellation)
        guard let key = PreparedNameIndexes.Key(snapshot) else { throw DiskIndexError.staleSnapshot }
        let db = try open(snapshot, cancellation: cancellation)
        let completedMap = try { () throws -> Bool in
            let mode = try db.statement("PRAGMA journal_mode")
            return try mode.step() && mode.text(0) == "delete"
        }()
        guard completedMap else { return false }
        if Self.searchIndexes.lookup(key) != nil { return true }
        try Self.searchIndexes.begin(key) { try Task.checkCancellation(); try check(cancellation) }
        defer { Self.searchIndexes.end(key) }
        if Self.searchIndexes.lookup(key) != nil { return true }
        try db.execute("BEGIN")
        defer { try? db.execute("ROLLBACK") }
        try validate(snapshot, db: db)
        #if ORBIT_TESTING
        let builder = CompactNameIndex.Builder(byteLimit: Self.testSearchIndexByteLimit ?? 96 * 1024 * 1024)
        #else
        let builder = CompactNameIndex.Builder()
        #endif
        let rows = try db.statement("SELECT id,search_name FROM nodes WHERE parent IS NOT NULL ORDER BY id")
        var count = 0, complete = true
        while try rows.step() {
            if count & 511 == 0 { try Task.checkCancellation(); try check(cancellation) }; count += 1
            guard let bytes = sqlite3_column_text(rows.handle, 1) else { throw DiskIndexError.invalid("A search name is missing.") }
            let name = UnsafeBufferPointer(start: bytes, count: Int(sqlite3_column_bytes(rows.handle, 1)))
            if !builder.append(id: rows.integer(0), name: name) { complete = false; break }
        }
        try Task.checkCancellation(); try check(cancellation)
        guard PreparedNameIndexes.Key(snapshot) == key else { throw DiskIndexError.staleSnapshot }
        let index = builder.finish()
        guard index.recordCount > 0 || complete else { return false }
        Self.searchIndexes.insert(index, for: key, complete: complete)
        return true
    }

    /// Index an immutable name prefix while newer rows continue to arrive in the WAL.
    @discardableResult
    public func prepareStagedSearch(snapshot: IndexSnapshot, cancellation: ScanCancellation = ScanCancellation()) throws -> Bool {
        try Task.checkCancellation(); try check(cancellation)
        guard let key = PreparedNameIndexes.Key(snapshot, staged: true) else { throw DiskIndexError.staleSnapshot }
        let db = try open(snapshot, cancellation: cancellation)
        let monotonicIDs = try { () throws -> Bool in
            let schema = try db.statement("SELECT sql FROM sqlite_master WHERE type='table' AND name='nodes'")
            return try schema.step() && schema.text(0)?.contains("AUTOINCREMENT") == true
        }()
        guard monotonicIDs,
              try db.integer("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='orbit_search_generation'") == 1 else { return false }
        try Self.searchIndexes.begin(key) { try Task.checkCancellation(); try check(cancellation) }
        defer { Self.searchIndexes.end(key) }
        try db.execute("BEGIN")
        defer { try? db.execute("ROLLBACK") }
        try validate(snapshot, db: db)
        let generation = try db.integer("SELECT value FROM orbit_search_generation WHERE singleton=1")
        let highWater = try db.integer("SELECT COALESCE(MAX(id),0) FROM nodes")
        if let existing = Self.searchIndexes.lookup(key), existing.generation == generation,
           highWater <= existing.index.highWaterID || !existing.complete { return true }
        #if ORBIT_TESTING
        let builder = CompactNameIndex.Builder(byteLimit: Self.testSearchIndexByteLimit ?? 96 * 1024 * 1024)
        #else
        let builder = CompactNameIndex.Builder()
        #endif
        let rows = try db.statement("SELECT id,search_name FROM nodes WHERE parent IS NOT NULL ORDER BY id")
        var count = 0, complete = true
        while try rows.step() {
            if count & 511 == 0 { try Task.checkCancellation(); try check(cancellation) }; count += 1
            guard let bytes = sqlite3_column_text(rows.handle, 1) else { throw DiskIndexError.invalid("A search name is missing.") }
            let name = UnsafeBufferPointer(start: bytes, count: Int(sqlite3_column_bytes(rows.handle, 1)))
            if !builder.append(id: rows.integer(0), name: name) { complete = false; break }
        }
        try Task.checkCancellation(); try check(cancellation)
        guard PreparedNameIndexes.Key(snapshot, staged: true) == key else { throw DiskIndexError.staleSnapshot }
        let index = builder.finish()
        guard index.recordCount > 0 else { return false }
        Self.searchIndexes.insert(index, for: key, generation: generation, complete: complete)
        return true
    }

    public func isStagedSearchPrepared(snapshot: IndexSnapshot) -> Bool {
        guard let key = PreparedNameIndexes.Key(snapshot, staged: true) else { return false }
        return Self.searchIndexes.lookup(key) != nil
    }

    #if ORBIT_TESTING
    package func testSearchIndex(snapshot: IndexSnapshot, staged: Bool = false) -> (count: Int, highWater: Int64, complete: Bool, bytes: Int)? {
        guard let key = PreparedNameIndexes.Key(snapshot, staged: staged), let entry = Self.searchIndexes.lookup(key) else { return nil }
        return (entry.index.recordCount, entry.index.highWaterID, entry.complete, entry.index.byteCount)
    }
    #endif

    public func isSearchPrepared(snapshot: IndexSnapshot) -> Bool {
        guard let key = PreparedNameIndexes.Key(snapshot) else { return false }
        return Self.searchIndexes.lookup(key) != nil
    }

    public func search(query: String, snapshot: IndexSnapshot, limit: Int = 300,
                       cancellation: ScanCancellation = ScanCancellation(), stablePathOrder: Bool = false,
                       removals: [ScanRemoval] = []) throws -> IndexSearchResult {
        try check(cancellation)
        let needle = Self.fold(query.trimmingCharacters(in: .whitespacesAndNewlines))
        guard !needle.isEmpty else { return IndexSearchResult(nodes: [], totalCount: 0) }
        let key = PreparedNameIndexes.Key(snapshot)
        let db = try open(snapshot, cancellation: cancellation)
        if !removals.isEmpty {
            return try projectedSearch(needle: needle, limit: limit, stablePathOrder: stablePathOrder,
                                       removals: removals, db: db, cancellation: cancellation)
        }
        try db.execute("BEGIN")
        defer { try? db.execute("ROLLBACK") }
        try validate(snapshot, db: db)
        db.checkTaskCancellation()
        let values: [IndexValue] = [.text(needle)]
        let prepared = key == PreparedNameIndexes.Key(snapshot) ? key.flatMap { Self.searchIndexes.lookup($0) } : nil
        let matches = try prepared?.index.matches(needle) { try Task.checkCancellation(); try check(cancellation) }
        let count: Int64
        if let matches, let prepared {
            let tail = try prepared.complete ? 0 : db.integer("SELECT COUNT(*) FROM nodes WHERE id>? AND parent IS NOT NULL AND instr(search_name,?)>0",
                [.integer(prepared.index.highWaterID), .text(needle)])
            count = Int64(matches.count) + tail
        } else {
            count = try db.integer("SELECT COUNT(*) FROM nodes WHERE parent IS NOT NULL AND instr(search_name,?)>0", values)
        }
        try Task.checkCancellation(); try check(cancellation)
        let resultLimit = max(0, min(limit, 10_000))
        guard count > 0, resultLimit > 0 else {
            return IndexSearchResult(nodes: [], totalCount: Int(count))
        }
        let order = stablePathOrder ? "n.size DESC,\(InventorySchema.pathSQL()) COLLATE BINARY" : "n.size DESC,n.id"
        let predicate: String
        let parameters: [IndexValue]
        if let ids = matches?.ids, let prepared {
            let indexed = ids.isEmpty ? "0" : "n.id IN (" + Array(repeating: "?", count: ids.count).joined(separator: ",") + ")"
            if prepared.complete {
                predicate = indexed
                parameters = ids.map(IndexValue.integer)
            } else {
                let narrowed = ids.isEmpty ? "n.id>?" : "(\(indexed) OR n.id>?)"
                predicate = narrowed + " AND n.parent IS NOT NULL AND instr(n.search_name,?)>0"
                parameters = ids.map(IndexValue.integer) + [.integer(prepared.index.highWaterID), .text(needle)]
            }
        } else {
            predicate = "n.parent IS NOT NULL AND instr(n.search_name,?)>0"
            parameters = values
        }
        let nodes = try readNodes("SELECT \(Self.nodeColumns)\(Self.nodeFrom) WHERE \(predicate) ORDER BY \(order) LIMIT ?",
                                  parameters + [.integer(Int64(resultLimit))], db: db)
        try check(cancellation)
        return IndexSearchResult(nodes: nodes, totalCount: Int(count))
    }

    private func projectedSearch(needle: String, limit: Int, stablePathOrder: Bool, removals: [ScanRemoval],
                                 db: IndexDatabase, cancellation: ScanCancellation) throws -> IndexSearchResult {
        try db.execute("BEGIN")
        defer { try? db.execute("ROLLBACK") }
        try db.execute("CREATE TEMP TABLE removal_nodes(id INTEGER PRIMARY KEY) WITHOUT ROWID")
        let source = try stagedRemovalSources(removals, db: db, cancellation: cancellation)
        let removalInsert = try db.statement("INSERT OR IGNORE INTO removal_nodes SELECT \(Self.lookupIDSQL)")
        for node in source {
            try check(cancellation)
            try removalInsert.bind(Self.lookupValues(node.id))
            _ = try removalInsert.step(); try removalInsert.reset()
        }
        let projection = try DiskRemovalProjection(removals: removals, source: source, cancellation: cancellation)
        try db.execute("CREATE TEMP TABLE hidden_nodes(id INTEGER PRIMARY KEY) WITHOUT ROWID")
        try db.execute("""
            INSERT INTO hidden_nodes
            WITH RECURSIVE hidden(id) AS (
                SELECT id FROM removal_nodes
                UNION SELECT child.id FROM nodes child JOIN hidden h ON child.parent=h.id
            ) SELECT id FROM hidden
            """)
        try db.execute("CREATE TEMP TABLE adjustments(node_id INTEGER PRIMARY KEY, size INTEGER, allocated INTEGER, count INTEGER) WITHOUT ROWID")
        let adjustmentInsert = try db.statement("INSERT INTO adjustments SELECT id,?,?,? FROM directory_paths WHERE path=?")
        for (path, delta) in projection.indexedDeltas {
            try check(cancellation)
            try adjustmentInsert.bind([.integer(delta.size), .integer(delta.allocated), .integer(Int64(delta.count)), .text(path)])
            _ = try adjustmentInsert.step(); try adjustmentInsert.reset()
        }
        try db.execute("COMMIT")
        let from = Self.nodeFrom + " LEFT JOIN adjustments a ON n.id=a.node_id WHERE n.parent IS NOT NULL AND instr(n.search_name,?)>0 AND NOT EXISTS (SELECT 1 FROM hidden_nodes h WHERE h.id=n.id) AND n.size>=COALESCE(a.size,0) AND n.allocated>=COALESCE(a.allocated,0) AND n.file_count>=COALESCE(a.count,0)"
        let count = try db.integer("SELECT COUNT(*) " + from, [.text(needle)])
        try check(cancellation)
        let resultLimit = max(0, min(limit, 10_000))
        guard count > 0, resultLimit > 0 else { return IndexSearchResult(nodes: [], totalCount: Int(count)) }
        let columns = InventorySchema.pathSQL() + ",n.name,n.is_dir,n.is_link,n.accessible,n.size-COALESCE(a.size,0),n.allocated-COALESCE(a.allocated,0),n.file_count-COALESCE(a.count,0),n.raw_alloc,n.dev,n.inode"
        let tie = stablePathOrder ? InventorySchema.pathSQL() + " COLLATE BINARY" : "n.id"
        let nodes = try readNodes("SELECT " + columns + " " + from + " ORDER BY 6 DESC," + tie + " LIMIT ?",
                                  [.text(needle), .integer(Int64(resultLimit))], db: db)
        try check(cancellation)
        return IndexSearchResult(nodes: nodes, totalCount: Int(count))
    }

    public func invalidate(for root: URL) throws {
        try FileManager.default.createDirectory(at: lockDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let rootLease = try RootMutationLease(directory: lockDirectory, root: root)
        defer { withExtendedLifetime(rootLease) {} }
        Self.mutationLock.lock()
        defer { Self.mutationLock.unlock() }
        try rootLease.validate()
        try rootLease.invalidateGeneration()
        var cacheInfo = stat()
        if lstat(directory.path, &cacheInfo) != 0 {
            guard errno == ENOENT else { throw DiskIndexError.database(String(cString: strerror(errno))) }
            return
        }
        try IndexLocations.validateCache(directory)
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
    package static func validateReplacementPaths(_ paths: [String], root: String, cancellation: ScanCancellation) throws {
        if cancellation.isCancelled { throw CancellationError() }
        var all = Set<String>()
        all.reserveCapacity(paths.count)
        for (index, path) in paths.enumerated() {
            if index & 255 == 0, cancellation.isCancelled { throw CancellationError() }
            guard contains(path, in: root) else { throw DiskIndexError.invalid("An updated folder is outside the index.") }
            guard all.insert(path).inserted else { throw DiskIndexError.invalid("Updated folders overlap. Combine their refreshes first.") }
        }
        for (index, path) in paths.enumerated() {
            if index & 255 == 0, cancellation.isCancelled { throw CancellationError() }
            let bytes = Array(path.utf8)
            for offset in bytes.indices where bytes[offset] == 47 && offset > 0 {
                let prefix = String(decoding: bytes[..<offset], as: UTF8.self)
                if let match = all.firstIndex(of: prefix), contains(path, in: all[match]) {
                    throw DiskIndexError.invalid("Updated folders overlap. Combine their refreshes first.")
                }
            }
            if path != "/", all.contains("/") { throw DiskIndexError.invalid("Updated folders overlap. Combine their refreshes first.") }
        }
    }
    private func check(_ cancellation: ScanCancellation?) throws {
        if cancellation?.isCancelled == true { throw CancellationError() }
    }
    private func subtreePredicate(_ path: String) -> (sql: String, values: [IndexValue]) {
        path == "/" ? ("1", []) : (
            "id=\(Self.lookupIDSQL) OR parent IN (SELECT id FROM directory_paths WHERE path=? OR (path>=? AND path<?))",
            Self.lookupValues(path) + [.text(path), .text(path + "/"), .text(path + "0")])
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
        let statement = try db.statement("INSERT INTO nodes(parent,name,search_name,is_dir,is_link,accessible,size,allocated,file_count,raw_alloc,dev,inode) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")
        let directoryPath = try db.statement("INSERT INTO directory_paths(id,path) VALUES(?,?)")
        let parentID = try parent.map { try db.integer("SELECT id FROM directory_paths WHERE path=?", [.text($0)]) }
        var pending: [(DiskNode, String?, Int64?)] = [(root, parent, parentID)]
        while let (node, parent, parentID) = pending.popLast() {
            try check(cancellation)
            let path = Self.path(node.url)
            let name = parent == nil ? node.name : URL(fileURLWithPath: path).lastPathComponent
            let identity = InventorySchema.parseIdentity(node.fileIdentity)
            guard node.size >= 0, node.allocatedSize >= 0, node.fileCount >= 0, node.rawAllocatedSize >= 0,
                  node.fileIdentity == nil || identity != nil else {
                throw DiskIndexError.invalid("A scanned file has invalid metadata or parentage.")
            }
            if let parent {
                let bytes = path.utf8
                let simple = bytes.first == 47 && (bytes.count == 1 || bytes.last != 47)
                    && !path.hasSuffix("/.") && !path.hasSuffix("/..")
                    && !zip(bytes, bytes.dropFirst()).contains { $0 == 47 && $1 == 47 }
                let validParent: Bool
                if simple, let slash = bytes.lastIndex(of: 47) {
                    validParent = slash == path.startIndex ? parent == "/" : path[..<slash] == parent
                } else {
                    validParent = URL(fileURLWithPath: path, isDirectory: node.isDirectory).deletingLastPathComponent().path == parent
                }
                guard validParent, node.name == name else {
                    throw DiskIndexError.invalid("A scanned file has invalid metadata or parentage.")
                }
            }
            try statement.bind([parentID.map(IndexValue.integer) ?? .null, .text(name), .text(Self.fold(name)),
                                .integer(node.isDirectory ? 1 : 0), .integer(node.isSymbolicLink ? 1 : 0),
                                .integer(node.isAccessible ? 1 : 0), .integer(node.size), .integer(node.allocatedSize),
                                .integer(Int64(node.fileCount)), .integer(node.rawAllocatedSize),
                                identity.map { .integer(Int64($0.device)) } ?? .null,
                                identity.map { .integer(Int64(bitPattern: $0.inode)) } ?? .null])
            _ = try statement.step()
            let nodeID = sqlite3_last_insert_rowid(db.handle)
            try statement.reset()
            if node.isDirectory {
                try directoryPath.bind([.integer(nodeID), .text(path)])
                _ = try directoryPath.step(); try directoryPath.reset()
            }
            pending.append(contentsOf: node.children.map { ($0, path, nodeID) })
        }
    }
    private func readNode(_ path: String, db: IndexDatabase) throws -> DiskNode? {
        try readNodes("SELECT \(Self.nodeColumns)\(Self.nodeFrom) WHERE n.id=\(Self.lookupIDSQL)", Self.lookupValues(path), db: db).first
    }
    private func readChildren(_ parent: String, limit: Int?, db: IndexDatabase) throws -> [DiskNode] {
        var values: [IndexValue] = [.text(parent)]
        if let limit { values.append(.integer(Int64(limit))) }
        return try readNodes("SELECT \(Self.nodeColumns)\(Self.nodeFrom) WHERE n.parent=(SELECT id FROM directory_paths WHERE path=?) ORDER BY n.size DESC,n.name" + (limit == nil ? "" : " LIMIT ?"), values, db: db)
    }
    private func readNodes(_ sql: String, _ values: [IndexValue], db: IndexDatabase) throws -> [DiskNode] {
        let statement = try db.statement(sql)
        try statement.bind(values)
        return try readNodes(statement)
    }
    private func readNodes(_ statement: IndexStatement, cancellation: ScanCancellation? = nil) throws -> [DiskNode] {
        var nodes: [DiskNode] = []
        while try statement.step() {
            if nodes.count & 255 == 0 { try check(cancellation) }
            guard let path = statement.text(0), let name = statement.text(1), statement.integer(5) >= 0,
                  statement.integer(6) >= 0, statement.integer(7) >= 0, statement.integer(8) >= 0 else {
                throw DiskIndexError.invalid("A disk index entry is damaged.")
            }
            let isDirectory = statement.integer(2) != 0
            nodes.append(DiskNode(url: URL(fileURLWithPath: path, isDirectory: isDirectory), name: name, isDirectory: isDirectory,
                                  isSymbolicLink: statement.integer(3) != 0, size: statement.integer(5),
                                  allocatedSize: statement.integer(6), fileCount: Int(statement.integer(7)),
                                  isAccessible: statement.integer(4) != 0, rawAllocatedSize: statement.integer(8),
                                  fileIdentity: statement.identity()))
        }
        return nodes
    }
    private func preview(_ node: DiskNode, depth: Int, budget: inout Int, query: IndexStatement) throws -> DiskNode {
        guard node.isDirectory, depth < 4, budget > 0 else { return node }
        try query.bind([.text(node.id), .integer(Int64(min(96, budget)))])
        let children = try readNodes(query)
        try query.reset()
        budget -= children.count
        let hydrated = try children.map { try preview($0, depth: depth + 1, budget: &budget, query: query) }
        return replacingChildren(node, hydrated)
    }
    private func replacingChildren(_ node: DiskNode, _ children: [DiskNode]) -> DiskNode {
        DiskNode(url: node.url, name: node.name, isDirectory: node.isDirectory, isSymbolicLink: node.isSymbolicLink,
                 size: node.size, allocatedSize: node.allocatedSize, fileCount: node.fileCount, children: children,
                 isAccessible: node.isAccessible, rawAllocatedSize: node.rawAllocatedSize, fileIdentity: node.fileIdentity)
    }
    private func publish(_ temporary: URL, to destination: URL, cancellation: ScanCancellation) throws {
        try cancellation.commitIfNotCancelled {
            guard rename(temporary.path, destination.path) == 0 else { throw DiskIndexError.database(String(cString: strerror(errno))) }
        }
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
    private var checksTaskCancellation = false

    func checkTaskCancellation() {
        guard let cancellation else { return }
        checksTaskCancellation = true
        sqlite3_progress_handler(handle, 1_000, { pointer in
            guard let pointer else { return 0 }
            #if ORBIT_TESTING
            DiskIndexStore.testSearchSQLiteProgress?()
            #endif
            return Unmanaged<ScanCancellation>.fromOpaque(pointer).takeUnretainedValue().isCancelled || Task<Never, Never>.isCancelled ? 1 : 0
        }, Unmanaged.passUnretained(cancellation).toOpaque())
    }

    func registerHardLinkOrder() throws {
        let status = sqlite3_create_collation(handle, "ORBIT_HARDLINK_ORDER", SQLITE_UTF8, nil) { _, leftCount, leftBytes, rightCount, rightBytes in
            guard let leftBytes, let rightBytes else { return 0 }
            let left = Data(bytes: leftBytes, count: Int(leftCount)), right = Data(bytes: rightBytes, count: Int(rightCount))
            if HardLinkPathOrder.precedes(left, right) { return -1 }
            if HardLinkPathOrder.precedes(right, left) { return 1 }
            return 0
        }
        guard status == SQLITE_OK else { throw DiskIndexError.database("Could not prepare hard-link ordering.") }
    }

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
        if cancellation?.isCancelled == true || (checksTaskCancellation && Task<Never, Never>.isCancelled) { return CancellationError() }
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
            case .text(let text): status = text.withCString { sqlite3_bind_text(handle, index, $0, Int32(text.utf8.count), unsafeBitCast(-1, to: sqlite3_destructor_type.self)) }
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
    func identity() -> String? { handle.flatMap { InventorySchema.identity($0, device: 9, inode: 10) } }
    func integer(_ index: Int32) -> Int64 { sqlite3_column_int64(handle, index) }
    func real(_ index: Int32) -> Double { sqlite3_column_double(handle, index) }
}

private final class PreparedNameIndexes: @unchecked Sendable {
    struct Key: Hashable {
        let snapshot: String
        let path: String
        let device: Int32
        let inode: UInt64
        let size: Int64
        let modifiedSeconds: Int
        let modifiedNanos: Int
        let changedSeconds: Int
        let changedNanos: Int
        let staged: Bool
        init?(_ snapshot: IndexSnapshot, staged: Bool = false) {
            var info = stat()
            guard lstat(snapshot.databaseURL.path, &info) == 0, info.st_mode & S_IFMT == S_IFREG else { return nil }
            self.snapshot = snapshot.id; path = snapshot.databaseURL.path; self.staged = staged
            device = info.st_dev; inode = info.st_ino; size = staged ? 0 : info.st_size
            modifiedSeconds = staged ? 0 : info.st_mtimespec.tv_sec; modifiedNanos = staged ? 0 : info.st_mtimespec.tv_nsec
            changedSeconds = staged ? 0 : info.st_ctimespec.tv_sec; changedNanos = staged ? 0 : info.st_ctimespec.tv_nsec
        }
    }
    private let lock = NSLock()
    struct Value {
        let index: CompactNameIndex
        let generation: Int64?
        let complete: Bool
    }
    private var entries: [(key: Key, value: Value)] = []
    private var building: Key?
    func lookup(_ key: Key) -> Value? {
        #if ORBIT_TESTING
        if DiskIndexStore.testDisablePreparedSearch { return nil }
        #endif
        lock.lock(); defer { lock.unlock() }
        guard let position = entries.firstIndex(where: { $0.key == key }) else { return nil }
        let entry = entries.remove(at: position); entries.append(entry)
        return entry.value
    }
    func begin(_ key: Key, check: () throws -> Void) throws {
        while true {
            try check()
            lock.lock()
            if building == nil { building = key; lock.unlock(); return }
            lock.unlock()
            Thread.sleep(forTimeInterval: 0.02)
        }
    }
    func end(_ key: Key) {
        lock.lock(); defer { lock.unlock() }
        if building == key { building = nil }
    }
    func insert(_ index: CompactNameIndex, for key: Key, generation: Int64? = nil, complete: Bool = true) {
        lock.lock(); defer { lock.unlock() }
        entries.removeAll { $0.key.path == key.path }
        while !entries.isEmpty && entries.reduce(index.byteCount, { $0 + $1.value.index.byteCount }) > 128 * 1024 * 1024 {
            entries.removeFirst()
        }
        entries.append((key, Value(index: index, generation: generation, complete: complete)))
    }
}
