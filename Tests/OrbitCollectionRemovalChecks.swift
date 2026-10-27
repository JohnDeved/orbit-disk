import AppKit
import Combine
import Foundation
import OrbitCore

final class Gate: @unchecked Sendable {
    private let lock = NSLock(), semaphore = DispatchSemaphore(value: 0)
    private var armed = false, waiting = false
    func arm() { lock.lock(); armed = true; lock.unlock() }
    var isWaiting: Bool { lock.lock(); defer { lock.unlock() }; return waiting }
    func waitOnce() { lock.lock(); let wait = armed; armed = false; waiting = wait; lock.unlock()
        if wait { semaphore.wait(); lock.lock(); waiting = false; lock.unlock() } }
    func release() { semaphore.signal() }
}
@main struct Checks {
    @MainActor static func wait(_ message: String, _ condition: () -> Bool) async throws {
        for _ in 0..<1000 { if condition() { return }; try await Task.sleep(nanoseconds: 10_000_000) }
        throw NSError(domain: message, code: 1)
    }
    @MainActor static func main() async throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1]).standardizedFileURL
        let root = base.appendingPathComponent("root"), otherRoot = base.appendingPathComponent("other")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: otherRoot, withIntermediateDirectories: true)
        let a = root.appendingPathComponent("a"), b = root.appendingPathComponent("b")
        try Data([1,2,3]).write(to: a); try Data([4,5]).write(to: b)
        let suite = "ui-row-edit-\(UUID())", prefs = UserDefaults(suiteName: suite)!
        defer { prefs.removePersistentDomain(forName: suite) }
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("cache"), preferences: prefs)
        model.scan(root); try await wait("initial scan") { model.root?.id == root.path && !model.isScanning && model.node(withID: a.path) != nil }
        model.cancelScan()
        let first = model.node(withID: a.path)!, second = model.node(withID: b.path)!
        let existing = (0..<1_200).map { i in
            DiskNode(url: root.appendingPathComponent("fake-\(i)"), name: "fake-\(i)", isDirectory: false,
                     size: 1, allocatedSize: 1, fileCount: 1, fileIdentity: "fake:\(i)")
        }
        let gate = Gate(); model.testBeforeCollectionEdit = { gate.waitOnce() }
        model.collection = existing + [first, second]
        gate.arm(); model.removeFromCollection(first.id)
        try await wait("first worker") { gate.isWaiting }
        precondition(model.isUpdatingCollection && model.isRemovingFromCollection(first.id))
        model.deleteCollectionPermanently()
        precondition(!model.isRemoving && FileManager.default.fileExists(atPath: a.path))
        precondition(!model.collect([first]) && model.beginCollectionDrag([first], label: "a") == nil)
        model.removeFromCollection(second.id)
        precondition(model.isRemovingFromCollection(second.id))
        gate.release(); try await wait("queued removals") { !model.isUpdatingCollection }
        precondition(model.collection.count == 1_200 && model.collectionSize == 1_200
                     && !model.collection.contains { $0.id == first.id || $0.id == second.id })

        model.collection = existing
        model.showCollection = true
        gate.arm(); model.removeFromCollection(existing[0].id)
        try await wait("rapid gate") { gate.isWaiting }
        for index in 1..<100 { model.removeFromCollection(existing[index].id) }
        precondition((0..<100).allSatisfy { model.isRemovingFromCollection(existing[$0].id) })
        model.showCollection = false
        gate.release(); try await wait("closed sheet edit") { !model.isUpdatingCollection }
        precondition(model.collection.count == 1_100 && model.collectionSize == 1_100
                     && (0..<100).allSatisfy { index in !model.collection.contains { $0.id == existing[index].id } },
                     "Rapid edits must survive closing the collection sheet")

        model.collection = existing
        gate.arm(); model.removeFromCollection(existing[0].id)
        try await wait("switch gate") { gate.isWaiting }
        model.scan(otherRoot); try await wait("switch away") { model.root?.id == otherRoot.path && !model.isScanning }
        gate.release(); model.scan(root)
        try await wait("return edited root") { model.root?.id == root.path && !model.isScanning && !model.isUpdatingCollection }
        precondition(model.collection.count == 1_199 && !model.collection.contains { $0.id == existing[0].id },
                     "An accepted removal must survive switching locations")

        model.collection = existing
        gate.arm()
        var switchAtLaunch = true
        let switchObserver = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if switchAtLaunch { switchAtLaunch = false; model.scan(otherRoot) }
            }
        }
        model.removeFromCollection(existing[0].id)
        switchObserver.cancel()
        try await wait("reentrant launch switch") { !switchAtLaunch && gate.isWaiting && model.root?.id == otherRoot.path && !model.isScanning }
        gate.release(); model.scan(root)
        try await wait("reentrant launch return") { model.root?.id == root.path && !model.isScanning && !model.isUpdatingCollection }
        precondition(!model.collection.contains { $0.id == existing[0].id },
                     "Switching location during the launch notification must not lose the accepted removal")

        model.collection = existing
        gate.arm(); model.removeFromCollection(existing[0].id)
        try await wait("refresh gate") { gate.isWaiting }
        model.rescan()
        try await wait("refresh requested") { model.isScanning || model.isRefreshing }
        gate.release(); try await wait("refresh done") { !model.isScanning && !model.isRefreshing && !model.isUpdatingCollection }
        precondition(model.collection.count == 1_199 && !model.collection.contains { $0.id == existing[0].id },
                     "An accepted removal must survive a same-root refresh")

        model.collection = existing
        gate.arm(); model.removeFromCollection(existing[0].id)
        try await wait("clear gate") { gate.isWaiting }
        model.collection = []
        gate.release(); try await wait("clear retained") { !model.isUpdatingCollection }
        precondition(model.collection.isEmpty && model.collectionSize == 0,
                     "An older edit must not restore entries after clearing")

        model.collection = existing
        var armed = true
        let observer = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if armed { armed = false; model.removeFromCollection(existing[1].id) }
            }
        }
        model.removeFromCollection(existing[0].id)
        observer.cancel()
        try await wait("reentrant removals") { !model.isUpdatingCollection }
        precondition(!armed && model.collection.count == 1_198
                     && !model.collection.contains { $0.id == existing[0].id || $0.id == existing[1].id },
                     "Reentrant removal intents must both apply")
        print("PASS: pending collection edits, destructive-action guard, coalescing, root switch, refresh, clearing, and reentrancy")
    }
}
