import AppKit
import Combine
import Foundation
import OrbitCore

actor AsyncGate {
    var waiting = false
    var used = false
    var resumed = false
    var continuation: CheckedContinuation<Void, Never>?
    func pauseOnce() async {
        guard !used else { return }
        used = true
        await withCheckedContinuation { continuation in
            self.continuation = continuation
            waiting = true
        }
        resumed = true
    }
    func release() { waiting = false; continuation?.resume(); continuation = nil }
}
actor StepGate {
    var calls = 0
    var waiting = 0
    var finished = 0
    var continuation: CheckedContinuation<Void, Never>?
    func pauseSelected() async {
        calls += 1
        let call = calls
        if call == 1 || call == 3 {
            await withCheckedContinuation { continuation in
                self.continuation = continuation
                waiting = call
            }
            waiting = 0
        }
        finished = call
    }
    func release() { continuation?.resume(); continuation = nil }
}
actor BufferedDeleteGate {
    var calls = 0
    var waiting = 0
    var continuation: CheckedContinuation<Void, Never>?
    func pauseSelected() async {
        calls += 1
        let call = calls
        if call == 2 || call == 4 || call == 7 {
            await withCheckedContinuation { continuation in
                self.continuation = continuation
                waiting = call
            }
            waiting = 0
        }
    }
    func release() { continuation?.resume(); continuation = nil }
}
final class ThreadGate: @unchecked Sendable {
    let lock = NSLock(), semaphore = DispatchSemaphore(value: 0)
    private var active = false
    var waiting: Bool { lock.lock(); defer { lock.unlock() }; return active }
    func pause() {
        lock.lock(); active = true; lock.unlock()
        semaphore.wait()
        lock.lock(); active = false; lock.unlock()
    }
    func release() { semaphore.signal() }
}
@main struct QueueChecks {
    @MainActor static func wait(_ label: String, _ check: @MainActor () async -> Bool) async throws {
        for _ in 0..<3000 { if await check() { return }; try await Task.sleep(nanoseconds: 3_000_000) }
        fatalError("timeout: \(label)")
    }
    @MainActor static func main() async throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let root = base.appendingPathComponent("root", isDirectory: true)
        let other = base.appendingPathComponent("other", isDirectory: true)
        let fm = FileManager.default
        try fm.createDirectory(at: root, withIntermediateDirectories: true)
        try fm.createDirectory(at: other, withIntermediateDirectories: true)
        for name in ["one", "two", "three", "four", "five", "six", "seven", "eight", "gone"] { try Data([1]).write(to: root.appendingPathComponent(name)) }
        try Data([1]).write(to: other.appendingPathComponent("other-file"))
        let suite = "queue-check-\(UUID())"
        let preferences = UserDefaults(suiteName: suite)!
        defer { preferences.removePersistentDomain(forName: suite) }
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("index"), preferences: preferences)
        model.scan(root)
        try await wait("initial map") { model.root?.id == root.path && !model.isScanning }
        func node(_ name: String) -> DiskNode { model.current!.children.first { $0.name == name }! }
        precondition(model.collect([node("one")]))
        try await wait("initial collect") { !model.isValidatingCollection && model.collection.count == 1 }
        let gate = AsyncGate()
        model.testCollectionPrepare = { await gate.pauseOnce() }
        precondition(model.collect([node("two")]))
        try await wait("held validation") { await gate.waiting }
        let queued = model.beginCollectionDrag([node("three")], label: "three")!
        let queuedToken = queued.data(forType: DiskModel.collectionDragType)!
        model.endCollectionDrag(queued)
        precondition(model.collectDrag(data: queuedToken), "Second drop rejected while validation runs")
        let review = model.beginCollectionReview()!
        precondition(model.collectionReviewMatches(review))
        let deletionGate = ThreadGate()
        model.testBeforeRemoval = { deletionGate.pause() }
        model.deleteCollectionPermanently(review: review)
        try await wait("held removal") { deletionGate.waiting && model.isRemoving }
        let duringDeletion = model.beginCollectionDrag([node("four")], label: "four")!
        let deletionToken = duringDeletion.data(forType: DiskModel.collectionDragType)!
        model.endCollectionDrag(duringDeletion)
        precondition(model.collectDrag(data: deletionToken), "Drop rejected while deleting previous selection")
        model.scan(other)
        try await wait("switch away") { model.root?.id == other.path && !model.isScanning }
        model.scan(root)
        try await wait("switch back") { model.root?.id == root.path && model.current?.children.count == 9 }
        await gate.release()
        deletionGate.release()
        try await wait("queued drops applied") { !model.isRemoving && !model.isValidatingCollection && model.collection.count == 3 }
        precondition(!fm.fileExists(atPath: root.appendingPathComponent("one").path))
        precondition(Set(model.collection.map(\.name)) == Set(["two", "three", "four"]))
        precondition(["two", "three", "four"].allSatisfy { fm.fileExists(atPath: root.appendingPathComponent($0).path) })
        let alreadyIncluded = Array(repeating: node("three"), count: 65)
        let group = model.beginCollectionDrag(alreadyIncluded, label: "65 items")!
        let groupToken = group.data(forType: DiskModel.collectionDragType)!
        model.endCollectionDrag(group)
        precondition(model.collectDrag(data: groupToken), "Large groups should start without scanning every member on the main thread")
        try await wait("included group") { !model.isValidatingCollection }
        precondition(model.collection.count == 3 && model.statusMessage?.contains("already included") == true)
        let firstDelayed = model.beginCollectionDrag([node("five")], label: "five")!
        let firstData = firstDelayed.data(forType: DiskModel.collectionDragType)!
        let firstReservation = model.reserveCollectionDrop()!
        let newerDrag = model.beginCollectionDrag([node("six")], label: "newer")!
        let newerData = newerDrag.data(forType: DiskModel.collectionDragType)!
        precondition(model.completeCollectionDrop(firstReservation, data: firstData), "Accepted drop was lost to a newer drag")
        precondition(model.draggingLabel == "newer", "An earlier drop cleared a newer drag")
        precondition(!model.completeCollectionDrop(firstReservation, data: firstData), "A reservation must be consumed once")
        let secondReservation = model.reserveCollectionDrop()!
        model.scan(other)
        try await wait("switch before drop data") { model.root?.id == other.path && !model.isScanning }
        precondition(model.completeCollectionDrop(secondReservation, data: newerData), "Accepted drop was lost after switching roots")
        precondition(model.collection.isEmpty, "Drop from another root entered the visible collection")
        model.scan(root)
        try await wait("reserved drops restored") {
            model.root?.id == root.path && !model.isValidatingCollection
                && model.collection.contains { $0.name == "five" } && model.collection.contains { $0.name == "six" }
        }
        let failedData = model.beginCollectionDrag([node("gone")], label: "gone")!
        let failedToken = failedData.data(forType: DiskModel.collectionDragType)!
        let failedReservation = model.reserveCollectionDrop()!
        precondition(!model.completeCollectionDrop(failedReservation, data: nil))
        precondition(!model.completeCollectionDrop(failedReservation, data: failedToken), "Failed provider data must release its reservation")
        let vanished = node("gone")
        try fm.removeItem(at: vanished.url)
        precondition(model.collect([vanished]), "A stale request must be accepted for background validation")
        try await wait("missing item rejected") { !model.isValidatingCollection && model.errorMessage != nil }
        precondition(!model.collection.contains { $0.id == vanished.id })
        let clearGate = AsyncGate()
        model.testCollectionPrepare = { await clearGate.pauseOnce() }
        precondition(model.collect([node("seven")]))
        try await wait("held add before clear") { await clearGate.waiting }
        let clearReview = model.beginCollectionReview()!
        let pendingProvider = model.beginCollectionDrag([node("eight")], label: "eight")!
        let pendingData = pendingProvider.data(forType: DiskModel.collectionDragType)!
        let pendingReservation = model.reserveCollectionDrop()!
        model.clearCollection(review: clearReview)
        precondition(!model.completeCollectionDrop(pendingReservation, data: pendingData), "Clear must cancel delayed provider data")
        await clearGate.release()
        try await wait("cancelled worker resumed") { await clearGate.resumed }
        try await Task.sleep(nanoseconds: 100_000_000)
        precondition(model.collection.isEmpty && !model.isValidatingCollection,
                     "Clear must cancel queued additions rather than refill the collection")
        let oldDrag = model.beginCollectionDrag([node("five")], label: "five")!
        let oldToken = oldDrag.data(forType: DiskModel.collectionDragType)!
        var switched = false
        let observer = model.objectWillChange.sink { _ in
            guard !switched else { return }
            switched = true
            model.testActivateResolved(other)
        }
        precondition(!model.collectDrag(data: oldToken), "A reentrant root switch must reject the stale drop")
        precondition(switched)
        observer.cancel()
        model.cancelScan()
        let fairnessRoot = base.appendingPathComponent("fairness", isDirectory: true)
        try fm.createDirectory(at: fairnessRoot, withIntermediateDirectories: true)
        for index in 0..<130 { try Data([1]).write(to: fairnessRoot.appendingPathComponent("group-\(index)")) }
        try Data([2]).write(to: fairnessRoot.appendingPathComponent("quick"))
        let fairness = DiskModel(cacheDirectory: base.appendingPathComponent("fairness-index"), preferences: preferences)
        fairness.scan(fairnessRoot)
        try await wait("fairness map") { fairness.root?.id == fairnessRoot.path && !fairness.isScanning }
        let groupNodes = fairness.current!.children.filter { $0.name.hasPrefix("group-") }
        let quick = fairness.current!.children.first { $0.name == "quick" }!
        let steps = StepGate()
        fairness.testCollectionPrepare = { await steps.pauseSelected() }
        let largeDrag = fairness.beginCollectionDrag(groupNodes, label: "130 items")!
        let largeToken = largeDrag.data(forType: DiskModel.collectionDragType)!
        fairness.endCollectionDrag(largeDrag)
        precondition(fairness.collectDrag(data: largeToken))
        try await wait("first group chunk held") { await steps.waiting == 1 }
        precondition(fairness.collect([quick]))
        await steps.release()
        try await wait("small item overtakes group remainder") {
            await steps.waiting == 3 && fairness.collection.contains { $0.id == quick.id }
        }
        precondition(fairness.collection.count == 129 && !fairness.collection.contains { $0.id == groupNodes[128].id })
        let fairnessReview = fairness.beginCollectionReview()!
        fairness.clearCollection(review: fairnessReview)
        await steps.release()
        try await wait("cancelled group worker resumed") { await steps.finished == 3 }
        try await Task.sleep(nanoseconds: 100_000_000)
        precondition(fairness.collection.isEmpty && !fairness.isValidatingCollection,
                     "Clear must cancel every remaining group chunk")
        fairness.cancelScan()
        let bufferRoot = base.appendingPathComponent("buffered-delete", isDirectory: true)
        try fm.createDirectory(at: bufferRoot, withIntermediateDirectories: true)
        for index in 0..<260 { try Data([1]).write(to: bufferRoot.appendingPathComponent("entry-\(index)")) }
        let bufferModel = DiskModel(cacheDirectory: base.appendingPathComponent("buffer-index"), preferences: preferences)
        bufferModel.scan(bufferRoot)
        try await wait("buffer deletion map") { bufferModel.root?.id == bufferRoot.path && !bufferModel.isScanning }
        let bufferNodes = bufferModel.current!.children
        let bufferedVictim = bufferNodes[128]
        let bufferGate = BufferedDeleteGate()
        bufferModel.testCollectionPrepare = { await bufferGate.pauseSelected() }
        let bufferDrag = bufferModel.beginCollectionDrag(bufferNodes, label: "260 items")!
        let bufferToken = bufferDrag.data(forType: DiskModel.collectionDragType)!
        bufferModel.endCollectionDrag(bufferDrag)
        precondition(bufferModel.collectDrag(data: bufferToken))
        try await wait("buffered chunk held") { await bufferGate.waiting == 2 }
        precondition(bufferModel.collect([bufferedVictim]))
        await bufferGate.release()
        try await wait("buffered victim separately collected") {
            await bufferGate.waiting == 4 && bufferModel.collection.contains { $0.id == bufferedVictim.id }
        }
        let laterQuick = bufferNodes[258]
        precondition(bufferModel.collect([laterQuick]), "A small request must be accepted during a buffer recheck")
        bufferModel.collection = [bufferedVictim]
        bufferModel.deleteCollectionPermanently()
        try await wait("delete buffered victim") { !bufferModel.isRemoving && !fm.fileExists(atPath: bufferedVictim.id) }
        await bufferGate.release()
        try await wait("small request overtakes recheck") {
            await bufferGate.waiting == 7 && bufferModel.collection.contains { $0.id == laterQuick.id }
        }
        precondition(!bufferModel.collection.contains { $0.id == bufferNodes[129].id },
                     "Buffered group remainder committed before the later small request")
        await bufferGate.release()
        try await wait("buffer group completes") { !bufferModel.isValidatingCollection }
        precondition(!bufferModel.collection.contains { $0.id == bufferedVictim.id } && !fm.fileExists(atPath: bufferedVictim.id),
                     "A deleted file must not reappear when buffered validation commits")
        precondition(!bufferModel.collection.isEmpty, "Surviving buffered files should still be collected")
        bufferModel.cancelScan()
        let confirmRoot = base.appendingPathComponent("confirmation", isDirectory: true)
        let confirmOther = base.appendingPathComponent("confirmation-other", isDirectory: true)
        try fm.createDirectory(at: confirmRoot, withIntermediateDirectories: true)
        try fm.createDirectory(at: confirmOther, withIntermediateDirectories: true)
        for name in ["reviewed", "later", "cancelled", "switched", "cleared"] {
            try Data([1]).write(to: confirmRoot.appendingPathComponent(name))
        }
        try Data([1]).write(to: confirmOther.appendingPathComponent("other"))
        let confirm = DiskModel(cacheDirectory: base.appendingPathComponent("confirmation-index"), preferences: preferences)
        confirm.scan(confirmRoot)
        try await wait("confirmation map") { confirm.root?.id == confirmRoot.path && !confirm.isScanning }
        func confirmNode(_ name: String) -> DiskNode { confirm.current!.children.first { $0.name == name }! }
        precondition(confirm.collect([confirmNode("reviewed")]))
        try await wait("confirmation initial collection") { !confirm.isValidatingCollection && confirm.collection.count == 1 }
        let confirmationGate = AsyncGate()
        confirm.testCollectionPrepare = { await confirmationGate.pauseOnce() }
        precondition(confirm.collect([confirmNode("later")]))
        try await wait("confirmation validation") { await confirmationGate.waiting }
        let reviewed = confirm.beginCollectionReview()!
        let lease = confirm.beginCollectionConfirmation(review: reviewed)!
        await confirmationGate.release()
        try await wait("publication held for confirmation") { confirm.testConfirmationPublicationHoldCount == 1 }
        precondition(confirm.collectionReviewMatches(reviewed) && confirm.collection.map(\.name) == ["reviewed"])
        confirm.deleteCollectionPermanently(review: reviewed)
        confirm.endCollectionConfirmation(lease)
        try await wait("confirmed removal and later addition") {
            !confirm.isRemoving && !confirm.isValidatingCollection && confirm.collection.map(\.name) == ["later"]
        }
        precondition(!fm.fileExists(atPath: confirmRoot.appendingPathComponent("reviewed").path))
        precondition(fm.fileExists(atPath: confirmRoot.appendingPathComponent("later").path), "Unreviewed item was deleted")
        if let token = confirm.collectionReviewToken { confirm.dismissCollection(review: token) }
        let cancelGate = AsyncGate()
        confirm.testCollectionPrepare = { await cancelGate.pauseOnce() }
        precondition(confirm.collect([confirmNode("cancelled")]))
        try await wait("cancel validation") { await cancelGate.waiting }
        let cancelReview = confirm.beginCollectionReview()!
        let cancelLease = confirm.beginCollectionConfirmation(review: cancelReview)!
        await cancelGate.release()
        try await wait("publication held before cancel") { confirm.testConfirmationPublicationHoldCount == 2 }
        confirm.endCollectionConfirmation(cancelLease)
        try await wait("publication resumes after cancel") {
            !confirm.isValidatingCollection && confirm.collection.contains { $0.name == "cancelled" }
        }
        precondition(!confirm.collectionReviewMatches(cancelReview))
        confirm.dismissCollection(review: cancelReview)
        let switchGate = AsyncGate()
        confirm.testCollectionPrepare = { await switchGate.pauseOnce() }
        precondition(confirm.collect([confirmNode("switched")]))
        try await wait("switch validation") { await switchGate.waiting }
        let switchReview = confirm.beginCollectionReview()!
        let switchLease = confirm.beginCollectionConfirmation(review: switchReview)!
        await switchGate.release()
        try await wait("publication held before switch") { confirm.testConfirmationPublicationHoldCount == 3 }
        confirm.testCollectionPrepare = {}
        confirm.scan(confirmOther)
        try await wait("switch confirmation owner") { confirm.root?.id == confirmOther.path && !confirm.isScanning }
        precondition(confirm.collect([confirm.current!.children.first { $0.name == "other" }!]))
        try await wait("other session is not held") { !confirm.isValidatingCollection && confirm.collection.map(\.name) == ["other"] }
        let otherReview = confirm.beginCollectionReview()!
        let otherLease = confirm.beginCollectionConfirmation(review: otherReview)!
        confirm.endCollectionConfirmation(switchLease)
        precondition(confirm.beginCollectionConfirmation(review: otherReview) == nil, "Stale lease released the new confirmation")
        confirm.endCollectionConfirmation(otherLease)
        confirm.dismissCollection(review: otherReview)
        confirm.scan(confirmRoot)
        try await wait("switched collection restored") {
            confirm.root?.id == confirmRoot.path && !confirm.isValidatingCollection && confirm.collection.contains { $0.name == "switched" }
        }
        let clearConfirmationGate = AsyncGate()
        confirm.testCollectionPrepare = { await clearConfirmationGate.pauseOnce() }
        precondition(confirm.collect([confirmNode("cleared")]))
        try await wait("clear confirmation validation") { await clearConfirmationGate.waiting }
        let clearConfirmation = confirm.beginCollectionReview()!
        let clearLease = confirm.beginCollectionConfirmation(review: clearConfirmation)!
        await clearConfirmationGate.release()
        try await wait("publication held before clear") { confirm.testConfirmationPublicationHoldCount == 4 }
        confirm.clearCollection(review: clearConfirmation)
        confirm.endCollectionConfirmation(clearLease)
        precondition(confirm.collection.isEmpty && !confirm.isValidatingCollection && !confirm.showCollection)
        confirm.cancelScan()
        print("PASS: queued add/drop survives validation, deletion, and location switching")
    }
}
