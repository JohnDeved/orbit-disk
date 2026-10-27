import CryptoKit
import Darwin
import Foundation
#if ASYNC_CHECKS_SHARED_MODULE
@testable import OrbitCore
#endif

enum DiscoveryHotPathChecks {
    private struct StopListing: Error {}

    static func run(_ base: URL) throws {
        let fixture = try AsyncFixture(base, "reusable-bulk-buffer")
        let wide = try fixture.directory("wide"), small = try fixture.directory("small")
        let empty = try fixture.directory("empty")
        for index in 0..<260 { try fixture.file("wide/file-\(index)", size: index + 1) }
        let owner = try fixture.file("small/が-é-quoted'file", size: 4097)
        try FileManager.default.linkItem(at: owner, to: small.appendingPathComponent("linked"))
        try FileManager.default.createSymbolicLink(atPath: small.appendingPathComponent("symbolic").path,
                                                  withDestinationPath: "../wide")
        let buffer = StreamingBulkDirectory.Buffer()
        for directory in [wide, empty, small, wide] {
            let native = try list(directory, buffer: buffer)
            let fallback = try list(directory, buffer: buffer, fallback: true)
            try require(native.count == fallback.count, "Reused native buffer changed the listing count")
            let expected = Dictionary(uniqueKeysWithValues: fallback.map { ($0.name, $0) })
            for actual in native {
                guard let other = expected[actual.name] else {
                    throw AsyncCheckFailure(description: "Reused native buffer retained an earlier filename")
                }
                try require(actual.device == other.device && actual.inode == other.inode
                    && actual.isDirectory == other.isDirectory && actual.isSymbolicLink == other.isSymbolicLink
                    && actual.isMountPoint == other.isMountPoint && actual.linkCount == other.linkCount
                    && actual.size == other.size && actual.allocatedSize == other.allocatedSize,
                    "Native and fallback metadata differ with a reused buffer")
            }
        }
        do {
            _ = try list(wide, buffer: buffer, stopAfterBatch: true)
            throw AsyncCheckFailure(description: "Listing callback did not interrupt discovery")
        } catch is StopListing {}
        try require(try list(wide, buffer: buffer).count == 260, "Interrupted listing damaged the reusable buffer")
        try cacheKeyCompatibility(fixture)
        print("PASS: native buffer reuse, fallback parity, interruption and existing cache-key compatibility")
    }

    private static func list(_ directory: URL, buffer: StreamingBulkDirectory.Buffer,
                             fallback: Bool = false, stopAfterBatch: Bool = false) throws -> [FileMetadata] {
        let descriptor = open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        try require(descriptor >= 0, "Cannot open reusable-buffer fixture")
        defer { close(descriptor) }
        var records: [FileMetadata] = [], issues: [String] = []
        try StreamingBulkDirectory.stream(descriptor, url: directory, cancellation: ScanCancellation(),
            progress: { _, _ in }, issue: { issues.append($0) }, forceFallback: fallback, buffer: buffer) {
                if stopAfterBatch { throw StopListing() }
                records.append(contentsOf: $0)
            }
        try require(issues.isEmpty, "Reusable-buffer fixture reported an enumeration issue")
        return records
    }

    private static func cacheKeyCompatibility(_ fixture: AsyncFixture) throws {
        let store = DiskIndexStore(directory: fixture.cache)
        try FileManager.default.createDirectory(at: fixture.cache, withIntermediateDirectories: true)
        for name in ["wide", "small/../が-é-quoted'folder"] {
            let root = fixture.root.appendingPathComponent(name, isDirectory: true)
            let legacyName = SHA256.hash(data: Data(root.standardizedFileURL.path.utf8))
                .map { String(format: "%02x", $0) }.joined() + ".sqlite"
            let file = fixture.cache.appendingPathComponent(legacyName)
            try require(!store.hasSavedSnapshotFile(for: root), "Cache-key fixture unexpectedly exists")
            try Data().write(to: file)
            try require(store.hasSavedSnapshotFile(for: root), "Cache lookup missed an existing key or a newly published map")
            try FileManager.default.removeItem(at: file)
            try require(!store.hasSavedSnapshotFile(for: root), "Cache lookup retained a removed map")
        }
    }
}
