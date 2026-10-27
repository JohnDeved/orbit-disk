import CSQLite
import Darwin
import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

struct AsyncCheckFailure: Error, CustomStringConvertible {
    let description: String
}

func require(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
    guard try value() else { throw AsyncCheckFailure(description: message) }
}

func identity(_ url: URL) throws -> String {
    let record = try FileMetadata.read(url)
    return "\(record.device):\(record.inode)"
}

final class Signals: @unchecked Sendable {
    private let condition = NSCondition()
    private var value = 0

    func signal() {
        condition.lock(); value += 1; condition.broadcast(); condition.unlock()
    }

    func until(_ message: String, timeout: TimeInterval = 10, _ predicate: () throws -> Bool) throws {
        let deadline = Date().addingTimeInterval(timeout)
        while true {
            condition.lock(); let observed = value; condition.unlock()
            if try predicate() { return }
            condition.lock()
            if value == observed {
                guard condition.wait(until: deadline) else {
                    condition.unlock()
                    throw AsyncCheckFailure(description: "Timed out: " + message)
                }
            }
            condition.unlock()
        }
    }
}

final class HookOutcome: @unchecked Sendable {
    private let lock = NSLock()
    private var called = false
    private var failure: Error?
    func run(_ body: () throws -> Void) {
        lock.lock()
        guard !called else { lock.unlock(); return }
        called = true; lock.unlock()
        do { try body() } catch { lock.lock(); failure = error; lock.unlock() }
    }
    func verify() throws {
        lock.lock(); defer { lock.unlock() }
        try require(called, "Snapshot boundary hook was not reached")
        if let failure { throw failure }
    }
}

final class FirstDirectoryFailure: @unchecked Sendable {
    private let lock = NSLock()
    private var selected: String?
    private var failure: Error?
    let remove: Bool
    init(remove: Bool) { self.remove = remove }
    func intercept(_ bytes: Data) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard selected == nil else { return false }
        let path = String(decoding: bytes, as: UTF8.self)
        selected = path
        do {
            if remove { try FileManager.default.removeItem(atPath: path) }
            else { try require(chmod(path, 0) == 0, "Could not deny test directory permissions") }
        } catch { failure = error }
        return false
    }
    func path() throws -> String {
        lock.lock(); defer { lock.unlock() }
        if let failure { throw failure }
        guard let selected else { throw AsyncCheckFailure(description: "No directory selected for failure") }
        return selected
    }
    func preferredPath() -> Data? {
        lock.lock(); defer { lock.unlock() }
        return selected.map { Data($0.utf8) }
    }
    func restore() {
        lock.lock(); defer { lock.unlock() }
        if !remove, let selected { _ = chmod(selected, 0o700) }
    }
}

final class DuplicateListingInjection: @unchecked Sendable {
    private let lock = NSLock()
    private let target: Data
    private let crossBatch: Bool
    private var first: FileMetadata?
    private var calls = 0
    private var injected = false
    init(target: URL, crossBatch: Bool) {
        self.target = Data(target.path.utf8); self.crossBatch = crossBatch
    }
    func records(_ path: Data, _ records: [FileMetadata]) -> [FileMetadata] {
        guard path == target else { return records }
        lock.lock(); defer { lock.unlock() }
        calls += 1
        guard records.count > 1 else { return records }
        var result = records
        if crossBatch {
            if calls == 1 { first = records[0] }
            else if calls == 2, let first { result[0] = first; injected = true }
        } else if calls == 1 {
            result[1] = result[0]; injected = true
        }
        return result
    }
    func verify() throws {
        lock.lock(); defer { lock.unlock() }
        try require(injected, "Duplicate listing callback was not reached")
    }
}

struct AsyncFixture {
    let base: URL
    let root: URL
    let cache: URL

    init(_ base: URL, _ name: String) throws {
        let directory = base.appendingPathComponent(name, isDirectory: true)
        let root = directory.appendingPathComponent("root", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        self.base = directory.standardizedFileURL
        self.root = root.standardizedFileURL
        cache = self.base.appendingPathComponent("cache", isDirectory: true)
    }

    @discardableResult func directory(_ relative: String) throws -> URL {
        let url = root.appendingPathComponent(relative, isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    @discardableResult func file(_ relative: String, size: Int = 7) throws -> URL {
        let url = root.appendingPathComponent(relative)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data(repeating: 7, count: size).write(to: url)
        return url
    }

    func stage(_ cancellation: ScanCancellation = ScanCancellation()) throws -> ResumableStage {
        let root = root.standardizedFileURL
        return try ResumableStage(cache: cache, root: root,
            current: StageCheckpoint(rootPath: root.path, identity: "async-check", eventID: 0), cancellation: cancellation)
    }
}

func scalar(_ path: String, _ sql: String) throws -> Int64 {
    var handle: OpaquePointer?
    guard sqlite3_open_v2(path, &handle, SQLITE_OPEN_READONLY, nil) == SQLITE_OK, let handle else {
        throw AsyncCheckFailure(description: "Cannot open test inventory")
    }
    defer { sqlite3_close(handle) }
    var statement: OpaquePointer?
    guard sqlite3_prepare_v2(handle, sql, -1, &statement, nil) == SQLITE_OK, let statement else {
        throw AsyncCheckFailure(description: "Cannot prepare test inventory query")
    }
    defer { sqlite3_finalize(statement) }
    guard sqlite3_step(statement) == SQLITE_ROW else { throw AsyncCheckFailure(description: "No scalar row") }
    return sqlite3_column_int64(statement, 0)
}

func finish(_ stage: ResumableStage) throws {
    while try stage.advance(progress: { _ in }) {}
}

func verifyTree(_ stage: ResumableStage, fixture: AsyncFixture) throws {
    let expected = try DiskScanner().scan(fixture.root).root
    func compare(_ reference: DiskNode) throws {
        guard let actual = try stage.view(path: reference.id, removals: [], cancellation: ScanCancellation())?.folder else {
            throw AsyncCheckFailure(description: "Missing mapped folder")
        }
        try require(actual.scanState == .complete, "Completed discovery left a partial folder")
        try require(actual.size == reference.size && actual.allocatedSize == reference.allocatedSize && actual.fileCount == reference.fileCount,
                    "Mapped folder totals differ from filesystem scanner")
        let observed = Dictionary(uniqueKeysWithValues: actual.children.map { ($0.id, $0) })
        try require(observed.count == reference.children.count, "Mapped child count differs")
        for child in reference.children {
            guard let node = observed[child.id] else { throw AsyncCheckFailure(description: "Mapped child is missing") }
            try require(node.fileIdentity == child.fileIdentity && node.isDirectory == child.isDirectory && node.isSymbolicLink == child.isSymbolicLink,
                        "Mapped metadata differs")
            try require(node.size == child.size && node.allocatedSize == child.allocatedSize && node.fileCount == child.fileCount,
                        "Mapped child totals differ")
            if child.isDirectory && !child.isSymbolicLink { try compare(child) }
        }
    }
    try compare(expected)
}
