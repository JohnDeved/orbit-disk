import CSQLite
import Darwin
import Foundation

/// A cache-wide record of removals, committed before the filesystem is changed.
public final class RemovalJournal: @unchecked Sendable {
    public enum Failure: Error { case unsafe(String), invalid(String), sqlite(String), closed }
    public struct Entry: Sendable {
        public let sequence: Int64
        public let root: String
        public let rootIdentity: String
        public let completed: Bool
    }
    public enum Relation: Sendable { case inside, coversRoot, unresolved }
    public struct Intersection: Sendable {
        public let originalPath: String
        public let removal: ScanRemoval
        public let relation: Relation
        public let originalRelation: Relation?
    }
    fileprivate struct ParentIdentity {
        let canonical: Data?
        let identity: String?
    }
    private let directory: URL
    private let dirFD: Int32
    private let directoryIdentity: stat
    private let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
    private let fenceName = "journal.lock"
    private let dbName = "journal.sqlite"
    #if ORBIT_TESTING
    package static var testBeforePendingCommit: (@Sendable () -> Void)?
    package static var testAfterRecoveryProbe: (@Sendable () -> Void)?
    #endif

    public init(cache: URL) throws {
        let parentURL = IndexLocations.locks(for: cache)
        try FileManager.default.createDirectory(at: parentURL, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let parent = open(parentURL.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard parent >= 0 else { throw Failure.unsafe("journal parent") }
        defer { close(parent) }
        if mkdirat(parent, ".orbit-removals", 0o700) != 0 && errno != EEXIST { throw Failure.unsafe("journal directory") }
        guard fsync(parent) == 0 else { throw Failure.unsafe("sync journal parent") }
        let dir = openat(parent, ".orbit-removals", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard dir >= 0 else { throw Failure.unsafe("open journal directory") }
        var info = stat()
        guard fstat(dir, &info) == 0, (info.st_mode & S_IFMT) == S_IFDIR, info.st_uid == geteuid(), (info.st_mode & 0o777) == 0o700 else {
            close(dir); throw Failure.unsafe("journal directory identity")
        }
        directory = parentURL.appendingPathComponent(".orbit-removals", isDirectory: true)
        dirFD = dir; directoryIdentity = info
        do { try validateDirectory() } catch { close(dir); throw error }
    }
    deinit { close(dirFD) }

    private func validateDirectory() throws {
        var held = stat(), actual = stat()
        guard fstat(dirFD, &held) == 0, lstat(directory.path, &actual) == 0,
              (actual.st_mode & S_IFMT) == S_IFDIR, actual.st_uid == geteuid(), (actual.st_mode & 0o777) == 0o700,
              actual.st_dev == directoryIdentity.st_dev, actual.st_ino == directoryIdentity.st_ino,
              held.st_dev == directoryIdentity.st_dev, held.st_ino == directoryIdentity.st_ino else { throw Failure.unsafe("journal directory replaced") }
    }
    private func checkedFile(_ name: String, flags: Int32, create: Bool) throws -> (Int32, stat) {
        try validateDirectory()
        var created = false
        var fd: Int32
        if create && (flags & O_EXCL) == 0 {
            fd = openat(dirFD, name, flags | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
            if fd >= 0 { created = true }
            else if errno == EEXIST { fd = openat(dirFD, name, (flags & ~O_CREAT) | O_NOFOLLOW | O_CLOEXEC, 0o600) }
        } else {
            fd = openat(dirFD, name, flags | O_NOFOLLOW | O_CLOEXEC, 0o600)
            created = create && fd >= 0
        }
        guard fd >= 0 else { throw Failure.unsafe("open \(name)") }
        var held = stat(), actual = stat()
        guard fstat(fd, &held) == 0, fstatat(dirFD, name, &actual, AT_SYMLINK_NOFOLLOW) == 0,
              (held.st_mode & S_IFMT) == S_IFREG, held.st_uid == geteuid(), held.st_nlink == 1, (held.st_mode & 0o777) == 0o600,
              held.st_dev == actual.st_dev, held.st_ino == actual.st_ino else { close(fd); throw Failure.unsafe("unsafe \(name)") }
        if created && fsync(dirFD) != 0 { close(fd); throw Failure.unsafe("sync journal directory") }
        return (fd, held)
    }
    private func validateFile(_ name: String, fd: Int32, expected: stat) throws {
        try validateDirectory()
        var held = stat(), actual = stat()
        guard fstat(fd, &held) == 0, fstatat(dirFD, name, &actual, AT_SYMLINK_NOFOLLOW) == 0,
              (actual.st_mode & S_IFMT) == S_IFREG, actual.st_uid == geteuid(), actual.st_nlink == 1, (actual.st_mode & 0o777) == 0o600,
              held.st_dev == expected.st_dev, held.st_ino == expected.st_ino,
              actual.st_dev == expected.st_dev, actual.st_ino == expected.st_ino else { throw Failure.unsafe("journal file replaced") }
    }
    private static func canonicalDirectory(_ url: URL) -> Data? {
        var before = stat(), after = stat()
        guard stat(url.path,&before) == 0, (before.st_mode & S_IFMT) == S_IFDIR,
              let resolved = realpath(url.path, nil) else { return nil }
        defer { free(resolved) }
        let path = String(cString: resolved)
        let canonical = IndexLocations.exclusionPaths([URL(fileURLWithPath: path, isDirectory: true)]).min { $0.lexicographicallyPrecedes($1) }
        guard stat(url.path,&after) == 0, before.st_dev == after.st_dev, before.st_ino == after.st_ino else { return nil }
        return canonical
    }
    private static func parentIdentity(_ url: URL) -> ParentIdentity {
        var before = stat(), after = stat()
        guard stat(url.path, &before) == 0, let canonical = canonicalDirectory(url), stat(url.path, &after) == 0,
              before.st_dev == after.st_dev, before.st_ino == after.st_ino else { return ParentIdentity(canonical:nil,identity:nil) }
        return ParentIdentity(canonical:canonical,identity:"\(before.st_dev):\(before.st_ino)")
    }
    private func checkSidecars() throws {
        for suffix in ["-wal", "-shm", "-journal"] {
            var info = stat()
            if fstatat(dirFD, dbName + suffix, &info, AT_SYMLINK_NOFOLLOW) != 0 {
                guard errno == ENOENT else { throw Failure.unsafe("journal sidecar") }; continue
            }
            guard (info.st_mode & S_IFMT) == S_IFREG, info.st_uid == geteuid(), info.st_nlink == 1, (info.st_mode & 0o777) == 0o600 else { throw Failure.unsafe("journal sidecar identity") }
        }
    }
    private func waitLock(_ fd: Int32, cancellation: ScanCancellation) throws {
        while flock(fd, LOCK_EX | LOCK_NB) != 0 {
            let e = errno
            if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
            guard e == EWOULDBLOCK || e == EINTR else { throw Failure.unsafe("journal lock") }
            usleep(10_000)
        }
        if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
    }

    public final class Operation: @unchecked Sendable {
        fileprivate let journal: RemovalJournal
        fileprivate let name: String
        fileprivate let fd: Int32
        fileprivate let identity: stat
        public let sequence: Int64
        private let parents: [Data: ParentIdentity]
        private let lock = NSLock()
        private var completed = false
        fileprivate init(journal: RemovalJournal, name: String, fd: Int32, identity: stat, sequence: Int64, parents: [Data: ParentIdentity]) {
            self.journal = journal; self.name = name; self.fd = fd; self.identity = identity; self.sequence = sequence; self.parents = parents
        }
        /// Reject a resolved parent that changed since its intent was committed.
        public func parentIsUnchanged(path: String) -> Bool {
            let parent = URL(fileURLWithPath:path).deletingLastPathComponent()
            guard let saved = parents[Data(parent.path.utf8)] else { return false }
            guard let expectedIdentity = saved.identity, let expectedPath = saved.canonical else { return false }
            let current = RemovalJournal.parentIdentity(parent)
            return current.identity == expectedIdentity && current.canonical == expectedPath
        }
        deinit { close(fd) }
        /// Call only after the last attempted filesystem mutation, even if some attempts failed.
        public func complete(cancellation: ScanCancellation = ScanCancellation()) throws {
            lock.lock(); defer { lock.unlock() }
            guard !completed else { throw Failure.invalid("operation already completed") }
            try journal.validateFile(name, fd: fd, expected: identity)
            try journal.withFence(cancellation: cancellation) { fence in try fence.finish(sequence: sequence) }
            completed = true
        }
    }

    public func begin(root: URL, rootIdentity: String, items: [ScanRemoval], cancellation: ScanCancellation = ScanCancellation()) throws -> Operation {
        guard root.isFileURL else { throw Failure.invalid("removal root") }
        let path = root.standardizedFileURL.path, rootBytes = Data(path.utf8)
        guard rootBytes.first == 47 else { throw Failure.invalid("removal root") }
        var prefix = rootBytes; if prefix.last != 47 { prefix.append(47) }
        guard !rootIdentity.isEmpty, !items.isEmpty else { throw Failure.invalid("empty removal") }
        var parents: [Data: ParentIdentity] = [:]
        for item in items {
            if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
            let bytes = Data(item.path.utf8)
            guard !item.identity.isEmpty, bytes.count > prefix.count, bytes.starts(with: prefix), !bytes.contains(0),
                  Data(URL(fileURLWithPath: item.path).standardizedFileURL.path.utf8) == bytes else { throw Failure.invalid("removal path") }
            let parent = URL(fileURLWithPath:item.path).deletingLastPathComponent(), key = Data(parent.path.utf8)
            if parents[key] == nil { parents[key] = Self.parentIdentity(parent) }
        }
        let name = UUID().uuidString + ".operation"
        let (fd, info) = try checkedFile(name, flags: O_CREAT | O_EXCL | O_RDWR, create: true)
        var keep = false; defer { if !keep { close(fd) } }
        try waitLock(fd, cancellation: cancellation)
        let sequence = try withFence(cancellation: cancellation) { fence in
            try fence.insert(root: path, rootIdentity: rootIdentity, name: name, items: items, parents: parents, cancellation: cancellation)
        }
        keep = true
        return Operation(journal: self, name: name, fd: fd, identity: info, sequence: sequence, parents: parents)
    }

    public func withFence<T>(cancellation: ScanCancellation = ScanCancellation(), _ body: (Fence) throws -> T) throws -> T {
        let (fd, identity) = try checkedFile(fenceName, flags: O_CREAT | O_RDWR, create: true)
        defer { close(fd) }
        try waitLock(fd, cancellation: cancellation)
        try validateFile(fenceName, fd: fd, expected: identity)
        let fence = try Fence(journal: self, lockFD: fd, lockIdentity: identity, cancellation: cancellation)
        defer { fence.close() }
        let result = try body(fence)
        try validateFile(fenceName, fd: fd, expected: identity)
        try fence.validate()
        return result
    }

    public final class Fence {
        private let journal: RemovalJournal
        private let cancellation: ScanCancellation
        private let lockFD: Int32
        private let lockIdentity: stat
        private var db: OpaquePointer?
        private let fileFD: Int32
        private let fileIdentity: stat
        private(set) public var highWater: Int64 = 0
        private(set) public var identity: String = ""
        public var floor: Int64 { 0 }
        fileprivate init(journal: RemovalJournal, lockFD: Int32, lockIdentity: stat, cancellation: ScanCancellation) throws {
            self.journal = journal; self.lockFD = lockFD; self.lockIdentity = lockIdentity; self.cancellation = cancellation
            let (file, identity) = try journal.checkedFile(journal.dbName, flags: O_CREAT | O_RDWR, create: true)
            fileFD = file; fileIdentity = identity
            do {
                try journal.checkSidecars()
                let path = journal.directory.appendingPathComponent(journal.dbName).path
                var handle: OpaquePointer?
                guard sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE | SQLITE_OPEN_NOFOLLOW, nil) == SQLITE_OK, let handle else {
                    if let handle { sqlite3_close(handle) }; throw Failure.sqlite("open journal")
                }
                db = handle
                try validate()
                let appID = try scalar("PRAGMA application_id"), version = try scalar("PRAGMA user_version")
                let objects = try scalar("SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
                guard (appID == 0 && version == 0 && objects == 0) || (appID == 0x4f524a4e && (version == 2 || version == 3)) else { throw Failure.invalid("incompatible journal") }
                try exec("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
                if objects == 0 {
                    try exec("BEGIN IMMEDIATE; PRAGMA application_id=1330793038; PRAGMA user_version=3; CREATE TABLE metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),identity TEXT NOT NULL); CREATE TABLE operations(seq INTEGER PRIMARY KEY AUTOINCREMENT,root BLOB NOT NULL,root_identity TEXT NOT NULL,lock_name TEXT NOT NULL UNIQUE,complete INTEGER NOT NULL DEFAULT 0); CREATE INDEX pending_operations ON operations(complete,seq); CREATE TABLE items(op INTEGER NOT NULL,path BLOB NOT NULL,identity TEXT NOT NULL,canonical BLOB,parent_identity TEXT,PRIMARY KEY(op,path)) WITHOUT ROWID;")
                    let meta = try prepare("INSERT INTO metadata VALUES(1,?)")
                    text(UUID().uuidString,1,meta)
                    do { try done(meta); sqlite3_finalize(meta) } catch { sqlite3_finalize(meta); throw error }
                    try exec("COMMIT")
                } else {
                    guard try scalar("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('metadata','operations','items')") == 3 else { throw Failure.invalid("incompatible journal schema") }
                    if version == 2 {
                        try exec("BEGIN IMMEDIATE; CREATE INDEX pending_operations ON operations(complete,seq); PRAGMA user_version=3; COMMIT;")
                    }
                }
                let meta = try prepare("SELECT identity FROM metadata WHERE singleton=1")
                defer { sqlite3_finalize(meta) }
                guard sqlite3_step(meta) == SQLITE_ROW else { throw Failure.invalid("missing journal identity") }
                self.identity = try value(meta,0)
                guard UUID(uuidString:self.identity) != nil, sqlite3_step(meta) == SQLITE_DONE else { throw Failure.invalid("invalid journal identity") }
                highWater = try scalar("SELECT COALESCE(MAX(seq),0) FROM operations")
            } catch { if let db { sqlite3_close(db); self.db = nil }; Darwin.close(file); throw error }
        }
        fileprivate func close() { if let db { sqlite3_close(db); self.db = nil; Darwin.close(fileFD) } }
        /// Verify the fence and database immediately before a snapshot rename.
        public func validate() throws {
            guard db != nil else { throw Failure.closed }
            try journal.validateFile(journal.fenceName, fd:lockFD, expected:lockIdentity)
            try journal.validateFile(journal.dbName, fd:fileFD, expected:fileIdentity)
            try journal.checkSidecars()
        }
        private func check() throws { guard db != nil else { throw Failure.closed }; if cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() } }
        private func exec(_ sql: String) throws { try check(); guard sqlite3_exec(db,sql,nil,nil,nil) == SQLITE_OK else { throw Failure.sqlite(String(cString:sqlite3_errmsg(db))) } }
        private func prepare(_ sql: String) throws -> OpaquePointer { try check(); var stmt: OpaquePointer?; guard sqlite3_prepare_v2(db,sql,-1,&stmt,nil) == SQLITE_OK, let stmt else { throw Failure.sqlite("prepare") }; return stmt }
        private func done(_ stmt: OpaquePointer) throws { try check(); guard sqlite3_step(stmt) == SQLITE_DONE else { throw Failure.sqlite(String(cString:sqlite3_errmsg(db))) } }
        private func scalar(_ sql: String) throws -> Int64 { let s=try prepare(sql);defer{sqlite3_finalize(s)};guard sqlite3_step(s)==SQLITE_ROW else {throw Failure.sqlite("scalar")};return sqlite3_column_int64(s,0) }
        private func blob(_ data: Data,_ i:Int32,_ s:OpaquePointer) { data.withUnsafeBytes { _ = sqlite3_bind_blob(s,i,$0.baseAddress,Int32(data.count),journal.transient) } }
        private func text(_ value:String,_ i:Int32,_ s:OpaquePointer) { value.withCString { _ = sqlite3_bind_text(s,i,$0,-1,journal.transient) } }
        private func value(_ s:OpaquePointer,_ i:Int32) throws -> String {
            guard let p=sqlite3_column_blob(s,i) else {throw Failure.sqlite("missing value")}
            let data=Data(bytes:p,count:Int(sqlite3_column_bytes(s,i)))
            guard let value=String(data:data,encoding:.utf8) else {throw Failure.sqlite("invalid UTF-8")};return value
        }
        fileprivate func insert(root: String, rootIdentity: String, name: String, items: [ScanRemoval], parents: [Data: ParentIdentity], cancellation: ScanCancellation) throws -> Int64 {
            try exec("BEGIN IMMEDIATE")
            do {
                let op=try prepare("INSERT INTO operations(root,root_identity,lock_name) VALUES(?,?,?)");defer{sqlite3_finalize(op)}
                blob(Data(root.utf8),1,op);text(rootIdentity,2,op);text(name,3,op);try done(op)
                let seq=sqlite3_last_insert_rowid(db)
                let item=try prepare("INSERT INTO items(op,path,identity,canonical,parent_identity) VALUES(?,?,?,?,?)");defer{sqlite3_finalize(item)}
                for entry in items {
                    try check();sqlite3_reset(item);sqlite3_clear_bindings(item);sqlite3_bind_int64(item,1,seq);blob(Data(entry.path.utf8),2,item);text(entry.identity,3,item)
                    let url = URL(fileURLWithPath:entry.path), parent = parents[Data(url.deletingLastPathComponent().path.utf8)]
                    if let parent, var canonical = parent.canonical, let identity = parent.identity {
                        if canonical.last != 47 { canonical.append(47) }
                        canonical.append(contentsOf:url.lastPathComponent.utf8)
                        blob(canonical,4,item); text(identity,5,item)
                    } else { sqlite3_bind_null(item,4); sqlite3_bind_null(item,5) }
                    try done(item)
                }
                #if ORBIT_TESTING
                RemovalJournal.testBeforePendingCommit?()
                #endif
                try validate(); try exec("COMMIT"); highWater=seq; return seq
            } catch { try? exec("ROLLBACK"); throw error }
        }
        fileprivate func finish(sequence: Int64) throws {
            let s=try prepare("UPDATE operations SET complete=1 WHERE seq=? AND complete=0");defer{sqlite3_finalize(s)}
            sqlite3_bind_int64(s,1,sequence);try validate();try done(s)
            guard sqlite3_changes(db) == 1 else {throw Failure.invalid("unknown operation")}
        }
        public func operations(after: Int64, limit: Int) throws -> [Entry] {
            guard after >= floor, after <= highWater, limit > 0 && limit <= 512 else {throw Failure.invalid("operation page")}
            let s=try prepare("SELECT seq,root,root_identity,complete FROM operations WHERE seq>? AND seq<=? ORDER BY seq LIMIT ?");defer{sqlite3_finalize(s)}
            sqlite3_bind_int64(s,1,after);sqlite3_bind_int64(s,2,highWater);sqlite3_bind_int(s,3,Int32(limit));var output:[Entry]=[]
            while true { try check();let status=sqlite3_step(s);if status==SQLITE_DONE {break};guard status==SQLITE_ROW else {throw Failure.sqlite("read operations")};output.append(Entry(sequence:sqlite3_column_int64(s,0),root:try value(s,1),rootIdentity:try value(s,2),completed:sqlite3_column_int(s,3) != 0)) }
            return output
        }
        public func pendingOperations(after: Int64, limit: Int) throws -> [Entry] {
            guard after >= floor, after <= highWater, limit > 0 && limit <= 512 else { throw Failure.invalid("pending operation page") }
            let s = try prepare("SELECT seq,root,root_identity,complete FROM operations WHERE complete=0 AND seq>? AND seq<=? ORDER BY seq LIMIT ?")
            defer { sqlite3_finalize(s) }
            sqlite3_bind_int64(s,1,after); sqlite3_bind_int64(s,2,highWater); sqlite3_bind_int(s,3,Int32(limit))
            var output: [Entry] = []
            while true {
                try check()
                let status = sqlite3_step(s)
                if status == SQLITE_DONE { break }
                guard status == SQLITE_ROW else { throw Failure.sqlite("read pending operations") }
                output.append(Entry(sequence:sqlite3_column_int64(s,0),root:try value(s,1),rootIdentity:try value(s,2),completed:false))
            }
            return output
        }
        public func isLive(sequence:Int64) throws -> Bool {
            let s=try prepare("SELECT lock_name FROM operations WHERE seq=?");defer{sqlite3_finalize(s)};sqlite3_bind_int64(s,1,sequence)
            guard sqlite3_step(s)==SQLITE_ROW else {throw Failure.invalid("unknown operation")};let name=try value(s,0)
            guard name.hasSuffix(".operation"), UUID(uuidString:String(name.dropLast(".operation".count))) != nil else {throw Failure.sqlite("invalid operation lock")}
            let (fd,identity)=try journal.checkedFile(name,flags:O_RDWR,create:false);defer{Darwin.close(fd)}
            if flock(fd,LOCK_EX|LOCK_NB) != 0 {guard errno == EWOULDBLOCK else {throw Failure.unsafe("operation lock")};return true}
            try journal.validateFile(name,fd:fd,expected:identity);return false
        }
        func recoveryRootIdentity(_ root: URL) -> Data? {
            var before = stat(), after = stat()
            guard stat(root.path, &before) == 0, let canonical = RemovalJournal.canonicalDirectory(root),
                  stat(root.path, &after) == 0, before.st_dev == after.st_dev, before.st_ino == after.st_ino else { return nil }
            var identity = Data("\(before.st_dev):\(before.st_ino):".utf8)
            identity.append(canonical)
            return identity
        }
        public func intersections(sequence:Int64, root:URL, afterPath:String? = nil, limit:Int) throws -> [Intersection] {
            guard limit > 0 && limit <= 512, root.isFileURL else {throw Failure.invalid("item page")}
            let rootPath = root.standardizedFileURL.path, rootData = Data(rootPath.utf8)
            let canonicalRoot = RemovalJournal.canonicalDirectory(root.standardizedFileURL)
            func range(_ root: Data) -> (Data, Data, [Data]) {
                var prefix=root; if prefix.last != 47 {prefix.append(47)}
                var upper=prefix;upper[upper.index(before:upper.endIndex)]=48
                var ancestors:[Data]=[],current=root
                while true {
                    ancestors.append(current);if current == Data([47]) {break}
                    guard let slash=current.lastIndex(of:47) else {break}
                    current=slash==0 ? Data([47]) : Data(current.prefix(upTo:slash))
                }
                return (prefix,upper,ancestors)
            }
            let raw=range(rootData), canonical=canonicalRoot.map(range)
            let after=afterPath == nil ? "" : "AND path>?"
            let rawPlaceholders=raw.2.map{_ in "?"}.joined(separator:",")
            let canonicalPlaceholders=canonical?.2.map{_ in "?"}.joined(separator:",") ?? ""
            let matching = canonical == nil ? "" : "AND (canonical IS NULL OR (canonical>=? AND canonical<?) OR canonical IN (\(canonicalPlaceholders)) OR (path>=? AND path<?) OR path IN (\(rawPlaceholders)))"
            let s=try prepare("SELECT path,identity,canonical FROM items WHERE op=? \(after) \(matching) ORDER BY path LIMIT ?");defer{sqlite3_finalize(s)}
            var i:Int32=1;sqlite3_bind_int64(s,i,sequence);i+=1
            if let afterPath {blob(Data(afterPath.utf8),i,s);i+=1}
            if let canonical {
                blob(canonical.0,i,s);i+=1;blob(canonical.1,i,s);i+=1
                for ancestor in canonical.2 {blob(ancestor,i,s);i+=1}
                blob(raw.0,i,s);i+=1;blob(raw.1,i,s);i+=1
                for ancestor in raw.2 {blob(ancestor,i,s);i+=1}
            }
            sqlite3_bind_int(s,i,Int32(limit))
            var output:[Intersection]=[]
            while true {
                try check();let status=sqlite3_step(s);if status==SQLITE_DONE {break};guard status==SQLITE_ROW else {throw Failure.sqlite("read items")}
                let original=try value(s,0), identity=try value(s,1), originalBytes=Data(original.utf8)
                let rawRelation: Relation? = originalBytes.starts(with:raw.0) ? .inside : raw.2.contains(originalBytes) ? .coversRoot : nil
                guard let canonicalRoot, let canonicalRange=canonical, sqlite3_column_type(s,2) != SQLITE_NULL else {
                    output.append(Intersection(originalPath:original,removal:ScanRemoval(path:original,identity:identity),relation:.unresolved,originalRelation:rawRelation));continue
                }
                guard let bytes=sqlite3_column_blob(s,2) else {throw Failure.sqlite("missing canonical path")}
                let resolved=Data(bytes:bytes,count:Int(sqlite3_column_bytes(s,2)))
                if resolved.starts(with:canonicalRange.0) && resolved.count > canonicalRoot.count {
                    var projected=rootData
                    let suffix=resolved.dropFirst(canonicalRoot.count)
                    if projected.last != 47 && suffix.first != 47 {projected.append(47)}
                    projected.append(contentsOf:suffix)
                    guard let path=String(data:projected,encoding:.utf8) else {throw Failure.sqlite("invalid projected path")}
                    let additional=Data(path.utf8)==originalBytes ? nil : rawRelation
                    output.append(Intersection(originalPath:original,removal:ScanRemoval(path:path,identity:identity),relation:.inside,originalRelation:additional))
                } else if canonicalRange.2.contains(resolved) {
                    output.append(Intersection(originalPath:original,removal:ScanRemoval(path:original,identity:identity),relation:.coversRoot,originalRelation:rawRelation == .inside ? rawRelation : nil))
                } else if let rawRelation {
                    output.append(Intersection(originalPath:original,removal:ScanRemoval(path:original,identity:identity),relation:rawRelation,originalRelation:nil))
                } else { throw Failure.sqlite("invalid intersection") }
            }
            return output
        }
    }
}
