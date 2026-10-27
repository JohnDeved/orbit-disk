import AppKit
import Foundation
import OrbitCore

final class Gate: @unchecked Sendable {
    private let lock = NSLock(), semaphore = DispatchSemaphore(value: 0)
    private var claimed = false, blocked = false
    var waiting: Bool { lock.lock(); defer { lock.unlock() }; return blocked }
    func pause() {
        lock.lock(); let wait = !claimed; claimed = true; if wait { blocked = true }; lock.unlock()
        if wait { semaphore.wait(); lock.lock(); blocked = false; lock.unlock() }
    }
    func release() { semaphore.signal() }
}
@main struct Check {
    @MainActor static func wait(_ message: String, _ condition: () -> Bool) async throws {
        for _ in 0..<1_000 { if condition() { return }; try await Task.sleep(nanoseconds: 20_000_000) }
        fatalError("Timed out: \(message)")
    }
    @MainActor static func main() async throws {
        let root = URL(fileURLWithPath: CommandLine.arguments[1]).appendingPathComponent("root")
        let one = root.appendingPathComponent("one"), two = root.appendingPathComponent("two")
        try FileManager.default.createDirectory(at: one, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: two, withIntermediateDirectories: true)
        try Data([1]).write(to: one.appendingPathComponent("file1"))
        try Data([2]).write(to: two.appendingPathComponent("file2"))
        let scanGate = Gate(), routeGate = Gate()
        let model = DiskModel(cacheDirectory: root.deletingLastPathComponent().appendingPathComponent("cache"),
                              preferences: UserDefaults(suiteName: UUID().uuidString)!)
        model.testAfterStageStep = { stage in
            guard let view = try? stage.view(path: nil, removals: [], cancellation: ScanCancellation()),
                  view.folder.children.count == 2, !view.isListing else { return }
            scanGate.pause()
        }
        model.testBeforeFinishRouteLoad = { routeGate.pause() }
        model.scan(root)
        try await wait("root preview") { scanGate.waiting && model.current?.children.count == 2 }
        model.open(model.current!.children.first { $0.id == one.path }!)
        try await wait("first route") { model.current?.id == one.path }
        scanGate.release()
        try await wait("route load held") { routeGate.waiting }
        if CommandLine.arguments.count > 2 && CommandLine.arguments[2] == "remove" {
            let victim = one.appendingPathComponent("file1")
            try await wait("mapped victim") { model.current?.children.contains(where: { $0.id == victim.path }) == true }
            precondition(model.collect([model.current!.children.first { $0.id == victim.path }!]))
            try await wait("route collection") { !model.isValidatingCollection }
            model.deleteCollectionPermanently()
            try await wait("removal during route load") { !model.isRemoving && !FileManager.default.fileExists(atPath: victim.path) }
            routeGate.release()
            try await wait("handoff after removal") {
                !model.isScanning && !model.isRefreshing && model.current?.id == one.path &&
                model.current?.children.contains(where: { $0.id == victim.path }) == false
            }
            print("removal during saved-folder preparation reconciled before handoff")
            return
        }
        model.goBack()
        try await wait("back to root") { model.current?.id == root.path }
        model.open(model.current!.children.first { $0.id == two.path }!)
        try await wait("second route") { model.current?.id == two.path }
        routeGate.release()
        try await wait("handoff latest route") { !model.isScanning && !model.isPartialMap && model.current?.id == two.path }
        print("route changed during saved-folder preparation; handoff reached the latest route")
    }
}
