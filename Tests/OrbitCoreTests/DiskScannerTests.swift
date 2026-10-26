import Darwin
import Foundation
import OrbitCore

final class DiskScannerTests: CheckCase {
    var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("OrbitDisk-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try FileManager.default.removeItem(at: root)
    }

    func write(_ name: String, bytes: Int) throws {
        let url = root.appendingPathComponent(name)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data(repeating: 0x41, count: bytes).write(to: url)
    }

    func testHierarchyIncludesHiddenFilesAndEmptyDirectories() throws {
        try write("large.bin", bytes: 8192)
        try write("sub/small.txt", bytes: 47)
        try write(".hidden", bytes: 3)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("empty"), withIntermediateDirectories: true)
        let result = try DiskScanner().scan(root)
        checkEqual(result.root.size, 8242)
        checkEqual(result.root.fileCount, 3)
        checkEqual(result.root.children.map(\.name), ["large.bin", "sub", ".hidden", "empty"])
        checkEqual(result.root.children.last?.children.count, 0)
        checkEqual(result.root.children.last?.isAccessible, true)
        checkEqual(result.skippedCount, 0)
        let ordinaryFile = try checkUnwrap(result.root.children.first(where: { $0.name == "large.bin" }))
        checkNotNil(ordinaryFile.fileIdentity)
        checkEqual(ordinaryFile.rawAllocatedSize, ordinaryFile.allocatedSize)
        var rootInfo = stat()
        checkEqual(lstat(root.path, &rootInfo), 0)
        checkEqual(result.root.fileIdentity, "\(rootInfo.st_dev):\(rootInfo.st_ino)")
        checkEqual(result.root.rawAllocatedSize, 0)
        let sub = try checkUnwrap(result.root.children.first(where: { $0.name == "sub" }))
        checkNotNil(sub.fileIdentity)
        try FileManager.default.moveItem(at: sub.url, to: root.appendingPathComponent("moved-sub"))
        try FileManager.default.createDirectory(at: sub.url, withIntermediateDirectories: false)
        let replaced = try DiskScanner().scan(root).root.children.first(where: { $0.name == "sub" })
        checkTrue(replaced?.fileIdentity != sub.fileIdentity, "A replacement folder must have a different identity")
    }

    func testDoesNotFollowSymbolicLinks() throws {
        try write("folder/file", bytes: 900)
        try FileManager.default.createSymbolicLink(atPath: root.appendingPathComponent("folder/loop").path, withDestinationPath: root.path)
        try FileManager.default.createSymbolicLink(atPath: root.appendingPathComponent("broken").path, withDestinationPath: "missing")
        let result = try DiskScanner().scan(root)
        let folder = try checkUnwrap(result.root.children.first(where: { $0.name == "folder" }))
        let link = try checkUnwrap(folder.children.first(where: { $0.name == "loop" }))
        checkTrue(link.isSymbolicLink)
        checkFalse(link.isDirectory)
        checkTrue(link.children.isEmpty)
        checkEqual(result.root.fileCount, 3)
        checkEqual(result.root.size, 900 + Int64(root.path.utf8.count) + 7)
        checkEqual(result.skippedCount, 0)
    }

    func testHardLinksRetainLogicalSizeAndCountAllocationOnce() throws {
        try write("original", bytes: 8192)
        try FileManager.default.linkItem(at: root.appendingPathComponent("original"), to: root.appendingPathComponent("copy"))
        let earlierFolder = root.appendingPathComponent("a-folder")
        try FileManager.default.createDirectory(at: earlierFolder, withIntermediateDirectories: true)
        try FileManager.default.linkItem(at: root.appendingPathComponent("original"), to: earlierFolder.appendingPathComponent("z-link"))
        try FileManager.default.linkItem(at: root.appendingPathComponent("original"), to: root.appendingPathComponent("b-link"))
        let result = try DiskScanner().scan(root)
        var original = stat()
        checkEqual(lstat(root.appendingPathComponent("original").path, &original), 0)
        checkEqual(result.root.fileCount, 4)
        checkEqual(result.root.size, 32768)
        checkEqual(result.root.allocatedSize, original.st_blocks * 512)
        checkEqual(result.root.children.filter { $0.allocatedSize == 0 }.count, 3)
        let firstFolder = try checkUnwrap(result.root.children.first(where: { $0.name == "a-folder" }))
        let allocationOwner = try checkUnwrap(firstFolder.children.first)
        let duplicate = try checkUnwrap(result.root.children.first(where: { $0.name == "original" }))
        checkEqual(allocationOwner.allocatedSize, original.st_blocks * 512)
        checkEqual(duplicate.allocatedSize, 0)
        checkEqual(duplicate.rawAllocatedSize, original.st_blocks * 512)
        checkEqual(allocationOwner.fileIdentity, duplicate.fileIdentity)
        checkEqual(duplicate.fileIdentity, "\(original.st_dev):\(original.st_ino)")
    }

    func testSparseFileSeparatesLogicalAndAllocatedSizes() throws {
        let path = root.appendingPathComponent("sparse").path
        let descriptor = open(path, O_CREAT | O_WRONLY, 0o600)
        checkGreaterThanOrEqual(descriptor, 0)
        defer { close(descriptor) }
        checkEqual(ftruncate(descriptor, 64 * 1024 * 1024), 0)
        let result = try DiskScanner().scan(root)
        checkEqual(result.root.size, 64 * 1024 * 1024)
        checkLessThan(result.root.allocatedSize, result.root.size)
    }

    func testBulkMatchesFallbackAcrossMultipleBatches() throws {
        for index in 0..<3000 { try write("folder/item-\(index).txt", bytes: index % 73) }
        try write("unicode/日本語 café 😀.txt", bytes: 73)
        try FileManager.default.createSymbolicLink(atPath: root.appendingPathComponent("link").path, withDestinationPath: "folder")
        let bulk = try DiskScanner().scan(root)
        let fallback = try DiskScanner(useBulkEnumeration: false).scan(root)
        func compare(_ left: DiskNode, _ right: DiskNode) {
            checkEqual(left.id, right.id)
            checkEqual(left.size, right.size, left.id)
            checkEqual(left.allocatedSize, right.allocatedSize, left.id)
            checkEqual(left.rawAllocatedSize, right.rawAllocatedSize, left.id)
            checkEqual(left.fileIdentity, right.fileIdentity, left.id)
            checkEqual(left.isSymbolicLink, right.isSymbolicLink)
            checkEqual(left.children.count, right.children.count)
            for (a, b) in zip(left.children, right.children) { compare(a, b) }
        }
        compare(bulk.root, fallback.root)
        checkEqual(bulk.skippedCount, 0)
        checkEqual(bulk.root.fileCount, 3002)
        let descriptor = open(root.appendingPathComponent("folder").path, O_RDONLY | O_DIRECTORY)
        checkGreaterThanOrEqual(descriptor, 0)
        defer { close(descriptor) }
        let direct = try BulkDirectory.read(descriptor, url: root.appendingPathComponent("folder"),
            cancellation: ScanCancellation(), progress: { _, _ in }, issue: { _ in })
        checkEqual(direct.count, 3000, "Bulk API must work without silently falling back")
        let cancellableDescriptor = open(root.appendingPathComponent("folder").path, O_RDONLY | O_DIRECTORY)
        checkGreaterThanOrEqual(cancellableDescriptor, 0)
        defer { close(cancellableDescriptor) }
        let cancellation = ScanCancellation()
        checkThrowsError(try BulkDirectory.read(cancellableDescriptor, url: root.appendingPathComponent("folder"),
            cancellation: cancellation, progress: { _, _ in cancellation.cancel() }, issue: { _ in })) {
                checkTrue($0 is CancellationError)
            }
    }

    func testResourceForkAllocationMatchesFallback() throws {
        try write("forked", bytes: 77)
        let forkPath = root.appendingPathComponent("forked/..namedfork/rsrc").path
        let descriptor = open(forkPath, O_WRONLY | O_CREAT, 0o600)
        checkGreaterThanOrEqual(descriptor, 0)
        guard descriptor >= 0 else { return }
        let data = Data(repeating: 65, count: 16_384)
        let written = data.withUnsafeBytes { Darwin.write(descriptor, $0.baseAddress!, $0.count) }
        close(descriptor)
        checkEqual(written, data.count)
        let bulk = try DiskScanner().scan(root)
        let fallback = try DiskScanner(useBulkEnumeration: false).scan(root)
        checkEqual(bulk.root.size, 77)
        checkEqual(bulk.root.allocatedSize, fallback.root.allocatedSize)
    }

    func testCancellationBeforeAndDuringScan() throws {
        let cancelled = ScanCancellation()
        cancelled.cancel()
        checkThrowsError(try DiskScanner().scan(root, cancellation: cancelled)) {
            checkTrue($0 is CancellationError)
        }
        try write("one", bytes: 1)
        let during = ScanCancellation()
        checkThrowsError(try DiskScanner().scan(root, cancellation: during, progress: { _ in during.cancel() })) {
            checkTrue($0 is CancellationError)
        }
    }

    func testMissingFileRootAndSymlinkRootAreRejected() throws {
        checkThrowsError(try DiskScanner().scan(URL(string: "https://example.com/folder")!))
        checkThrowsError(try DiskScanner().scan(root.appendingPathComponent("missing")))
        try write("file", bytes: 12)
        checkThrowsError(try DiskScanner().scan(root.appendingPathComponent("file")))
        try FileManager.default.createSymbolicLink(atPath: root.appendingPathComponent("link").path, withDestinationPath: root.path)
        checkThrowsError(try DiskScanner().scan(root.appendingPathComponent("link")))
    }

    func testUnreadableChildIsReportedAndUnreadableRootThrows() throws {
        guard getuid() != 0 else { throw CheckFailure("Permission test requires a non-root user") }
        let locked = root.appendingPathComponent("locked")
        try FileManager.default.createDirectory(at: locked, withIntermediateDirectories: true)
        checkEqual(chmod(locked.path, 0), 0)
        defer { chmod(locked.path, 0o700) }
        let result = try DiskScanner().scan(root)
        checkEqual(result.skippedCount, 1)
        checkTrue(result.issues.first?.contains("locked") == true)
        checkEqual(result.root.children.first(where: { $0.name == "locked" })?.isAccessible, false)
        checkTrue(result.root.isAccessible)
        checkThrowsError(try DiskScanner().scan(locked))
    }

    func testProgressFinishesWithAccurateTotals() throws {
        try write("folder/file", bytes: 42)
        let capture = ProgressCapture()
        let result = try DiskScanner().scan(root, progress: { capture.append($0) })
        checkEqual(capture.values.first?.itemCount, 0)
        checkEqual(capture.values.last?.itemCount, 2)
        checkEqual(capture.values.last?.bytesScanned, result.root.size)
        checkLessThanOrEqual(capture.values.count, 3)
    }
}

private final class ProgressCapture: @unchecked Sendable {
    var values: [ScanProgress] = []
    func append(_ progress: ScanProgress) { values.append(progress) }
}
