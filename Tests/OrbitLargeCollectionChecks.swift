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
    @MainActor static func wait(_ condition: () -> Bool) async throws {
        for _ in 0..<500 { if condition() { return }; try await Task.sleep(nanoseconds: 10_000_000) }
        fatalError("timed out")
    }
    @MainActor static func main() async throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1]).standardizedFileURL, root = base.appendingPathComponent("root")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let a = root.appendingPathComponent("a"), b = root.appendingPathComponent("b")
        try Data([1,2,3]).write(to: a); try Data([4,5]).write(to: b)
        let suite = "ui-async-\(UUID())"
        let preferences = UserDefaults(suiteName: suite)!
        defer { preferences.removePersistentDomain(forName: suite) }
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("cache"), preferences: preferences)
        model.scan(root); try await wait { model.root != nil && !model.isScanning && model.node(withID: a.path) != nil && model.node(withID: b.path) != nil }
        model.cancelScan()
        let first = model.node(withID: a.path)!, second = model.node(withID: b.path)!
        let existing = (0..<1200).map { i in
            DiskNode(url: root.appendingPathComponent("fake-\(i)"), name: "fake-\(i)", isDirectory: false,
                     size: 1, allocatedSize: 1, fileCount: 1, fileIdentity: "fake:\(i)")
        }
        model.collection = existing
        let token = model.beginCollectionDrag([first], label: "a")!.data(forType: DiskModel.collectionDragType)!
        model.endCollectionDrag()
        precondition(model.collectDrag(data: token) && model.isValidatingCollection)
        try await wait { !model.isValidatingCollection }
        precondition(model.collection.count == 1201 && model.collection.last?.id == a.path && model.collectionSize == 1203)
        precondition(model.collect([second]) && model.isValidatingCollection)
        try await wait { !model.isValidatingCollection }
        precondition(model.collection.count == 1202 && model.collection.last?.id == b.path && model.collectionSize == 1205)
        model.collection = existing
        let gate = Gate(); model.testCollectionPrepare = { gate.waitOnce() }; gate.arm()
        precondition(model.collect([first]))
        try await wait { gate.isWaiting }
        model.collection = existing + [second]
        gate.release(); try await wait { !model.isValidatingCollection }
        precondition(model.collection.count == 1202 && model.collection.contains { $0.id == a.path }
                     && model.collection.contains { $0.id == b.path } && model.collectionSize == 1205)
        model.collection = existing
        gate.arm()
        precondition(model.collect([first]))
        try await wait { gate.isWaiting }
        model.cancelScan()
        precondition(model.isValidatingCollection && model.collection.count == existing.count,
                     "Pausing mapping must retain the accepted collection job")
        gate.release(); try await wait { !model.isValidatingCollection }
        precondition(model.collection.count == 1201 && model.collection.contains { $0.id == a.path } && model.collectionSize == 1203,
                     "The accepted collection must finish after mapping is paused")
        model.collection = existing
        var armed = true
        let observer = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if armed { armed = false; model.cancelScan() }
            }
        }
        precondition(!model.collect([first]))
        observer.cancel()
        precondition(!armed && !model.isValidatingCollection && model.collection.count == existing.count,
                     "Reentrant cancellation during launch must not leave validation stuck")
        let nextRoot = base.appendingPathComponent("other")
        try FileManager.default.createDirectory(at: nextRoot, withIntermediateDirectories: true)
        var switchArmed = true
        let switchObserver = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if switchArmed { switchArmed = false; model.scan(nextRoot) }
            }
        }
        precondition(!model.collect([first]))
        switchObserver.cancel()
        try await wait { model.root?.id == nextRoot.path && !model.isScanning }
        precondition(!switchArmed && !model.isValidatingCollection,
                     "Reentrant root switch during launch must not start stale validation")
        model.scan(root); try await wait { model.root?.id == root.path && !model.isScanning }
        model.collection = existing
        model.errorMessage = nil
        gate.arm()
        precondition(model.collect([first, model.root!]))
        try await wait { gate.isWaiting }
        model.testCancelCollectionValidation()
        gate.release(); try await wait { !model.isValidatingCollection }
        model.collection = existing
        gate.arm()
        var launchStatusArmed = true
        let launchStatusObserver = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if launchStatusArmed { launchStatusArmed = false; model.statusMessage = "Newer launch status" }
            }
        }
        precondition(model.collect([first]))
        launchStatusObserver.cancel()
        try await wait { gate.isWaiting }
        precondition(model.statusMessage == "Newer launch status")
        gate.release(); try await wait { !model.isValidatingCollection }
        precondition(model.statusMessage == "Newer launch status", "Validation must preserve a newer launch status")
        model.collection = existing
        model.errorMessage = nil
        gate.arm()
        precondition(model.collect([first, model.root!]))
        try await wait { gate.isWaiting }
        model.testCancelCollectionValidation()
        gate.release(); try await wait { !model.isValidatingCollection }
        model.collection = existing
        model.errorMessage = nil
        gate.arm()
        precondition(model.collect([first, model.root!]))
        try await wait { gate.isWaiting }
        model.errorMessage = "Newer midflight error"
        gate.release(); try await wait { !model.isValidatingCollection }
        precondition(model.errorMessage == "Newer midflight error", "Old rejection must not overwrite a newer error")
        model.errorMessage = nil
        try await wait { model.errorMessage?.contains("mapped folder cannot be removed") == true }
        model.errorMessage = nil
        model.collection = existing
        gate.arm()
        precondition(model.collect([first, model.root!]))
        try await wait { gate.isWaiting }
        var completionFeedbackArmed = true
        let completionFeedback = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if completionFeedbackArmed && model.isValidatingCollection {
                    completionFeedbackArmed = false
                    model.statusMessage = "Newer completion status"
                    model.errorMessage = "Newer completion error"
                }
            }
        }
        gate.release(); try await wait { !completionFeedbackArmed && !model.isValidatingCollection }
        completionFeedback.cancel()
        precondition(model.statusMessage == "Newer completion status" && model.errorMessage == "Newer completion error",
                     "Validation must preserve newer feedback set during its publication")
        model.errorMessage = nil
        try await wait { model.errorMessage?.contains("mapped folder cannot be removed") == true }
        model.errorMessage = nil
        model.collection = existing
        gate.arm()
        precondition(model.collect([first, model.root!]))
        try await wait { gate.isWaiting }
        var completionSwitchArmed = true
        let completionSwitch = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if completionSwitchArmed && model.isValidatingCollection {
                    completionSwitchArmed = false
                    model.scan(nextRoot)
                }
            }
        }
        gate.release()
        try await wait { !completionSwitchArmed && model.root?.id == nextRoot.path && !model.isScanning }
        completionSwitch.cancel()
        precondition(!model.isValidatingCollection && model.collection.isEmpty && model.errorMessage == nil
                     && !(model.statusMessage?.contains("Added a") ?? false) && !(model.statusMessage?.contains("Skipped") ?? false),
                     "Old-root completion must not publish collection, status, or errors")
        model.scan(root)
        try await wait { model.root?.id == root.path && !model.isScanning && !model.isValidatingCollection
                         && model.collection.contains { $0.id == first.id }
                         && model.errorMessage?.contains("mapped folder cannot be removed") == true }
        model.collection = existing
        model.errorMessage = nil
        gate.arm()
        precondition(model.collect([first, model.root!]))
        try await wait { gate.isWaiting }
        var completionCancelArmed = true
        let completionCancel = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if completionCancelArmed && model.isValidatingCollection {
                    completionCancelArmed = false
                    model.cancelScan()
                }
            }
        }
        gate.release()
        try await wait { !completionCancelArmed && !model.isValidatingCollection }
        completionCancel.cancel()
        precondition(model.collection.count == existing.count + 1 && model.collection.contains { $0.id == first.id }
                     && model.collectionSize == 1203 && model.errorMessage?.contains("mapped folder cannot be removed") == true
                     && !(model.statusMessage?.contains("Added a") ?? false) && !(model.statusMessage?.contains("Skipped") ?? false),
                     "Pausing mapping during completion must keep the collection and newer scan status")
        model.errorMessage = nil
        model.collection = existing
        gate.arm()
        precondition(model.collect([first]))
        try await wait { gate.isWaiting }
        var cancelReentryArmed = true
        let cancelReentry = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if cancelReentryArmed && model.isValidatingCollection {
                    cancelReentryArmed = false
                    model.cancelScan()
                    _ = model.collect([second])
                }
            }
        }
        model.cancelScan()
        gate.release()
        try await wait { !cancelReentryArmed && !model.isValidatingCollection }
        cancelReentry.cancel()
        precondition(model.errorMessage == nil && model.collection.count == existing.count + 1
                     && model.collection.contains { $0.id == first.id } && !(model.statusMessage?.contains("Checking") ?? false),
                     "Reentrant scan pause must preserve the accepted job without leaving stale validation state")
        model.collection = existing
        gate.arm()
        precondition(model.collect([first]))
        try await wait { gate.isWaiting }
        var repeatedCancelNotifications = 0
        let recursiveCancellation = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                repeatedCancelNotifications += 1
                model.testCancelCollectionValidation()
            }
        }
        model.testCancelCollectionValidation()
        recursiveCancellation.cancel()
        gate.release()
        try await wait { !model.isValidatingCollection }
        precondition(repeatedCancelNotifications == 1 && model.collection.count == existing.count,
                     "Repeated cancellation during its own notification must not recurse")
        model.collection = existing
        gate.arm()
        model.testCollectionPrepare = { gate.waitOnce() }
        precondition(model.collect([first]))
        try await wait { gate.isWaiting }
        model.testCancelCollectionValidation()
        model.testCollectionPrepare = {}
        precondition(model.collect([second]))
        try await wait { !model.isValidatingCollection }
        let newerStatus = model.statusMessage
        precondition(model.collection.count == existing.count + 1 && model.collection.last?.id == second.id)
        gate.release()
        try await Task.sleep(nanoseconds: 100_000_000)
        precondition(!model.isValidatingCollection && model.collection.count == existing.count + 1
                     && model.collection.last?.id == second.id && model.statusMessage == newerStatus,
                     "An older cancelled worker must not clear a newer completed validation")
        var errorNotifications = 0
        let errorObserver = model.objectWillChange.sink { _ in errorNotifications += 1 }
        model.errorMessage = "test error"
        errorObserver.cancel()
        precondition(errorNotifications == 1 && model.errorMessage == "test error", "Error changes must notify observers")
        model.errorMessage = nil
        model.collection = existing
        model.testCollectionPrepare = { gate.waitOnce() }
        gate.arm()
        precondition(model.collect([first, model.root!]))
        try await wait { gate.isWaiting }
        model.testCancelCollectionValidation()
        gate.release(); try await wait { !model.isValidatingCollection }
        model.collection = existing
        model.statusMessage = nil
        gate.arm()
        precondition(model.collect([first]))
        try await wait { gate.isWaiting }
        model.statusMessage = "Checking items for collection…"
        gate.release(); try await wait { !model.isValidatingCollection }
        precondition(model.statusMessage == "Checking items for collection…",
                     "Completion must preserve a newer identical status")
        model.collection = existing
        model.statusMessage = nil
        gate.arm()
        precondition(model.collect([first]))
        try await wait { gate.isWaiting }
        model.statusMessage = "Checking items for collection…"
        model.testCancelCollectionValidation()
        gate.release(); try await wait { !model.isValidatingCollection }
        precondition(model.statusMessage == "Checking items for collection…",
                     "Cancellation must preserve a newer identical status")
        model.statusMessage = nil
        model.collection = existing
        gate.arm()
        precondition(model.collect([first, model.root!]))
        try await wait { gate.isWaiting }
        let movedRoot = base.appendingPathComponent("old-root")
        try FileManager.default.moveItem(at: root, to: movedRoot)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        var failureSwitchArmed = true
        let failureSwitch = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if failureSwitchArmed && model.isValidatingCollection {
                    failureSwitchArmed = false
                    model.scan(nextRoot)
                }
            }
        }
        gate.release()
        try await wait { !failureSwitchArmed && model.root?.id == nextRoot.path && !model.isScanning }
        failureSwitch.cancel()
        precondition(!model.isValidatingCollection && model.collection.isEmpty && model.errorMessage == nil
                     && !(model.statusMessage?.contains("The map changed") ?? false),
                     "Failure cleanup must not publish stale status to a new root")
        let staleRoot = base.appendingPathComponent("stale-root"), staleFile = staleRoot.appendingPathComponent("a")
        try FileManager.default.createDirectory(at: staleRoot, withIntermediateDirectories: true)
        try Data([1]).write(to: staleFile)
        let staleSuite = "ui-async-stale-\(UUID())"
        let stalePreferences = UserDefaults(suiteName: staleSuite)!
        defer { stalePreferences.removePersistentDomain(forName: staleSuite) }
        let stale = DiskModel(cacheDirectory: base.appendingPathComponent("stale-cache"), preferences: stalePreferences)
        stale.scan(staleRoot); try await wait { stale.root?.id == staleRoot.path && !stale.isScanning }
        let staleNode = stale.node(withID: staleFile.path)!
        stale.collection = existing
        let staleGate = Gate(); stale.testCollectionPrepare = { staleGate.waitOnce() }; staleGate.arm()
        precondition(stale.collect([staleNode]))
        try await wait { staleGate.isWaiting }
        try FileManager.default.moveItem(at: staleRoot, to: base.appendingPathComponent("stale-old"))
        try FileManager.default.createDirectory(at: staleRoot, withIntermediateDirectories: true)
        stale.testBumpResolution()
        staleGate.release(); try await wait { !stale.isValidatingCollection }
        precondition(stale.statusMessage == nil, "Failure after request change must clear its stale progress")
        print("PASS: large collection small drop, selection action, revision retry, cancellation, reentrant launch, completion, feedback, failure cleanup, and totals")
    }
}
