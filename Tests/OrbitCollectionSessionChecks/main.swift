import AppKit
import Combine
import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Never>?
    private var first = true, waitingValue = false, released = false
    var waiting: Bool { lock.lock(); defer { lock.unlock() }; return waitingValue }
    func pause() async {
        precondition(!Thread.isMainThread)
        await withCheckedContinuation { continuation in
            lock.lock()
            let shouldWait = first && !released
            first = false
            if shouldWait { self.continuation = continuation; waitingValue = true }
            lock.unlock()
            if !shouldWait { continuation.resume() }
        }
    }
    func release() {
        lock.lock()
        released = true; waitingValue = false
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume()
    }
}
@main struct Check {
    private static let timeoutQueue = DispatchQueue(label: "OrbitCollectionSessionChecks.timeout")
    @MainActor static func wait(_ label: String, _ f: () -> Bool) async throws {
        fputs("CHECK collection session: \(label)\n", stderr)
        for _ in 0..<1000 { if f() { return }; try await Task.sleep(nanoseconds: 10_000_000) }
        fatalError("Timeout: \(label)")
    }
    @MainActor static func main() async throws {
        let deadline = DispatchWorkItem {
            fputs("FAIL collection session did not finish within 60 seconds; see the last CHECK line\n", stderr)
            exit(EXIT_FAILURE)
        }
        timeoutQueue.asyncAfter(deadline: .now() + 60, execute: deadline)
        defer { deadline.cancel() }
        let base=URL(fileURLWithPath:CommandLine.arguments[1]), fm=FileManager.default
        let a=base.appendingPathComponent("a"), b=base.appendingPathComponent("b")
        for dir in [a,b] { try fm.createDirectory(at:dir,withIntermediateDirectories:true); try Data([1]).write(to:dir.appendingPathComponent("file")) }
        let stale=b.appendingPathComponent("stale"), stale2=b.appendingPathComponent("stale2"), stale3=b.appendingPathComponent("stale3"), fresh=b.appendingPathComponent("fresh"), fresh2=b.appendingPathComponent("fresh2")
        for url in [stale,stale2,stale3,fresh,fresh2] { try Data([2]).write(to:url) }
        let prefs=UserDefaults(suiteName:"collection-reentry-\(UUID())")!
        let m=DiskModel(cacheDirectory:base.appendingPathComponent("cache"),preferences:prefs)
        m.scan(b); try await wait("B map") { m.root?.id == b.path && !m.isScanning }
        let bItem=m.current!.children.first { $0.name == "file" }!, staleItem=m.current!.children.first { $0.name == "stale" }!, bGate=Gate()
        try fm.removeItem(at:stale)
        m.testCollectionPrepare={await bGate.pause()}
        precondition(m.testCollectInBackground([bItem, staleItem]))
        try await wait("B validation") {bGate.waiting && m.isValidatingCollection}
        m.scan(a); try await wait("A map") {m.root?.id == a.path && !m.isScanning}
        let aItem=m.current!.children.first!, aGate=Gate()
        m.testCollectionPrepare={await aGate.pause()}
        precondition(m.testCollectInBackground([aItem]))
        try await wait("A validation") {aGate.waiting && m.isValidatingCollection}
        let reopen=Gate(); m.testBeforeSessionReopen={await reopen.pause()}
        var fired=false
        let observer=m.objectWillChange.sink { _ in
            guard !fired else {return}; fired=true; m.testActivateResolved(b)
        }
        m.testDetachValidation(); observer.cancel()
        try await wait("B active while A shown") { fired && reopen.waiting && m.testActiveSessionRoot() == b.path && m.root?.id == a.path }
        bGate.release()
        try await wait("B commit before root publication") {m.collection.map(\.id) == [bItem.id]}
        precondition(m.root?.id == a.path && m.displayedCollection.isEmpty && m.displayedCollectionSize == 0
                     && m.collectionCoverage.isEmpty && !m.canReviewDisplayedCollection,
                     "A displayed map must not expose B's collection, count, size, highlights, or review action")
        reopen.release()
        try await wait("B displayed") {m.root?.id == b.path && !m.isScanning}
        precondition(m.collection.map(\.id) == [bItem.id] && m.displayedCollection.map(\.id) == [bItem.id]
                     && m.canReviewDisplayedCollection && !m.isValidatingCollection, "B result or busy state lost")
        precondition(m.errorMessage?.contains("stale") == true, "B rejected item was not reported after root publication")
        m.scan(a); try await wait("undismissed error away") {m.root?.id == a.path && !m.isScanning}
        m.scan(b); try await wait("undismissed error return") {m.root?.id == b.path && !m.isScanning && m.errorMessage?.contains("stale") == true}
        m.errorMessage=nil
        aGate.release()
        m.testBeforeSessionReopen={}
        m.scan(a); try await wait("A restored") {m.root?.id == a.path && !m.isScanning && m.collection.map(\.id) == [aItem.id] && !m.isValidatingCollection}
        m.scan(b); try await wait("B retained") {m.root?.id == b.path && !m.isScanning && m.collection.map(\.id) == [bItem.id]}
        let next=m.current!.children.first { $0.name == "stale2" }!
        try fm.removeItem(at:stale2)
        let secondGate=Gate(); m.testCollectionPrepare={await secondGate.pause()}
        precondition(m.testCollectInBackground([next])); try await wait("newer error held") {secondGate.waiting}
        m.errorMessage="newer error"; secondGate.release()
        try await wait("newer error retained") {!m.isValidatingCollection}
        precondition(m.errorMessage == "newer error", "new rejection overwrote the newer error")
        var intercept=false, armIntercept=false
        let errorObserver=m.objectWillChange.sink { _ in
            guard armIntercept && !intercept && m.errorMessage == nil else {return}
            intercept=true; m.errorMessage="intervening error"
        }
        armIntercept=true; m.errorMessage=nil
        try await wait("intervening error preserved") {intercept && m.errorMessage == "intervening error"}
        errorObserver.cancel()
        m.errorMessage=nil
        try await wait("deferred rejection") {m.errorMessage?.contains("stale2") == true}
        m.errorMessage=nil
        let third=m.current!.children.first { $0.name == "stale3" }!
        try fm.removeItem(at:stale3)
        let thirdGate=Gate(); m.testCollectionPrepare={await thirdGate.pause()}
        precondition(m.testCollectInBackground([third])); try await wait("return rejection held") {thirdGate.waiting}
        m.scan(a); try await wait("return rejection away") {m.root?.id == a.path && !m.isScanning}
        m.scan(b); try await wait("return rejection back") {m.root?.id == b.path && !m.isScanning && m.isValidatingCollection}
        thirdGate.release(); try await wait("return rejection visible") {!m.isValidatingCollection && m.errorMessage?.contains("stale3") == true}
        m.errorMessage=nil
        let fresh2Item=m.current!.children.first { $0.name == "fresh2" }!
        var launchObserved=false
        let launchObserver=m.objectWillChange.sink { _ in
            guard !launchObserved else {return}; launchObserved=true; m.testSetCollectionEdit(b,pending:true)
        }
        precondition(!m.testCollectInBackground([fresh2Item]), "validation started despite reentrant row edit")
        launchObserver.cancel(); precondition(launchObserved); m.testSetCollectionEdit(b,pending:false)
        let sameStatusGate=Gate(); m.testCollectionPrepare={await sameStatusGate.pause()}
        precondition(m.testCollectInBackground([fresh2Item])); try await wait("same status held") {sameStatusGate.waiting}
        precondition(m.statusMessage == "Checking items for collection…")
        m.statusMessage="Checking items for collection…"
        m.scan(a); try await wait("same status away") {m.root?.id == a.path && !m.isScanning}
        sameStatusGate.release(); try await wait("same status job done") {!m.testHasPendingCollection(b)}
        m.scan(b); try await wait("same status return") {m.root?.id == b.path && !m.isScanning}
        precondition(m.statusMessage?.contains("Added fresh2") != true, "older job overwrote same-text newer status")
        m.testSetTransferFeedback(b)
        m.testActivateResolved(b,force:true)
        precondition(m.testTransferFeedback(b) == "queued collection status", "force refresh lost inherited collection feedback")
        try await wait("feedback transfer refresh") {m.root?.id == b.path && !m.isScanning && !m.isRefreshing}
        let freshItem=m.current!.children.first { $0.name == "fresh" }!, failureGate=Gate()
        m.testCollectionPrepare={await failureGate.pause()}; m.testForceCollectionFailure=true
        precondition(m.testCollectInBackground([freshItem])); try await wait("failure held") {failureGate.waiting}
        var forced=false
        let failureObserver=m.objectWillChange.sink { _ in
            guard !forced && m.isValidatingCollection && !failureGate.waiting else {return}
            forced=true; m.testActivateResolved(b,force:true)
        }
        failureGate.release()
        try await wait("failure reentrant refresh") {forced && m.root?.id == b.path && !m.isScanning && !m.isRefreshing}
        try await wait("failure preserved") {m.statusMessage?.contains("map changed") == true || m.errorMessage?.contains("map changed") == true}
        failureObserver.cancel(); m.testForceCollectionFailure=false
        m.cancelScan()
        let ownerA=base.appendingPathComponent("owner-a"), ownerB=base.appendingPathComponent("owner-b")
        for dir in [ownerA, ownerB] {
            try fm.createDirectory(at:dir,withIntermediateDirectories:true)
            try Data([1]).write(to:dir.appendingPathComponent("file"))
        }
        let other=DiskModel(cacheDirectory:base.appendingPathComponent("owner-cache"),preferences:prefs)
        other.scan(ownerA); try await wait("owner A map") {other.root?.id == ownerA.path && !other.isScanning}
        let ownerItem=other.current!.children[0], ownerGate=Gate()
        other.testCollectionPrepare={await ownerGate.pause()}
        precondition(other.testCollectInBackground([ownerItem]))
        try await wait("owner A job held") {ownerGate.waiting}
        other.scan(ownerB); try await wait("owner B map") {other.root?.id == ownerB.path && !other.isScanning}
        precondition(other.collection.isEmpty)
        other.statusMessage="B status"; other.errorMessage="B error"
        ownerGate.release(); try await wait("owner A completes offscreen") {!other.testHasPendingCollection(ownerA)}
        precondition(other.root?.id == ownerB.path && other.collection.isEmpty && other.statusMessage == "B status" && other.errorMessage == "B error",
                     "An offscreen collection must not publish items or feedback into another root")
        other.scan(ownerA); try await wait("owner A collection restored") {other.root?.id == ownerA.path && !other.isScanning && other.collection.map(\.id) == [ownerItem.id]}
        other.errorMessage="A feedback"
        other.beginCollectionReview()
        let nilRootOwner=other.collectionReviewToken!, originalOwnerRoot=other.root
        other.root=nil
        other.dismissCollection(review:nilRootOwner)
        precondition(!other.showCollection && other.errorMessage == "A feedback", "A review may close after root invalidation without clearing unowned feedback")
        other.root=originalOwnerRoot
        other.beginCollectionReview()
        let oldOwner=other.collectionReviewToken!
        other.scan(ownerB); try await wait("review owner B") {other.root?.id == ownerB.path && !other.isScanning}
        let ownerBItem=other.current!.children[0]
        precondition(other.collect([ownerBItem]))
        other.beginCollectionReview()
        let currentOwner=other.collectionReviewToken!
        precondition(!other.collectionReviewHasOwner(oldOwner) && other.collectionReviewMatches(currentOwner))
        other.errorMessage="B newer error"
        other.clearCollection(review:oldOwner)
        other.removeFromCollection(ownerBItem.id,review:oldOwner)
        other.moveCollectionToTrash(review:oldOwner)
        other.deleteCollectionPermanently(review:oldOwner)
        other.dismissCollectionError(review:oldOwner)
        other.dismissCollection(review:oldOwner)
        precondition(other.collection.map(\.id) == [ownerBItem.id] && other.errorMessage == "B newer error"
                     && other.showCollection && !other.isRemoving && fm.fileExists(atPath:ownerBItem.id),
                     "Stale review and confirmation actions must not change another session")
        other.dismissCollection(review:currentOwner)
        precondition(!other.showCollection && other.errorMessage == "B newer error",
                     "An older Done callback must not dismiss newer feedback")
        other.beginCollectionReview()
        let freshOwner=other.collectionReviewToken!
        other.collection = other.collection
        precondition(!other.collectionReviewMatches(freshOwner), "A review action must capture its collection revision")
        other.deleteCollectionPermanently(review:freshOwner)
        other.moveCollectionToTrash(review:freshOwner)
        other.removeFromCollection(ownerBItem.id,review:freshOwner)
        other.clearCollection(review:freshOwner)
        precondition(other.showCollection && !other.isRemoving && other.collection.map(\.id) == [ownerBItem.id] && fm.fileExists(atPath:ownerBItem.id),
                     "A stale confirmation must not act on a changed collection")
        let updatedOwner=other.collectionReviewToken!
        other.dismissCollection(review:updatedOwner)
        precondition(!other.showCollection)
        other.beginCollectionReview()
        let reopened=other.collectionReviewToken!
        precondition(!other.collectionReviewHasOwner(freshOwner) && other.collectionReviewHasOwner(reopened),
                     "A reopened sheet must have a distinct review instance")
        other.clearCollection(review:freshOwner)
        precondition(other.showCollection && other.collection.map(\.id) == [ownerBItem.id])
        var reopenArmed=true, replacementReview: CollectionReviewToken?
        let reopenObserver=other.objectWillChange.sink { _ in
            guard reopenArmed else { return }
            reopenArmed=false
            other.dismissCollection(review:reopened)
            replacementReview=other.beginCollectionReview()
        }
        other.removeFromCollection(ownerBItem.id,review:reopened)
        reopenObserver.cancel()
        precondition(!reopenArmed && replacementReview != nil && replacementReview != reopened
                     && other.collection.map(\.id) == [ownerBItem.id],
                     "A row action from a dismissed review must not alter the reopened review")
        var rowNotifications=0
        let rowObserver=other.objectWillChange.sink { _ in rowNotifications += 1 }
        other.removeFromCollection(ownerBItem.id,review:replacementReview)
        rowObserver.cancel()
        precondition(rowNotifications == 1 && other.collection.isEmpty,
                     "A row action must publish once and atomically after its ownership check")
        other.collection=[ownerBItem]
        other.scan(ownerA); try await wait("review A for reentry") {other.root?.id == ownerA.path && !other.isScanning}
        other.beginCollectionReview(); let reentryOwner=other.collectionReviewToken!
        var switched=false
        let clearObserver=other.objectWillChange.sink { _ in
            guard !switched else { return }
            switched=true; other.testActivateResolved(ownerB)
        }
        other.clearCollection(review:reentryOwner)
        clearObserver.cancel()
        try await wait("review B after reentrant clear") {switched && other.root?.id == ownerB.path && !other.isScanning}
        precondition(other.collection.map(\.id) == [ownerBItem.id] && fm.fileExists(atPath:ownerBItem.id),
                     "A reentrant session switch during clear must not erase the new session")
        var openArmed=true, nestedOwner: CollectionReviewToken?
        let openObserver=other.objectWillChange.sink { _ in
            guard openArmed else { return }
            openArmed=false
            other.collection = other.collection
            nestedOwner = other.beginCollectionReview()
        }
        let rejectedOwner=other.beginCollectionReview()
        openObserver.cancel()
        precondition(!openArmed && rejectedOwner == nil && nestedOwner != nil && other.collectionReviewToken == nestedOwner,
                     "A rejected outer open must not claim a reentrantly opened review")
        other.dismissCollection(review:nestedOwner)
        other.cancelScan()
        print("PASS: session transitions, deferred rejection ownership, offscreen completion and stale review actions")
    }
}
