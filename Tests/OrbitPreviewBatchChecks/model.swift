import AppKit
import Combine
import Foundation
import OrbitCore
struct InjectedFailure: Error {}
final class Gate: @unchecked Sendable {
    let lock=NSLock(), semaphore=DispatchSemaphore(value: 0)
    var active=false, waiting=false, fail=false
    func arm(fail: Bool) { lock.lock(); active=true; waiting=false; self.fail=fail; lock.unlock() }
    var isWaiting: Bool { lock.lock(); defer {lock.unlock()}; return waiting }
    func wait() throws {
        lock.lock(); let take=active; if take {active=false; waiting=true}; lock.unlock()
        guard take else { return }
        semaphore.wait()
        lock.lock(); waiting=false; let shouldFail=fail; lock.unlock()
        if shouldFail { throw InjectedFailure() }
    }
    func release() { semaphore.signal() }
}

@main struct ModelChecks {
    @MainActor static func wait(_ description: String, _ condition: () -> Bool) async throws {
        let deadline=Date().addingTimeInterval(30)
        while !condition() { if Date()>deadline { fatalError("timeout: \(description)") }; try await Task.sleep(nanoseconds: 3_000_000) }
    }
    @MainActor static func main() async throws {
        let base=URL(fileURLWithPath: CommandLine.arguments[1],isDirectory:true).standardizedFileURL
        let root=base.appendingPathComponent("model-files", isDirectory:true)
        let fm=FileManager.default
        try fm.createDirectory(at: root,withIntermediateDirectories:true)
        for i in 0..<2_500 { try fm.createDirectory(at: root.appendingPathComponent("folder-\(i)"),withIntermediateDirectories:true) }
        let testingBatch=Gate(), testingFallback=Gate()
        let preferences=UserDefaults(suiteName:"local.orbit.partialchecks.\(UUID())")!
        let cache=base.appendingPathComponent("normal")
        testingBatch.arm(fail:false)
        let model=DiskModel(cacheDirectory: cache,preferences:preferences)
        model.testScanner = DiskScanner(testBatch: { try testingBatch.wait() }, testFallback: { try testingFallback.wait() })
        model.scan(root)
        try await wait("provisional root") { testingBatch.isWaiting && model.current?.children.isEmpty == false }
        let partial=model.current!
        precondition(partial.children.count >= 512 && partial.children.count < 12000)
        let folder=partial.children.first!
        precondition(folder.isDirectory && !folder.isScanComplete && model.canCollect(folder))
        model.open(folder)
        try await wait("navigate to provisional folder") { model.current?.id == folder.id && !model.isOpeningFolder }
        model.goBack()
        try await wait("back to partial root") { model.current?.id == root.path && !model.isOpeningFolder }
        model.selectedID=folder.id
        let revision=model.mapRevision
        guard let drag = model.beginCollectionDrag([folder], label: folder.name) else { fatalError("partial folder not draggable") }
        let oldInteraction = model.beginMapInteraction()
        let currentInteraction = model.beginMapInteraction()
        oldInteraction()
        precondition(model.testMapInteractionActive)
        testingBatch.release()
        try await wait("save while interacting") { DiskIndexStore(directory: cache).savedSnapshot(for: root) != nil }
        try await Task.sleep(nanoseconds: 100_000_000)
        precondition(model.mapRevision == revision && model.isPartialMap, "gesture changed map")
        model.endCollectionDrag(drag)
        precondition(model.testMapInteractionActive && model.mapRevision == revision, "source completion changed the held map")
        currentInteraction()
        try await wait("complete") { !model.isScanning && !model.isPartialMap && !model.isOpeningFolder }
        precondition(model.selectedID == folder.id && model.current?.children.allSatisfy(\.isScanComplete) == true)
        model.cancelScan()
        print("PASS: pending collection available; provisional navigation/back, selection, held geometry and final handoff")
        for refresh in [false, true] {
            testingBatch.arm(fail: false)
            let reentrantCache = base.appendingPathComponent(refresh ? "refresh-reentrant" : "stop-reentrant")
            let reentrant = DiskModel(cacheDirectory: reentrantCache, preferences: preferences)
            reentrant.testScanner = DiskScanner(testBatch: { try testingBatch.wait() })
            reentrant.scan(root)
            try await wait("reentrant partial map") { testingBatch.isWaiting && reentrant.current?.children.isEmpty == false }
            let end = reentrant.beginMapInteraction()
            testingBatch.release()
            try await wait("reentrant saved map") { DiskIndexStore(directory: reentrantCache).savedSnapshot(for: root) != nil }
            try await Task.sleep(nanoseconds: 100_000_000)
            var entered = false
            var finishNew: (() -> Void)?
            let observer = reentrant.objectWillChange.sink { _ in
                guard !entered, !reentrant.testMapInteractionActive else { return }
                entered = true
                if refresh { reentrant.scan(root, force: true) } else { reentrant.cancelScan() }
                finishNew = reentrant.beginMapInteraction()
            }
            end()
            precondition(entered && reentrant.testMapInteractionActive, "Reentrant \(refresh ? "refresh" : "stop") lost the new interaction")
            finishNew?()
            observer.cancel()
            reentrant.cancelScan()
        }
        print("PASS: reentrant stop and refresh preserve a newly started interaction")
        for removed in [false, true] {
            let rollbackCache=base.appendingPathComponent(removed ? "removed" : "retained")
            testingBatch.arm(fail:true)
            testingFallback.arm(fail:false)
            let rollback=DiskModel(cacheDirectory: rollbackCache,preferences:preferences)
            rollback.testScanner = DiskScanner(testBatch: { try testingBatch.wait() }, testFallback: { try testingFallback.wait() })
            rollback.scan(root)
            try await wait("rollback preview") { testingBatch.isWaiting && rollback.current?.children.isEmpty == false }
            let provisional=rollback.current!.children.first { $0.isDirectory }!
            rollback.open(provisional)
            try await wait("open provisional folder") { rollback.current?.id == provisional.id && !rollback.isOpeningFolder }
            if removed { try fm.removeItem(at: provisional.url) }
            testingBatch.release()
            try await wait("fallback started") { testingFallback.isWaiting }
            try await Task.sleep(nanoseconds: 500_000_000)
            precondition(rollback.current?.id == provisional.id, "fallback temporarily changed the browse route")
            precondition(rollback.canCollect(provisional) == !removed, "fallback collection must reject a vanished folder")
            testingFallback.release()
            if removed {
                try await wait("rollback to root") { rollback.current?.id == root.path && !rollback.isOpeningFolder }
            }
            try await wait("rollback completes") { !rollback.isScanning && !rollback.isPartialMap && !rollback.isOpeningFolder }
            if removed {
                precondition(!rollback.current!.children.contains { $0.id == provisional.id })
            } else {
                precondition(rollback.current?.id == provisional.id, "confirmed existing route must be retained")
            }
            rollback.cancelScan()
            print("PASS: provisional route remains stable during fallback and handles confirmed \(removed ? "deletion" : "presence")")
        }
        let fileRoot = base.appendingPathComponent("model-file-only", isDirectory: true)
        try fm.createDirectory(at: fileRoot, withIntermediateDirectories: true)
        for i in 0..<2_500 { try Data([UInt8(i % 251)]).write(to: fileRoot.appendingPathComponent("file-\(i)")) }
        testingBatch.arm(fail: false)
        let files = DiskModel(cacheDirectory: base.appendingPathComponent("files-cache"), preferences: preferences)
        files.testScanner = DiskScanner(testBatch: { try testingBatch.wait() })
        files.scan(fileRoot)
        try await wait("provisional files") { testingBatch.isWaiting && files.current?.children.isEmpty == false }
        let file = files.current!.children.first!
        precondition(!file.isDirectory && !file.isScanComplete && files.canCollect(file))
        files.cancelScan()
        testingBatch.release()
        try await wait("cancel provisional files") { !files.isScanning }
        print("PASS: provisional files can be collected")
    }
}
