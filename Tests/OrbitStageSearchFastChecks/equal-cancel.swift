import Foundation
import OrbitCore

final class Gate: @unchecked Sendable {
    private let lock = NSLock()
    private let semaphore = DispatchSemaphore(value: 0)
    private var enteredValue = false
    var entered: Bool { lock.lock(); defer { lock.unlock() }; return enteredValue }
    func pause() {
        lock.lock()
        let shouldWait = !enteredValue
        enteredValue = true
        lock.unlock()
        if shouldWait { semaphore.wait() }
    }
    func release() { semaphore.signal() }
}

@main struct Main {
    static func main() async throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1])
        let root = base.appendingPathComponent("root")
        let cache = base.appendingPathComponent(UUID().uuidString)
        let stage = try ResumableStage(cache: cache, root: root,
            current: StageCheckpoint(rootPath: root.path, identity: "test", eventID: 0))
        while try stage.advance(progress: { _ in }) {}
        for cancelTask in [true, false] {
            let gate = Gate(), token = ScanCancellation()
            DiskIndexStore.testEqualSizeSearchSkip = { gate.pause() }
            let worker = Task.detached { try stage.search(query: "file-", cancellation: token, removals: [], limit: 3) }
            for _ in 0..<5000 {
                if gate.entered { break }
                try await Task.sleep(nanoseconds: 1_000_000)
            }
            guard gate.entered else {
                worker.cancel(); token.cancel(); gate.release()
                _ = try? await worker.value
                fatalError("equal-size fast path was not reached")
            }
            if cancelTask { worker.cancel() } else { token.cancel() }
            gate.release()
            do { _ = try await worker.value; fatalError("search did not cancel") }
            catch is CancellationError {}
            DiskIndexStore.testEqualSizeSearchSkip = nil
            print("PASS: \(cancelTask ? "Task" : "token") cancellation after entering equal-size fast path")
        }
        let result = try stage.search(query: "file-", cancellation: ScanCancellation(), removals: [], limit: 3)
        precondition(result.count == 4000 && result.nodes.count == 3)
        print("PASS: subsequent search still works")
    }
}
