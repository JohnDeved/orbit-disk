import Foundation
import OrbitCore
struct ForcedFailure: Error {}
final class Outcome: @unchecked Sendable { var value: Result<ScanResult, Error>? }
let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).standardizedFileURL
let root = base.appendingPathComponent("core-files", isDirectory: true)
let fm = FileManager.default
try fm.createDirectory(at: root, withIntermediateDirectories: true)
for i in 0..<4_000 { try Data(repeating: UInt8(i % 251), count: i % 9 + 1).write(to: root.appendingPathComponent("file-\(i)")) }
try fm.linkItem(at: root.appendingPathComponent("file-0"), to: root.appendingPathComponent("hardlink"))
try fm.createSymbolicLink(atPath: root.appendingPathComponent("loop").path, withDestinationPath: ".")
func check(_ condition: @autoclosure () -> Bool, _ message: String) { precondition(condition(), message) }
func awaitSignal(_ sem: DispatchSemaphore) { check(sem.wait(timeout: .now() + 20) == .success, "timed out") }
let preview = DiskScanPreview(), arrived = DispatchSemaphore(value: 0), gate = DispatchSemaphore(value: 0), done = DispatchSemaphore(value: 0), outcome = Outcome(), fallbackStarted = DispatchSemaphore(value: 0), fallbackGate = DispatchSemaphore(value: 0)
let scanner = DiskScanner(testBatch: {
    guard let view = preview.view(), view.folder.children.count >= 512 else { return }
    arrived.signal(); gate.wait(); throw ForcedFailure()
}, testFallback: { fallbackStarted.signal(); fallbackGate.wait() })
DispatchQueue.global().async {
    outcome.value = Result { try scanner.scan(root, preview: preview) }
    done.signal()
}
awaitSignal(arrived)
let provisional = preview.view()!
check(provisional.isListing, "batch must be marked provisional")
check(provisional.folder.children.count >= 512, "first batch not published")
check(provisional.folder.children.allSatisfy { !$0.isScanComplete }, "provisional children must not be collectable")
check(!preview.confirmsMissingDirectory(root.appendingPathComponent("not-yet-seen").path), "an unfinished listing cannot confirm a missing folder")
let staleFile = provisional.folder.children.first { !$0.isDirectory && !$0.isSymbolicLink && $0.name.hasPrefix("file-") }!
try FileManager.default.removeItem(at: staleFile.url)
gate.signal(); awaitSignal(fallbackStarted)
check(preview.view()?.folder.children.count == provisional.folder.children.count, "fallback must retain provisional listing")
fallbackGate.signal(); awaitSignal(done)
let fallback = try outcome.value!.get()
let final = preview.view()!
check(!final.isListing && final.folder.isScanComplete, "fallback completion not visible")
check(final.folder.children.count == fallback.root.children.count, "rollback left duplicate/stale children")
check(!final.folder.children.contains { $0.id == staleFile.id }, "rollback left stale file")
check(preview.confirmsMissingDirectory(root.appendingPathComponent("not-yet-seen").path), "a completed listing must confirm a missing folder")
let confirmedFile = final.folder.children.first { !$0.isDirectory && !$0.isSymbolicLink }!
check(preview.confirmsMissingDirectory(confirmedFile.id), "a file replacing a directory must count as missing")
let expected = try DiskScanner(useBulkEnumeration: false).scan(root)
check(expected.root.size == fallback.root.size && expected.root.allocatedSize == fallback.root.allocatedSize && expected.root.fileCount == fallback.root.fileCount, "fallback totals diverged")
let expectedMap = Dictionary(uniqueKeysWithValues: expected.root.children.map { ($0.id, ($0.size, $0.allocatedSize, $0.isSymbolicLink, $0.fileIdentity)) })
for node in fallback.root.children {
    guard let item = expectedMap[node.id] else { fatalError("missing path") }
    check(item.0 == node.size && item.1 == node.allocatedSize && item.2 == node.isSymbolicLink && item.3 == node.fileIdentity, "fallback node mismatch")
}
print("PASS: provisional files pending; vanished file rollback; fallback parity including hardlink and symlink")
let cancelPreview = DiskScanPreview(), token = ScanCancellation(), ready = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0), end = DispatchSemaphore(value: 0), cancelled = Outcome()
let cancelScanner = DiskScanner(testBatch: {
    guard let view = cancelPreview.view(), view.folder.children.count >= 512 else { return }
    ready.signal(); resume.wait()
})
DispatchQueue.global().async { cancelled.value = Result { try cancelScanner.scan(root, cancellation: token, preview: cancelPreview) }; end.signal() }
awaitSignal(ready); token.cancel(); resume.signal(); awaitSignal(end)
if case .failure(let error) = cancelled.value! { check(error is CancellationError, "wrong cancellation error") } else { fatalError("scan should cancel") }
let cancelledView = cancelPreview.view()!
check(cancelledView.isListing && !cancelledView.folder.children.isEmpty && cancelledView.folder.children.allSatisfy { !$0.isScanComplete }, "cancelled map exposes completed provisional entries")
print("PASS: cancelled provisional listing remains browsable and incomplete")

let outer = base.appendingPathComponent("outer", isDirectory: true)
let branch = outer.appendingPathComponent("branch", isDirectory: true)
try fm.createDirectory(at: branch, withIntermediateDirectories: true)
for i in 0..<1_500 { try fm.createDirectory(at: branch.appendingPathComponent("folder-\(i)"), withIntermediateDirectories: true) }
final class ChildCapture: @unchecked Sendable { var child: String? }
let nestedPreview = DiskScanPreview(), captured = ChildCapture()
let nestedScanner = DiskScanner(testBatch: {
    guard let view = nestedPreview.view(path: branch.path), view.folder.children.count == 512 else { return }
    captured.child = view.folder.children.first(where: { $0.isDirectory })?.id
    check(captured.child != nil && view.folder.children.allSatisfy { !$0.isScanComplete }, "nested preview must be pending")
    check(!nestedPreview.confirmsMissingDirectory(captured.child! + "/not-yet-seen"), "a pending parent cannot confirm a missing descendant")
    throw ForcedFailure()
}, testFallback: { throw ForcedFailure() })
let nestedResult = try nestedScanner.scan(outer, preview: nestedPreview)
let inaccessible = nestedResult.root.children.first { $0.id == branch.path }!
check(!inaccessible.isAccessible && inaccessible.children.isEmpty && nestedResult.skippedCount == 1, "failed nested listing not marked inaccessible")
let shown = nestedPreview.view(path: branch.path)!.folder
check(shown.isScanComplete && !shown.isAccessible && shown.children.isEmpty, "preview retained a failed listing")
check(nestedPreview.view(path: captured.child!) == nil, "stale child remained navigable")
check(!nestedPreview.confirmsMissingDirectory(branch.path), "an existing directory is not missing")
check(nestedPreview.confirmsMissingDirectory(captured.child!), "a failed completed listing must confirm a missing child")
print("PASS: failed fallback clears provisional children before completing inaccessible folder")
