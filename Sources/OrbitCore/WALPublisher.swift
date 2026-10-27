import CryptoKit
import Darwin
import Foundation

// Publish a complete staged snapshot under the root mutation lease.
enum MirrorPublisher {
    enum Failure: Error { case cannotResumeAfterFailedPublication(Error, Error) }
    static func publish(_ stage: DurableCrawler, checkpoint: StageCheckpoint,
                        directory: URL, elapsed: TimeInterval, expected: SnapshotExpectation, expectedGeneration: String? = nil,
                        publicationFence: (((() throws -> Void)) throws -> Void)? = nil,
                        boundary: (String) -> Void = { _ in }, checkpointTimeout: Int32 = 5_000,
                        cloner: (String, String) -> Int32 = { clonefile($0, $1, 0) }) throws -> IndexSnapshot {
        func checkCancellation() throws {
            if stage.cancellation.isCancelled || Task<Never, Never>.isCancelled { throw CancellationError() }
        }
        try checkCancellation()
        try stage.root.validate(url: stage.rootURL)
        let lockDirectory = IndexLocations.locks(for: directory)
        try fmCreate(lockDirectory)
        let mutationLease = try RootMutationLease(directory: lockDirectory, root: stage.rootURL, cancellation: stage.cancellation)
        defer { withExtendedLifetime(mutationLease) {} }
        try checkCancellation()
        if let expectedGeneration, try mutationLease.invalidationGeneration() != expectedGeneration { throw SnapshotExpectation.Failure.changed }
        var effectiveExpected = expected
        let current = try SnapshotExpectation.capture(directory: directory, root: stage.rootURL)
        if current == .absent, expected != .absent, expectedGeneration != nil {
            try stage.store.rebaseExpectation(root: stage.rootPath, from: expected, to: .absent)
            effectiveExpected = .absent
        } else if current != expected { throw SnapshotExpectation.Failure.changed }
        let prepared = try stage.store.sealMirror(rootURL: stage.rootURL, checkpoint: checkpoint, elapsed: elapsed)
        try checkCancellation()
        try stage.store.checkpointForClone(timeoutMS: checkpointTimeout, cancellation: stage.cancellation)
        do {
        try checkCancellation()
        try fmCreate(directory)
        let digest = SHA256.hash(data: Data(stage.rootURL.path.utf8)).map { String(format: "%02x", $0) }.joined()
        let destination = directory.appendingPathComponent(digest + ".sqlite")
        let temporary = directory.appendingPathComponent(".orbit-stage-\(UUID().uuidString).sqlite")
        var createdIdentity: stat?
        defer {
            if let createdIdentity {
                var current = stat()
                if lstat(temporary.path, &current) == 0, current.st_dev == createdIdentity.st_dev,
                   current.st_ino == createdIdentity.st_ino { _ = unlink(temporary.path) }
            }
        }
        if cloner(stage.store.databasePath, temporary.path) != 0 {
            var info = stat()
            guard lstat(temporary.path, &info) != 0 && errno == ENOENT else {
                throw ListingStage.Failure.sqlite("stage copy target already exists")
            }
            try FileManager.default.copyItem(atPath: stage.store.databasePath, toPath: temporary.path)
        }
        var copied = stat()
        guard lstat(temporary.path, &copied) == 0, (copied.st_mode & S_IFMT) == S_IFREG,
              copied.st_uid == geteuid(), copied.st_nlink == 1, (copied.st_mode & 0o777) == 0o600 else {
            throw ListingStage.Failure.sqlite("unsafe stage copy")
        }
        createdIdentity = copied
        boundary("cloned")
        #if ORBIT_TESTING
        DiskIndexStore.testBeforePublish?()
        #endif
        try checkCancellation()
        try stage.root.validate(url: stage.rootURL)
        try mutationLease.validate()
        try effectiveExpected.verify(directory: directory, root: stage.rootURL)
        if let expectedGeneration, try mutationLease.invalidationGeneration() != expectedGeneration { throw SnapshotExpectation.Failure.changed }
        try checkCancellation()
        let renameSnapshot = {
            try stage.cancellation.commitIfNotCancelled {
                guard !Task<Never, Never>.isCancelled else { throw CancellationError() }
                guard rename(temporary.path, destination.path) == 0 else {
                    throw ListingStage.Failure.sqlite("stage publish failed: \(String(cString: strerror(errno)))")
                }
            }
        }
        if let publicationFence { try publicationFence(renameSnapshot) }
        else { try renameSnapshot() }
        boundary("renamed")
        // The saved snapshot is already committed; a failed marker only postpones stage cleanup.
        try? stage.store.markPublished(id: prepared.id)
        return IndexSnapshot(id: prepared.id, rootURL: prepared.rootURL, databaseURL: destination,
            scannedAt: prepared.scannedAt, elapsed: prepared.elapsed, skippedCount: prepared.skippedCount,
            issues: prepared.issues, eventID: prepared.eventID, eventIdentity: prepared.eventIdentity)
        } catch {
            let publicationError = error
            do { try stage.store.resumeWAL() }
            catch { throw Failure.cannotResumeAfterFailedPublication(publicationError, error) }
            throw publicationError
        }
    }
    private static func fmCreate(_ directory: URL) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
    }
}
