import Darwin
import Foundation

@main struct BootVolumeChecks {
    struct Failure: Error { let message: String }
    static func require(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
        if try !value() { throw Failure(message: message) }
    }
    static func identity(_ path: String) throws -> DirectoryIdentity {
        var value = stat()
        guard lstat(path, &value) == 0 else { throw Failure(message: "Cannot read fixture identity: \(path)") }
        return DirectoryIdentity(device: value.st_dev, inode: value.st_ino)
    }
    static func record(_ path: String, mount: Bool = false) throws -> FileMetadata {
        let metadata = try FileMetadata.read(URL(fileURLWithPath: path))
        return FileMetadata(name: metadata.name, device: metadata.device, inode: metadata.inode,
            isDirectory: metadata.isDirectory, isSymbolicLink: metadata.isSymbolicLink, isMountPoint: mount,
            linkCount: metadata.linkCount, size: metadata.size, allocatedSize: metadata.allocatedSize)
    }
    static func listing(_ path: String, streaming: Bool, fallback: Bool = false) throws -> [FileMetadata] {
        let url = URL(fileURLWithPath: path), fd = open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw Failure(message: "Cannot open fixture directory") }
        defer { close(fd) }
        var issues: [String] = []
        var records: [FileMetadata] = []
        if streaming {
            try StreamingBulkDirectory.stream(fd, url: url, cancellation: ScanCancellation(), progress: { _, _ in },
                issue: { issues.append($0) }, forceFallback: fallback, batch: { records.append(contentsOf: $0) })
        } else {
            records = try BulkDirectory.read(fd, url: url, cancellation: ScanCancellation(), progress: { _, _ in }, issue: { issues.append($0) })
        }
        try require(!issues.contains(where: { $0.hasPrefix("/Users:") || $0.hasPrefix("/Applications:") }),
                    "A readable firmlink was rejected: \(issues)")
        return records
    }
    static func main() throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1])
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        let localIdentity = try identity(base.path), local = VolumeTraversalScope(rootURL: base, rootIdentity: localIdentity)
        let child = Data(base.appendingPathComponent("child").path.utf8)
        try require(local.allows(path: child, device: localIdentity.device, isMountPoint: false), "Normal same-volume child denied")
        try require(!local.allows(path: child, device: localIdentity.device, isMountPoint: true), "Ordinary nested mount admitted")
        try require(!local.allows(path: child, device: localIdentity.device &+ 1, isMountPoint: false), "Unrelated device admitted")
        try require(!local.allowsIdentity(path: Data((base.path + "-other").utf8), identity: localIdentity), "Sibling byte-prefix admitted")
        print("PASS: ordinary scope preserves device, mount and path boundaries")

        let rootURL = URL(fileURLWithPath: "/"), rootIdentity = try identity("/"), dataPath = "/System/Volumes/Data"
        let scope = VolumeTraversalScope(rootURL: rootURL, rootIdentity: rootIdentity)
        guard let dataIdentity = try? identity(dataPath),
              scope.allows(path: Data(dataPath.utf8), device: dataIdentity.device, isMountPoint: true) else {
            print("SKIP: boot-volume checks require a verified split System/Data layout")
            return
        }
        let usersIdentity = try identity("/Users")
        try require(scope.excludes(path: Data((dataPath + "/Users").utf8)), "Physical Users alias was not pruned")
        try require(scope.excludes(path: Data((dataPath + "/Users/example/file").utf8)), "Duplicate descendants were not pruned")
        try require(!scope.excludes(path: Data((dataPath + "/Users-extra").utf8)), "Unaliased Data sibling was pruned")
        try require(!scope.allows(path: Data("/System/Volumes/VM".utf8), device: rootIdentity.device, isMountPoint: true), "Another system volume was admitted")
        try require(!scope.allowsMount(path: Data("/Users".utf8), mount: "/Volumes/unrelated"), "Unrelated same-device mount admitted")
        try require(!scope.allowsMount(path: Data("/Users-extra".utf8), mount: dataPath), "Unverified native Data route admitted")
        try require(!scope.allowsIdentity(path: Data(dataPath.utf8), identity: DirectoryIdentity(device: dataIdentity.device, inode: dataIdentity.inode &+ 1)), "Replaced Data endpoint admitted")

        let secureRoot = try SecureRoot(url: rootURL), cursor = try SecureDirectoryCursor(root: secureRoot, rootURL: rootURL)
        for (streaming, fallback) in [(false, false), (true, false), (true, true)] {
            let records = try listing("/", streaming: streaming, fallback: fallback)
            for name in ["Users", "Applications", "Library", "private"] {
                guard let observed = records.first(where: { $0.name == name }) else { throw Failure(message: "Missing root entry \(name)") }
                let actual = try identity("/" + name)
                try require(DirectoryIdentity(device: observed.device, inode: observed.inode) == actual,
                            "Bulk firmlink identity differs from the opened target: \(name)")
                let descriptor = try cursor.openDirectory([DirectoryComponent(name: name, expected: actual)])
                close(descriptor)
            }
        }
        do {
            let descriptor = try cursor.openDirectory([DirectoryComponent(name: "Users", expected: DirectoryIdentity(device: usersIdentity.device, inode: usersIdentity.inode &+ 1))])
            close(descriptor)
            throw Failure(message: "Secure cursor accepted a stale firmlink identity")
        } catch SecureTraversalError.identityChanged {}
        print("PASS: bulk, streaming and fallback firmlink identities match secure opens")

        let rootID = "\(rootIdentity.device):\(rootIdentity.inode)"
        let home = FileManager.default.homeDirectoryForCurrentUser
        try require(try MappingCoverage.canReach(root: rootURL, target: home, expectedRootIdentity: rootID), "Boot coverage cannot reach the native home directory")
        let documents = home.appendingPathComponent("Documents")
        if let documentsIdentity = try? identity(documents.path) {
            try require(try MappingCoverage.canReach(root: rootURL, target: documents, expectedRootIdentity: rootID,
                expectedTargetIdentity: "\(documentsIdentity.device):\(documentsIdentity.inode)"), "Boot coverage cannot reach Documents")
        }
        try require(try !MappingCoverage.canReach(root: rootURL, target: URL(fileURLWithPath: dataPath + "/Users"), expectedRootIdentity: rootID), "Coverage claimed a pruned physical alias")
        try require(try !MappingCoverage.canReach(root: rootURL, target: URL(fileURLWithPath: "/dev"), expectedRootIdentity: rootID), "Coverage crossed an unrelated mount")
        let nested = URL(fileURLWithPath: "/System/Library/Caches")
        if (try? identity(nested.path)) != nil {
            func tree(_ url: URL, descendants: [String]) throws -> DiskNode {
                let value = try identity(url.path)
                let children: [DiskNode]
                if let next = descendants.first { children = [try tree(url.appendingPathComponent(next), descendants: Array(descendants.dropFirst()))] }
                else { children = [] }
                return DiskNode(url: url, name: url.lastPathComponent.isEmpty ? "/" : url.lastPathComponent,
                    isDirectory: true, size: 0, allocatedSize: 0, fileCount: 0, children: children,
                    fileIdentity: "\(value.device):\(value.inode)")
            }
            let coverageCache = base.appendingPathComponent("coverage")
            let coverageStore = DiskIndexStore(directory: coverageCache)
            _ = try coverageStore.save(ScanResult(root: tree(rootURL, descendants: ["System", "Library", "Caches"]),
                elapsed: 0, skippedCount: 0, issues: []), eventID: 0, eventIdentity: "boot-fixture")
            let coverage = try MappingCoverage.find(for: nested, cache: coverageCache)
            try require(coverage?.rootURL.path == "/", "Coverage stopped at a System ancestor before the unified boot map")
        }
        print("PASS: native home/Documents and nested firmlink coverage cross only verified boot routes")

        let token = ScanCancellation(), inventory = LiveMappingInventory(root: rootURL, identity: rootIdentity, cancellation: token)
        let store = try ListingStage(path: base.appendingPathComponent("boot.sqlite").path, mirror: true)
        defer { store.close() }
        try store.preserveScanStart(root: Data("/".utf8), identity: "boot-fixture", eventID: 0)
        try store.registerDirectory(path: Data("/".utf8), device: rootIdentity.device, inode: rootIdentity.inode)
        try store.initializeMirrorRoot(url: rootURL, identity: rootIdentity)
        var sequence: Int64 = 0
        func append(_ path: String, _ records: [FileMetadata]) throws {
            let parent = Data(path.utf8), parentIdentity = try identity(path)
            inventory.begin(parent, identity: parentIdentity)
            sequence += 1
            inventory.append(PendingInventoryBatch(sequence: sequence, parentPath: parent, parentIdentity: parentIdentity,
                components: [], records: records, completion: PendingListingCompletion(issueCount: 0, issues: []), failure: nil))
            _ = try store.append(parent: parent, epoch: nil, records: records, rootDevice: rootIdentity.device)
        }
        func file(_ name: String, inode: UInt64, size: Int64) -> FileMetadata {
            FileMetadata(name: name, device: dataIdentity.device, inode: inode, isDirectory: false, isSymbolicLink: false,
                         isMountPoint: false, linkCount: 1, size: size, allocatedSize: size)
        }
        try append("/", [record("/Users"), record("/System")])
        try append("/Users", [file("native-fixture-file", inode: 900_000_001, size: 25)])
        try append("/System", [record("/System/Volumes")])
        try append("/System/Volumes", [record(dataPath, mount: true)])
        try require(try store.directory(path: Data(dataPath.utf8)) != nil, "Data mount did not enter the durable directory queue")
        try append(dataPath, [record(dataPath + "/Users"), file("unaliased-fixture-file", inode: 900_000_002, size: 17)])
        try require(try store.directory(path: Data((dataPath + "/Users").utf8)) == nil, "Duplicate physical Users was queued")
        let view = try inventory.view(path: dataPath, removals: [], cancellation: token)
        try require(view?.folder.isAccessible == true && view?.root.fileCount == 2 && view?.root.size == 42,
                    "Data leftovers were inaccessible, missing, or double-counted in the live view")
        try require(try inventory.fileCount(removals: [], cancellation: token) == 2, "Count-only inventory disagreed on Data leftovers")
        let totals = try store.folderTotals(path: Data("/".utf8))
        try require(totals?.count == 2 && totals?.logical == 42, "Durable root totals lost or duplicated boot data")
        let snapshot = try store.previewMirror(rootURL: rootURL, checkpoint: StageCheckpoint(rootPath: "/", identity: "boot-fixture", eventID: 0))
        let savedData = try DiskIndexStore(directory: base).node(path: dataPath, snapshot: snapshot)
        try require(savedData?.isAccessible == true && savedData?.size == 17, "Durable Data node was marked inaccessible or lost its unique files")
        print("PASS: live view, count-only inventory and durable queue include unique Data entries once")
    }
}
