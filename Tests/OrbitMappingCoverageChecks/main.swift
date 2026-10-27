@testable import OrbitCore
import Darwin
import Foundation

private struct Failure: Error { let message: String }
private func require(_ value: Bool, _ message: String) throws {
    if !value { throw Failure(message: message) }
}
private func identity(_ url: URL) throws -> String {
    var info = stat()
    guard lstat(url.path, &info) == 0 else { throw Failure(message: "Missing fixture identity") }
    return "\(info.st_dev):\(info.st_ino)"
}

@main struct CoverageChecks {
    static func main() throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).resolvingSymlinksInPath()
        let files = FileManager.default
        func directory(_ path: String) throws -> URL {
            let url = base.appendingPathComponent(path, isDirectory: true)
            try files.createDirectory(at: url, withIntermediateDirectories: true)
            return url
        }
        func stage(_ root: URL, _ cache: URL) throws -> ResumableStage {
            try ResumableStage(cache: cache, root: root,
                               current: StageCheckpoint(rootPath: root.path, identity: "fixture-events", eventID: 0))
        }
        func saved(_ root: URL, _ cache: URL) throws -> IndexSnapshot {
            try DiskIndexStore(directory: cache).save(DiskScanner().scan(root), eventID: 0, eventIdentity: "fixture-events")
        }
        func find(_ target: URL, _ cache: URL) throws -> MappingCoverage? {
            try MappingCoverage.find(for: target, cache: cache)
        }

        let absent = try directory("absent/child"), absentCache = base.appendingPathComponent("absent-cache")
        try require(try find(absent, absentCache) == nil, "An unindexed folder gained coverage")
        try require(!files.fileExists(atPath: absentCache.path), "Discovery created a cache")

        let pendingRoot = try directory("pending"), pendingChild = try directory("pending/one/two")
        let pendingCache = base.appendingPathComponent("pending-cache")
        do {
            let active = try stage(pendingRoot, pendingCache)
            let coverage = try find(pendingChild, pendingCache)
            try require(coverage?.rootURL == pendingRoot && coverage?.source == .stage, "Pending ancestor stage was not found")
            try require(coverage?.requestedIdentity == identity(pendingChild), "Requested identity was lost")
            try require(try find(pendingRoot, pendingCache)?.rootURL == pendingRoot, "Exact stage was not found")
            withExtendedLifetime(active) {}
        }
        try require(try find(pendingChild, pendingCache)?.rootURL == pendingRoot, "Released stage was not found after restart")
        do {
            let child = try stage(pendingChild, pendingCache)
            try require(try find(pendingChild, pendingCache)?.rootURL == pendingChild, "Exact work did not win over ancestor")
            try DiskIndexStore(directory: pendingCache).invalidate(for: pendingChild)
            try require(try find(pendingChild, pendingCache)?.rootURL == pendingRoot, "Invalidated exact stage hid a valid ancestor")
            withExtendedLifetime(child) {}
        }
        print("PASS: pending, stopped and exact-stage coverage without acquiring the writer lease")

        let staleRoot = try directory("stale/child").deletingLastPathComponent()
        let staleChild = staleRoot.appendingPathComponent("child"), staleCache = base.appendingPathComponent("stale-cache")
        do {
            let value = try stage(staleRoot, staleCache)
            _ = try value.advance(progress: { _ in })
            try files.moveItem(at: staleChild, to: staleRoot.appendingPathComponent("old-child"))
            try files.createDirectory(at: staleChild, withIntermediateDirectories: false)
            try require(try find(staleChild, staleCache) == nil, "Stage claimed a known replaced directory")
            withExtendedLifetime(value) {}
        }
        try files.moveItem(at: staleRoot, to: base.appendingPathComponent("old-stale-root"))
        try files.createDirectory(at: staleChild, withIntermediateDirectories: true)
        try require(try find(staleChild, staleCache) == nil, "Stage claimed a replacement root")

        let savedRoot = try directory("saved/child").deletingLastPathComponent()
        let savedChild = savedRoot.appendingPathComponent("child"), savedCache = base.appendingPathComponent("saved-cache")
        _ = try saved(savedRoot, savedCache)
        let coverage = try find(savedChild, savedCache)
        try require(coverage?.rootURL == savedRoot && coverage?.source == .saved, "Saved ancestor directory was not found")
        let later = try directory("saved/later")
        try require(try find(later, savedCache) == nil, "Saved map claimed an unindexed directory")
        try files.moveItem(at: savedChild, to: savedRoot.appendingPathComponent("old-child"))
        try files.createDirectory(at: savedChild, withIntermediateDirectories: false)
        try require(try find(savedChild, savedCache) == nil, "Saved map claimed a replacement child")
        try files.moveItem(at: savedRoot, to: base.appendingPathComponent("old-root"))
        try files.createDirectory(at: savedChild, withIntermediateDirectories: true)
        try require(try find(savedChild, savedCache) == nil, "Saved map claimed a replacement root")
        print("PASS: saved coverage requires both root and requested directory identity")

        let sealedRoot = try directory("sealed/known").deletingLastPathComponent()
        let sealedCache = base.appendingPathComponent("sealed-cache")
        do {
            let complete = try stage(sealedRoot, sealedCache)
            while try complete.advance(progress: { _ in }) {}
            let snapshot = try complete.publish(elapsed: 0)
            try files.removeItem(at: snapshot.databaseURL)
        }
        try require(try find(sealedRoot.appendingPathComponent("known"), sealedCache)?.rootURL == sealedRoot,
                    "Published stage could not restore its known directory after cache purge")
        let unseen = try directory("sealed/new")
        try require(try find(unseen, sealedCache) == nil, "Sealed stage claimed an unindexed new directory")

        let reachRoot = try directory("reach/child").deletingLastPathComponent()
        let reachChild = reachRoot.appendingPathComponent("child"), rootID = try identity(reachRoot)
        let sibling = try directory("reach-other/child")
        try require(try !MappingCoverage.canReach(root: reachRoot, target: sibling, expectedRootIdentity: rootID),
                    "Byte-prefix sibling counted as a descendant")
        let external = try directory("external/child").deletingLastPathComponent()
        try files.createSymbolicLink(at: reachRoot.appendingPathComponent("link"), withDestinationURL: external)
        try require(try !MappingCoverage.canReach(root: reachRoot, target: reachRoot.appendingPathComponent("link/child"), expectedRootIdentity: rootID),
                    "Reachability crossed a symlink")
        try require(try !MappingCoverage.canReach(root: reachRoot, target: reachChild, expectedRootIdentity: "wrong"),
                    "Reachability ignored root identity")
        try require(try !MappingCoverage.canReach(root: reachRoot, target: reachChild, expectedRootIdentity: rootID, expectedTargetIdentity: "wrong"),
                    "Reachability ignored target identity")
        let cancelled = ScanCancellation(); cancelled.cancel()
        do {
            _ = try MappingCoverage.find(for: reachChild, cache: pendingCache, cancellation: cancelled)
            throw Failure(message: "Discovery ignored cancellation")
        } catch is CancellationError {}
        print("PASS: sealed-stage gaps, byte containment, symlink boundaries and cancellation")

        let unsafeRoot = try directory("unsafe"), unsafeCache = base.appendingPathComponent("unsafe-cache")
        do { let value = try stage(unsafeRoot, unsafeCache); withExtendedLifetime(value) {} }
        let stagePath = unsafeCache.appendingPathExtension("staging").appendingPathComponent(".orbit-stages")
            .appendingPathComponent(try StageLease.key(root: unsafeRoot) + ".sqlite")
        try files.moveItem(at: stagePath, to: stagePath.appendingPathExtension("held"))
        try files.createSymbolicLink(at: stagePath, withDestinationURL: stagePath.appendingPathExtension("held"))
        try require(try find(unsafeRoot, unsafeCache) == nil, "Discovery accepted a symlink stage database")
        try files.removeItem(at: stagePath)
        try Data().write(to: stagePath)
        try files.setAttributes([.posixPermissions: 0o600], ofItemAtPath: stagePath.path)
        try require(try find(unsafeRoot, unsafeCache) == nil, "Discovery accepted an empty stage database")
        print("PASS: private stage path validation rejects substituted artifacts")

        if CommandLine.arguments.count == 3 {
            let mounted = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
            try require(try !MappingCoverage.canReach(root: mounted.deletingLastPathComponent(), target: mounted,
                                                      expectedRootIdentity: identity(mounted.deletingLastPathComponent())),
                        "Reachability crossed the supplied mount boundary")
            print("PASS: mounted volume remains a separate mapping root")
        }
    }
}
