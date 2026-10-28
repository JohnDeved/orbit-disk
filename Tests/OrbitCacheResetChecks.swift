import Foundation

@main
enum CacheResetChecks {
    @MainActor
    static func main() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("orbit-cache-reset-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        try noRequest(root)
        try requested(root)
        try missingCaches(root)
        for mapping in [false, true] {
            for parent in [false, true] {
                try rejectsSymlink(root, mapping: mapping, parent: parent)
            }
        }
        try rejectsFile(root)
        print("PASS: requested cache reset, preserved preferences, safe paths and retry after validation failure")
    }

    @MainActor
    private static func noRequest(_ root: URL) throws {
        let fixture = try Fixture(root, name: "no-request")
        try require(try !fixture.perform(), "An unrequested reset must be a no-op")
        try fixture.requireCacheFiles()
        try fixture.requirePreferences(pending: false, lastRoot: true)
        try fixture.requireSentinels()
    }

    @MainActor
    private static func requested(_ root: URL) throws {
        let fixture = try Fixture(root, name: "requested")
        fixture.request()
        try require(try fixture.perform(), "A requested reset must report completion")
        try require(!fixture.exists(fixture.indexes) && !fixture.exists(fixture.mapping), "Reset must remove both cache trees")
        try fixture.requirePreferences(pending: false, lastRoot: false)
        try fixture.requireSentinels()
        try require(try !fixture.perform(), "A completed reset must not repeat")
    }

    @MainActor
    private static func missingCaches(_ root: URL) throws {
        for missingParents in [false, true] {
            let fixture = try Fixture(root, name: "missing-\(missingParents)")
            for target in [fixture.indexes, fixture.mapping] {
                try FileManager.default.removeItem(at: missingParents ? target.deletingLastPathComponent() : target)
            }
            fixture.request()
            try require(try fixture.perform(), "Missing cache folders must be accepted")
            try require(!fixture.exists(fixture.indexes) && !fixture.exists(fixture.mapping), "Reset must not create missing caches")
            try fixture.requirePreferences(pending: false, lastRoot: false)
            try require(fixture.exists(fixture.outside), "Missing-cache reset changed an unrelated folder")
        }
    }

    @MainActor
    private static func rejectsSymlink(_ root: URL, mapping: Bool, parent: Bool) throws {
        let fixture = try Fixture(root, name: "symlink-\(mapping)-\(parent)")
        let target = mapping ? fixture.mapping : fixture.indexes
        let replaced = parent ? target.deletingLastPathComponent() : target
        try FileManager.default.removeItem(at: replaced)
        try FileManager.default.createSymbolicLink(at: replaced, withDestinationURL: fixture.outside)
        fixture.request()
        try expectFailure { _ = try fixture.perform() }
        try fixture.requirePreferences(pending: true, lastRoot: true)
        try fixture.requireOutsideFiles()
        let other = mapping ? fixture.indexes : fixture.mapping
        try require(fixture.exists(other.appendingPathComponent("cache-data")), "Validation must finish before either cache is removed")

        try FileManager.default.removeItem(at: replaced)
        try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
        try require(try fixture.perform(), "A retained reset request must succeed after the invalid path is repaired")
        try require(!fixture.exists(fixture.indexes) && !fixture.exists(fixture.mapping), "Retry left cache data behind")
        try fixture.requirePreferences(pending: false, lastRoot: false)
        try fixture.requireOutsideFiles()
    }

    @MainActor
    private static func rejectsFile(_ root: URL) throws {
        let fixture = try Fixture(root, name: "regular-file")
        try FileManager.default.removeItem(at: fixture.mapping)
        try Data("not a cache directory".utf8).write(to: fixture.mapping)
        fixture.request()
        try expectFailure { _ = try fixture.perform() }
        try require(fixture.exists(fixture.indexes.appendingPathComponent("cache-data")), "An invalid second target must leave the first cache untouched")
        try require(try String(contentsOf: fixture.mapping, encoding: .utf8) == "not a cache directory", "Validation removed a regular file")
        try fixture.requirePreferences(pending: true, lastRoot: true)
        try fixture.requireSentinels()
    }
}

@MainActor
private final class Fixture {
    let preferences: UserDefaults
    let indexes: URL
    let mapping: URL
    let outside: URL
    private let suite: String
    private let sentinels: [URL]
    private let places = ["/Users/test/Projects", "/Volumes/Archive"]

    init(_ root: URL, name: String) throws {
        let directory = root.appendingPathComponent(name)
        indexes = directory.appendingPathComponent("Caches/local.orbitdisk.app/Indexes")
        mapping = directory.appendingPathComponent("Application Support/local.orbitdisk.app/Mapping")
        outside = directory.appendingPathComponent("outside")
        sentinels = [indexes.deletingLastPathComponent().appendingPathComponent("keep-cache-sibling"),
                     mapping.deletingLastPathComponent().appendingPathComponent("keep-support-sibling"),
                     directory.appendingPathComponent("Caches/another.app/keep")]
        suite = "OrbitCacheResetChecks.\(UUID().uuidString)"
        preferences = UserDefaults(suiteName: suite)!
        preferences.set("/Users/test/Documents", forKey: "lastIndexedRoot")
        preferences.set(places, forKey: "folderPlaces")
        preferences.set(true, forKey: "SUEnableAutomaticChecks")
        preferences.set("unchanged", forKey: "unrelatedPreference")
        for target in [indexes, mapping, outside] {
            try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
            try Data("keep".utf8).write(to: target.appendingPathComponent("cache-data"))
        }
        for sentinel in sentinels {
            try FileManager.default.createDirectory(at: sentinel.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data("keep".utf8).write(to: sentinel)
        }
    }

    deinit { preferences.removePersistentDomain(forName: suite) }
    func request() { preferences.set(true, forKey: MappingCacheReset.pendingKey) }
    func perform() throws -> Bool { try MappingCacheReset.performIfRequested(preferences: preferences, indexes: indexes, mapping: mapping) }
    func exists(_ url: URL) -> Bool { FileManager.default.fileExists(atPath: url.path) }

    func requireCacheFiles() throws {
        for target in [indexes, mapping] {
            try require(exists(target.appendingPathComponent("cache-data")), "Unrequested reset removed cached files")
        }
    }

    func requireOutsideFiles() throws {
        try require(try Data(contentsOf: outside.appendingPathComponent("cache-data")) == Data("keep".utf8), "Reset followed a symlink outside its cache paths")
    }

    func requireSentinels() throws {
        try requireOutsideFiles()
        for sentinel in sentinels {
            try require(try Data(contentsOf: sentinel) == Data("keep".utf8), "Reset removed a sibling file")
        }
    }

    func requirePreferences(pending: Bool, lastRoot: Bool) throws {
        try require(preferences.bool(forKey: MappingCacheReset.pendingKey) == pending, "Reset marker changed at the wrong boundary")
        try require(preferences.string(forKey: "lastIndexedRoot") == (lastRoot ? "/Users/test/Documents" : nil), "Last location changed before successful reset")
        try require(preferences.stringArray(forKey: "folderPlaces") == places, "Reset changed Places")
        try require(preferences.bool(forKey: "SUEnableAutomaticChecks"), "Reset changed update preferences")
        try require(preferences.string(forKey: "unrelatedPreference") == "unchanged", "Reset changed an unrelated preference")
    }
}

private struct CheckFailure: Error { let message: String }
private func require(_ condition: @autoclosure () throws -> Bool, _ message: String) throws {
    guard try condition() else { throw CheckFailure(message: message) }
}
private func expectFailure(_ action: () throws -> Void) throws {
    var failed = false
    do { try action() } catch { failed = true }
    try require(failed, "Unsafe cache path was accepted")
}
