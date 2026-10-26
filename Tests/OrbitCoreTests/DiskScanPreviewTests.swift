import Darwin
import Foundation
import OrbitCore

extension DiskScannerTests {
    func testPreviewPublishesRootAndPendingFoldersBeforeDescent() throws {
        try write("a-folder/deep/file", bytes: 4096)
        try write("z-file", bytes: 17)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("empty"), withIntermediateDirectories: true)
        let preview = DiskScanPreview()
        let capture = PreviewCapture()
        let token = ScanCancellation()
        checkNil(preview.view())
        checkEqual(preview.revision, 0)
        checkThrowsError(try DiskScanner().scan(root, cancellation: token, preview: preview) { _ in
            if let view = preview.view() {
                capture.values.append(view)
                if view.folder.children.count == 3 { token.cancel() }
            }
        }) { checkTrue($0 is CancellationError) }
        let first = try checkUnwrap(capture.values.first)
        checkEqual(first.root.scanState, .scanning)
        checkTrue(first.root.children.isEmpty)
        checkFalse(first.root.isScanComplete)
        let listed = try checkUnwrap(preview.view())
        checkEqual(listed.folder.children.map(\.name), ["z-file", "a-folder", "empty"])
        checkEqual(listed.root.size, 17)
        checkEqual(listed.root.fileCount, 1)
        checkEqual(listed.root.scanState, .scanning)
        let pending = try checkUnwrap(preview.view(path: root.appendingPathComponent("a-folder").path))
        checkEqual(pending.folder.scanState, .pending)
        checkTrue(pending.folder.children.isEmpty)
        checkEqual(pending.ancestors.map(\.id), [root.path, pending.folder.id])
        checkNil(preview.view(path: root.appendingPathComponent("z-file").path))
        checkNil(preview.view(path: root.appendingPathComponent("not-discovered").path))
        checkTrue(listed.revision > first.revision)
        checkEqual(preview.view()?.revision, listed.revision, "Reading the preview must not mutate it")

        let completed = try DiskScanner().scan(root, preview: preview)
        let final = try checkUnwrap(preview.view())
        checkTrue(final.root.isScanComplete)
        checkEqual(final.root.size, completed.root.size)
        checkEqual(final.root.children.first(where: { $0.name == "empty" })?.scanState, .complete)
        checkEqual(listed.root.size, 17, "Previously returned views must stay immutable")
    }

    func testPreviewTotalsMatchFinalScanAndFallback() throws {
        try write("a-folder/source", bytes: 8192)
        try FileManager.default.linkItem(at: root.appendingPathComponent("a-folder/source"),
                                        to: root.appendingPathComponent("z-hardlink"))
        try write("unicode/日本語 café 😀", bytes: 73)
        try FileManager.default.createSymbolicLink(atPath: root.appendingPathComponent("loop").path,
                                                 withDestinationPath: root.path)
        let sparse = open(root.appendingPathComponent("sparse").path, O_CREAT | O_WRONLY, 0o600)
        checkGreaterThanOrEqual(sparse, 0)
        checkEqual(ftruncate(sparse, 64 * 1024 * 1024), 0)
        close(sparse)
        let withoutPreview = try DiskScanner().scan(root)
        let bulkPreview = DiskScanPreview()
        let bulk = try DiskScanner().scan(root, preview: bulkPreview)
        let fallbackPreview = DiskScanPreview()
        let fallback = try DiskScanner(useBulkEnumeration: false).scan(root, preview: fallbackPreview)

        func compare(_ preview: DiskScanPreview, _ node: DiskNode) throws {
            let view = try checkUnwrap(preview.view(path: node.id))
            checkEqual(view.folder.size, node.size, node.id)
            checkEqual(view.folder.allocatedSize, node.allocatedSize, node.id)
            checkEqual(view.folder.fileCount, node.fileCount, node.id)
            checkEqual(view.folder.fileIdentity, node.fileIdentity, node.id)
            checkEqual(view.folder.scanState, .complete)
            checkEqual(view.folder.children.count, node.children.count)
            let children = Dictionary(uniqueKeysWithValues: view.folder.children.map { ($0.id, $0) })
            for child in node.children {
                let actual = try checkUnwrap(children[child.id])
                checkEqual(actual.size, child.size, child.id)
                checkEqual(actual.allocatedSize, child.allocatedSize, child.id)
                checkEqual(actual.rawAllocatedSize, child.rawAllocatedSize, child.id)
                checkEqual(actual.fileIdentity, child.fileIdentity, child.id)
                checkEqual(actual.isSymbolicLink, child.isSymbolicLink, child.id)
                checkEqual(actual.scanState, .complete)
                if child.isDirectory { try compare(preview, child) }
            }
        }
        try compare(bulkPreview, bulk.root)
        try compare(fallbackPreview, fallback.root)
        checkEqual(bulk.root.size, withoutPreview.root.size)
        checkEqual(bulk.root.allocatedSize, withoutPreview.root.allocatedSize)
        checkEqual(bulk.root.fileCount, withoutPreview.root.fileCount)
        checkEqual(fallback.root.size, bulk.root.size)
        checkEqual(fallback.root.allocatedSize, bulk.root.allocatedSize)
        let owner = try checkUnwrap(bulkPreview.view(path: root.appendingPathComponent("a-folder").path)?.folder.children.first)
        let duplicate = try checkUnwrap(bulkPreview.view()?.folder.children.first(where: { $0.name == "z-hardlink" }))
        checkTrue(owner.allocatedSize > 0)
        checkEqual(duplicate.allocatedSize, 0)
    }

    func testPreviewHydrationIsBoundedButDirectChildrenRemainAvailable() throws {
        for branch in 0..<4 {
            for leaf in 0..<600 { try write("branch-\(branch)/file-\(leaf)", bytes: 1) }
        }
        try write("deep/one/two/three/four/five/file", bytes: 9)
        let preview = DiskScanPreview()
        let result = try DiskScanner().scan(root, preview: preview)
        let view = try checkUnwrap(preview.view())
        func descendants(_ node: DiskNode) -> Int {
            node.children.reduce(0) { $0 + 1 + descendants($1) }
        }
        checkEqual(view.folder.children.count, result.root.children.count)
        checkLessThanOrEqual(descendants(view.folder) - view.folder.children.count, 2_000)
        let deeper = try checkUnwrap(preview.view(path: root.appendingPathComponent("deep/one/two/three/four/five").path))
        checkEqual(deeper.folder.children.map(\.name), ["file"])
        checkEqual(deeper.ancestors.count, 7)
        checkEqual(deeper.ancestors.first?.id, root.path)
        checkEqual(deeper.ancestors.last?.id, deeper.folder.id)
        checkTrue(deeper.root.children.isEmpty, "Ancestors carry totals without duplicating unrelated root rows")
        checkEqual(deeper.root.size, result.root.size)
        for branch in 0..<4 {
            let folder = try checkUnwrap(preview.view(path: root.appendingPathComponent("branch-\(branch)").path))
            checkEqual(folder.folder.children.count, 600)
            checkEqual(folder.folder.size, 600)
        }
    }

    func testPreviewDistinguishesUnavailableAndEmptyFolders() throws {
        guard getuid() != 0 else { throw CheckFailure("Permission test requires a non-root user") }
        let locked = root.appendingPathComponent("locked")
        let empty = root.appendingPathComponent("empty")
        try FileManager.default.createDirectory(at: locked, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: empty, withIntermediateDirectories: true)
        checkEqual(chmod(locked.path, 0), 0)
        defer { chmod(locked.path, 0o700) }
        let preview = DiskScanPreview()
        let result = try DiskScanner().scan(root, preview: preview)
        checkEqual(result.skippedCount, 1)
        let unavailable = try checkUnwrap(preview.view(path: locked.path)?.folder)
        let available = try checkUnwrap(preview.view(path: empty.path)?.folder)
        checkTrue(unavailable.isScanComplete)
        checkFalse(unavailable.isAccessible)
        checkTrue(available.isScanComplete)
        checkTrue(available.isAccessible)
        checkTrue(available.children.isEmpty)
    }

    func testPreviewOrderingInvalidatesWhenMeasuredChildSizesChange() throws {
        try write("a-folder/nested/large", bytes: 8192)
        try write("z-file", bytes: 17)
        let preview = DiskScanPreview()
        let capture = PreviewCapture()
        let folderPath = root.appendingPathComponent("a-folder").path
        _ = try DiskScanner().scan(root, preview: preview) { _ in
            if let view = preview.view(), view.folder.children.count == 2, capture.values.isEmpty {
                capture.values.append(view)
                if let pending = preview.view(path: folderPath) { capture.values.append(pending) }
            }
        }
        let initial = try checkUnwrap(capture.values.first)
        checkEqual(initial.folder.children.map(\.name), ["z-file", "a-folder"])
        checkEqual(capture.values.last?.folder.scanState, .pending)
        checkTrue(capture.values.last?.folder.children.isEmpty == true)
        let final = try checkUnwrap(preview.view())
        checkEqual(final.folder.children.map(\.name), ["a-folder", "z-file"])
        checkEqual(final.root.size, 8209)
        let folder = try checkUnwrap(preview.view(path: folderPath))
        checkEqual(folder.folder.children.map(\.name), ["nested"], "A listing must invalidate an earlier empty child cache")
        checkEqual(folder.folder.size, 8192)
        checkEqual(preview.view()?.folder.children.map(\.name), ["a-folder", "z-file"])
    }
}

private final class PreviewCapture: @unchecked Sendable {
    var values: [ScanPreviewView] = []
}
