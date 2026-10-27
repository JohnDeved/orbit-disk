import Foundation

let rootURL = URL(fileURLWithPath: "/preview-cancellation", isDirectory: true)
let root = DiskNode(url: rootURL, name: "preview-cancellation", isDirectory: true,
                    size: 0, allocatedSize: 0, fileCount: 0, scanState: .pending)
let count = 50_000
let children = (0..<count).map { index in
    DiskNode(url: rootURL.appendingPathComponent("item-\(index)"), name: "item-\(index)", isDirectory: false,
             size: Int64((index * 7919) % 131071), allocatedSize: 1, fileCount: 1)
}
let expected = children.sorted {
    $0.size == $1.size ? $0.name < $1.name : $0.size > $1.size
}.map(\.id)
let preview = DiskScanPreview()
preview.begin(root)
preview.recordListing(root.id, children: children)
let cancelled = ScanCancellation()
cancelled.cancel()
do {
    _ = try preview.view(cancellation: cancelled)
    preconditionFailure("A cancelled preview read must throw")
} catch is CancellationError {} catch { preconditionFailure("Unexpected error: \(error)") }

final class ResultBox: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false
    func recordCancellation() { lock.lock(); cancelled = true; lock.unlock() }
    var wasCancelled: Bool { lock.lock(); defer { lock.unlock() }; return cancelled }
}

func waitSynchronously(_ semaphore: DispatchSemaphore) { semaphore.wait() }
let taskStarted = DispatchSemaphore(value: 0)
let taskContinue = DispatchSemaphore(value: 0)
let taskDone = DispatchSemaphore(value: 0)
let taskResult = ResultBox()
let task = Task.detached {
    taskStarted.signal()
    waitSynchronously(taskContinue)
    do { _ = try preview.view(cancellation: ScanCancellation()) }
    catch is CancellationError { taskResult.recordCancellation() }
    catch { preconditionFailure("Unexpected error: \(error)") }
    taskDone.signal()
}
precondition(taskStarted.wait(timeout: .now() + 10) == .success)
task.cancel()
taskContinue.signal()
precondition(taskDone.wait(timeout: .now() + 10) == .success && taskResult.wasCancelled,
             "Cancelling the detached reader task must stop its preview read")

func interruptRead(checkNumber: Int, label: String) {
    let reached = DispatchSemaphore(value: 0)
    let resume = DispatchSemaphore(value: 0)
    let done = DispatchSemaphore(value: 0)
    let writerStarted = DispatchSemaphore(value: 0)
    let writerDone = DispatchSemaphore(value: 0)
    let token = ScanCancellation()
    let result = ResultBox()
    DispatchQueue.global().async {
        var checks = 0
        do {
            _ = try preview.view(path: nil, checkCancellation: {
                checks += 1
                if checks == checkNumber { reached.signal(); resume.wait() }
                if token.isCancelled { throw CancellationError() }
            })
        } catch is CancellationError { result.recordCancellation() }
        catch { preconditionFailure("Unexpected error: \(error)") }
        done.signal()
    }
    precondition(reached.wait(timeout: .now() + 10) == .success, "Read did not reach \(label)")
    DispatchQueue.global().async {
        writerStarted.signal()
        preview.markScanning(root.id)
        writerDone.signal()
    }
    precondition(writerStarted.wait(timeout: .now() + 10) == .success)
    precondition(writerDone.wait(timeout: .now() + 0.01) == .timedOut,
                 "The scanner writer must wait while hydration holds the lock")
    token.cancel()
    let start = DispatchTime.now().uptimeNanoseconds
    resume.signal()
    precondition(done.wait(timeout: .now() + 10) == .success && result.wasCancelled,
                 "The \(label) read must stop after cancellation")
    precondition(writerDone.wait(timeout: .now() + 10) == .success,
                 "A scanner write waiting for the preview lock must proceed")
    let latency = Double(DispatchTime.now().uptimeNanoseconds - start) / 1_000_000
    print(String(format: "PASS cancelled %@; reader and waiting scanner released in %.2f ms", label, latency))
}

// The first 196 checks copy the 50,000 IDs; check 201 follows the first sort.
interruptRead(checkNumber: 201, label: "first-read ordering before cache")
var reorderingChecks = 0
let afterCancelledSort = try preview.view(path: nil, checkCancellation: { reorderingChecks += 1 })!
precondition(reorderingChecks > 300, "A cancelled sort must not be cached")
precondition(afterCancelledSort.folder.children.map(\.id) == expected,
             "An interrupted sort must not cache a partial order")
precondition(afterCancelledSort.revision == preview.revision && afterCancelledSort.folder.scanState == .scanning)
interruptRead(checkNumber: 100, label: "cached-order hydration")
let afterCachedCancellation = preview.view()!
precondition(afterCachedCancellation.folder.children.map(\.id) == expected,
             "Cancelled hydration must preserve the complete cached order")
print("PASS cancelled preview reads preserve order, revision, and scanner progress")
