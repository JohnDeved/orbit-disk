import Foundation
import OrbitCore
#if canImport(Darwin)
import Darwin
#endif

final class TrashPolicyTests: CheckCase {
    private var fixture: URL!
    private var scanRoot: URL!
    private var home: URL!

    override func setUpWithError() throws {
        fixture = FileManager.default.temporaryDirectory.appendingPathComponent("OrbitTrashPolicy-\(UUID().uuidString)")
        home = fixture.appendingPathComponent("home")
        scanRoot = home.appendingPathComponent("Downloads")
        try FileManager.default.createDirectory(at: scanRoot, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        if let fixture { try FileManager.default.removeItem(at: fixture) }
    }

    func testOrdinaryDescendantIsAllowedButRootAndSiblingAreProtected() throws {
        let file = try makeFile("report.txt")
        checkNil(reason(file))
        checkNotNil(reason(scanRoot))
        let sibling = home.appendingPathComponent("Downloads-old")
        try FileManager.default.createDirectory(at: sibling, withIntermediateDirectories: true)
        checkNotNil(reason(sibling))
        checkNotNil(TrashPolicy.rejectionReason(for: home, scanRoot: fixture, home: home))
    }

    func testSymlinkAndAncestorEscapeAreRejected() throws {
        let file = try makeFile("report.txt")
        let link = scanRoot.appendingPathComponent("report-link")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: file)
        checkNotNil(reason(link))
        let outside = fixture.appendingPathComponent("outside")
        try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
        try Data("outside".utf8).write(to: outside.appendingPathComponent("item"))
        let escaped = scanRoot.appendingPathComponent("escape")
        try FileManager.default.createSymbolicLink(at: escaped, withDestinationURL: outside)
        checkNotNil(reason(escaped.appendingPathComponent("item")))
    }

    func testHiddenDirectoriesTrashAndLibraryAreProtected() throws {
        for folder in [".ssh", ".Trash", ".Trashes"] {
            let directory = scanRoot.appendingPathComponent(folder)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let file = directory.appendingPathComponent("item")
            try Data().write(to: file)
            checkNotNil(reason(directory))
            checkNotNil(reason(file))
        }
        let library = home.appendingPathComponent("Library")
        try FileManager.default.createDirectory(at: library, withIntermediateDirectories: true)
        checkNotNil(TrashPolicy.rejectionReason(for: library, scanRoot: home, home: home))
        checkNil(reason(try makeFile(".DS_Store")))
    }

    func testSystemPathsVolumesAndMissingItemsAreRejected() {
        for path in ["/", "/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc", "/private", "/private/var", "/Volumes/External"] {
            checkNotNil(TrashPolicy.rejectionReason(for: URL(fileURLWithPath: path), scanRoot: URL(fileURLWithPath: "/"), home: home), path)
        }
        checkNotNil(reason(scanRoot.appendingPathComponent("missing")))
        checkNotNil(TrashPolicy.rejectionReason(for: URL(string: "https://example.com/item")!, scanRoot: scanRoot, home: home))
    }

    func testCloudRootsAndResolvedAliasesAreProtected() throws {
        let cloud = home.appendingPathComponent("Library/CloudStorage/Provider", isDirectory: true)
        try FileManager.default.createDirectory(at: cloud, withIntermediateDirectories: true)
        let file = cloud.appendingPathComponent("report.txt")
        try Data("cloud".utf8).write(to: file)
        checkTrue(CloudStoragePolicy.isKnownCloudPath(file.path, home: home))
        checkFalse(CloudStoragePolicy.isKnownCloudPath(home.appendingPathComponent("Library/CloudStorage-old").path, home: home))
        checkEqual(TrashPolicy.rejectionReason(for: file, scanRoot: home, home: home), CloudStoragePolicy.excludedMessage)
        let alias = scanRoot.appendingPathComponent("shared")
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: cloud)
        checkEqual(reason(alias.appendingPathComponent("report.txt")), CloudStoragePolicy.excludedMessage)
        checkEqual(CloudStoragePolicy.exclusionReason(for: cloud.deletingLastPathComponent(), home: home,
                                                      excluding: [cloud], protectContainingRoots: true),
                   CloudStoragePolicy.excludedMessage)
        let lower = home.appendingPathComponent("library/cloudstorage/provider/report.txt")
        if FileManager.default.fileExists(atPath: lower.path) {
            checkEqual(CloudStoragePolicy.exclusionReason(for: lower, home: home, excluding: [cloud],
                                                          protectContainingRoots: false, probe: { _ in false }),
                       CloudStoragePolicy.excludedMessage, "Case-variant paths must match the cloud root's identity")
            let parent = home.appendingPathComponent("library")
            checkEqual(CloudStoragePolicy.exclusionReason(for: parent, home: home, excluding: [cloud],
                                                          protectContainingRoots: true, probe: { _ in false }),
                       CloudStoragePolicy.excludedMessage, "A case-variant containing folder cannot bypass cloud protection")
        }
    }

    func testCloudRootSnapshotUsesOnlyKnownFolderMetadata() throws {
        let desktop = home.appendingPathComponent("Desktop", isDirectory: true)
        let documents = home.appendingPathComponent("Documents", isDirectory: true)
        try FileManager.default.createDirectory(at: desktop, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: documents, withIntermediateDirectories: true)
        var probes: [String] = []
        let roots = CloudStoragePolicy.scanExclusions(home: home) { url in
            probes.append(url.path)
            return url.path == documents.path
        }
        checkEqual(probes, [desktop.path, documents.path])
        checkTrue(roots.contains { $0.path == documents.path })
        checkFalse(roots.contains { $0.path == desktop.path })
        checkTrue(roots.contains { $0.path == home.appendingPathComponent("Library/CloudStorage").path })
        checkTrue(roots.contains { $0.path == home.appendingPathComponent("Library/Mobile Documents").path })
        let unavailable = CloudStoragePolicy.scanExclusions(home: home) { _ in
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(EACCES))
        }
        checkFalse(unavailable.contains { $0.path == documents.path })
    }

    func testCloudMetadataAndDatalessErrorsAreDistinguished() throws {
        let shared = scanRoot.appendingPathComponent("Synced", isDirectory: true)
        let folder = shared.appendingPathComponent("nested", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let file = folder.appendingPathComponent("report.txt")
        try Data("test".utf8).write(to: file)
        let inherited = CloudStoragePolicy.exclusionReason(for: file, home: home, excluding: [], protectContainingRoots: false) {
            $0.path == shared.path
        }
        checkEqual(inherited, CloudStoragePolicy.excludedMessage)
        let local = CloudStoragePolicy.exclusionReason(for: file, home: home, excluding: [], protectContainingRoots: false) { _ in false }
        checkNil(local)
        let denied = CloudStoragePolicy.exclusionReason(for: file, home: home, excluding: [], protectContainingRoots: false) { _ in
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(EACCES))
        }
        checkNil(denied, "Normal inaccessible metadata must not be mislabeled cloud storage")
        let dataless = CloudStoragePolicy.exclusionReason(for: file, home: home, excluding: [], protectContainingRoots: false) { _ in
            throw NSError(domain: NSCocoaErrorDomain, code: NSFileReadUnknownError,
                          userInfo: [NSUnderlyingErrorKey: NSError(domain: NSPOSIXErrorDomain, code: Int(EDEADLK))])
        }
        checkNotNil(dataless)
    }

    func testCloudBatchValidatorReusesAncestorsButRefreshesEachItem() throws {
        let files = try (0..<128).map { try makeFile("item-\($0).txt") }
        var probes: [String: Int] = [:]
        var changedPath: String?
        let probe: (URL) throws -> Bool? = { url in
            probes[url.path, default: 0] += 1
            return url.path == changedPath
        }
        let validator = CloudStoragePolicy.Validator(home: home, excluding: [], protectContainingRoots: true, probe: probe)
        for file in files {
            checkNil(TrashPolicy.rejectionReason(for: file, scanRoot: scanRoot, home: home, cloudValidator: validator))
        }
        let canonicalParent = scanRoot.resolvingSymlinksInPath().path
        checkEqual(probes[canonicalParent], 1, "Sibling validation should inspect shared ancestors only once per batch")
        for file in files { checkEqual(probes[file.resolvingSymlinksInPath().path], 1) }
        changedPath = files[0].resolvingSymlinksInPath().path
        checkEqual(validator.exclusionReason(for: files[0]), CloudStoragePolicy.excludedMessage,
                   "An item must be rechecked even within the same batch")
        changedPath = canonicalParent
        let nextBatch = CloudStoragePolicy.Validator(home: home, excluding: [], protectContainingRoots: true, probe: probe)
        checkEqual(nextBatch.exclusionReason(for: files[1]), CloudStoragePolicy.excludedMessage,
                   "A new batch must not inherit old ancestor classifications")
        checkEqual(CloudStoragePolicy.exclusionReason(for: files[2], home: home, excluding: [],
                                                      protectContainingRoots: true, probe: probe), CloudStoragePolicy.excludedMessage,
                   "The fresh removal check must recheck changed ancestry")
    }

    func testCloudMaterializationPolicyRestoresAfterFailure() throws {
        #if canImport(Darwin)
        let previous = getiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD)
        checkTrue(previous >= 0)
        do {
            try CloudStoragePolicy.withoutMaterialization {
                checkEqual(getiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD),
                           IOPOL_MATERIALIZE_DATALESS_FILES_OFF)
                try CloudStoragePolicy.withoutMaterialization {
                    checkEqual(getiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD),
                               IOPOL_MATERIALIZE_DATALESS_FILES_OFF)
                }
                throw NSError(domain: "CloudPolicyFixture", code: 1)
            }
        } catch {}
        checkEqual(getiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD), previous)
        #endif
        #if ORBIT_TESTING
        let file = try makeFile("ordinary.txt")
        checkNil(reason(file))
        CloudStoragePolicy.testScanExclusions = [scanRoot]
        defer { CloudStoragePolicy.testScanExclusions = nil }
        checkEqual(reason(file), CloudStoragePolicy.excludedMessage, "Removal must recheck changed cloud policy")
        #endif
    }

    private func reason(_ url: URL) -> String? {
        TrashPolicy.rejectionReason(for: url, scanRoot: scanRoot, home: home)
    }

    private func makeFile(_ name: String) throws -> URL {
        let file = scanRoot.appendingPathComponent(name)
        try Data("test".utf8).write(to: file)
        return file
    }
}
