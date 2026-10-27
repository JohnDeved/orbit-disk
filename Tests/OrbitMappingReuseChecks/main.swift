import AppKit
import Darwin
import Foundation
import OrbitCore

private final class Gate: @unchecked Sendable {
    private let lock = NSLock()
    private let semaphore = DispatchSemaphore(value: 0)
    private var entered = false
    private var continuation: CheckedContinuation<Void, Never>?
    private var released = false
    var waiting: Bool { lock.lock(); defer { lock.unlock() }; return entered && !released }
    func pause() {
        lock.lock(); let wait = !entered && !released; entered = true; lock.unlock()
        if wait { semaphore.wait() }
    }
    func suspend() async {
        await withCheckedContinuation { continuation in
            lock.lock(); entered = true
            if released { lock.unlock(); continuation.resume() }
            else { self.continuation = continuation; lock.unlock() }
        }
    }
    func release() {
        lock.lock(); released = true; let pending = continuation; continuation = nil; lock.unlock()
        semaphore.signal(); pending?.resume()
    }
}

@main struct Checks {
    @MainActor static func wait(_ label: String, _ condition: () -> Bool) async throws {
        for _ in 0..<1_000 {
            if condition() { return }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        throw NSError(domain: label, code: 1)
    }

    static func populate(_ root: URL) throws {
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try Data([1, 2, 3]).write(to: root.appendingPathComponent("file"))
    }

    @MainActor static func rejectedSwitchbackChecks(_ base: URL, preferences: UserDefaults) async throws {
        let pending = base.appendingPathComponent("pending"), saved = base.appendingPathComponent("saved")
        try populate(pending); try populate(saved)
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("cache"), preferences: preferences)
        model.scan(saved)
        try await wait("switchback fixture saves its other map") { model.current?.id == saved.path && !model.isScanning && !model.isRefreshing }

        let attached = Gate()
        defer { attached.release(); model.cancelScan() }
        model.testAfterStageAttach = { attached.pause() }
        model.scan(pending)
        try await wait("switchback fixture holds a visible unfinished map") { attached.waiting && model.current?.id == pending.path }
        model.scan(saved)
        try await wait("switchback fixture leaves its worker running") { model.current?.id == saved.path && !model.isScanning && !model.isRefreshing }

        var rejectedPreview = false
        model.testBeforeLifecycleTransition = { phase in
            guard phase == "beginPreview", !rejectedPreview else { return }
            rejectedPreview = true
            model.hoveredListID = nil
        }
        model.scan(pending)
        try await wait("unfinished switchback rejects preview presentation") { rejectedPreview && model.testActiveSessionRoot() == pending.path }
        precondition(attached.waiting, "The original mapping worker must still be running")
        model.testBeforeLifecycleTransition = nil
        attached.release()
        try await wait("unfinished switchback still observes completion") { model.current?.id == pending.path && !model.isScanning && !model.isRefreshing }
        precondition(model.errorMessage == nil)

        let reopen = Gate()
        defer { reopen.release() }
        model.testBeforeSessionReopen = { await reopen.suspend() }
        var rejectedRestore = false
        model.testBeforeRootInstall = {
            guard !rejectedRestore else { return }
            rejectedRestore = true
            model.hoveredListID = nil
        }
        model.scan(saved)
        try await wait("saved switchback rejects remembered presentation") { rejectedRestore && reopen.waiting }
        precondition(model.isScanning, "The saved reopen must remain in flight while its worker is held")
        model.testBeforeRootInstall = nil
        reopen.release()
        try await wait("saved switchback still observes completion") { model.current?.id == saved.path && !model.isScanning && !model.isRefreshing }
        precondition(model.errorMessage == nil)
        print("PASS: rejected unfinished and saved switchback presentations retain their completion observers")
    }

    @MainActor static func main() async throws {
        setbuf(stdout, nil)
        let deadline = DispatchWorkItem { fputs("FAIL: mapping reuse timed out\n", stderr); exit(1) }
        DispatchQueue(label: "OrbitMappingReuse.timeout").asyncAfter(deadline: .now() + 90, execute: deadline)
        defer { deadline.cancel() }
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).standardizedFileURL
        let prefs = UserDefaults(suiteName: "mapping-reuse-\(UUID())")!
        let a = base.appendingPathComponent("a"), b = base.appendingPathComponent("b")
        try populate(a); try populate(b)
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("cache"), preferences: prefs)
        model.scan(a)
        do {
            try await wait("A finishes") { model.current?.id == a.path && !model.isScanning }
        } catch {
            fputs("FAIL: initial map expected=\(a.path) current=\(model.current?.id ?? "nil") scanning=\(model.isScanning) refreshing=\(model.isRefreshing) error=\(model.errorMessage ?? "nil")\n", stderr)
            throw error
        }
        let file = model.current!.children[0]
        model.selectedID = file.id
        model.scan(b)
        try await wait("B finishes") { model.current?.id == b.path && !model.isScanning }
        let reopen = Gate()
        model.testBeforeSessionReopen = { await reopen.suspend() }
        defer { reopen.release() }
        model.scan(a)
        try await wait("A restores before reading its saved index") { reopen.waiting && model.current?.id == a.path }
        precondition(model.selectedID == file.id && model.canCollect(file) && model.isScanning)
        let drag = model.beginCollectionDrag([file], label: file.name)!
        precondition(model.collectDrag(data: drag.data(forType: DiskModel.collectionDragType)!))
        try await wait("collect during saved reopen") { model.collection.count == 1 }
        model.deleteCollectionPermanently()
        try await wait("delete during saved reopen") { !model.isRemoving && !FileManager.default.fileExists(atPath: file.id) }
        precondition(reopen.waiting, "Deletion waited for saved-index loading")
        reopen.release()
        try await wait("reopen finishes") { !model.isScanning && !model.isRefreshing }
        model.cancelScan()
        print("PASS: cached session restores selection and allows drag/delete before saved-index loading finishes")

        let parent = base.appendingPathComponent("parent"), child = parent.appendingPathComponent("child")
        try populate(child)
        try populate(parent.appendingPathComponent("other"))
        let cache = base.appendingPathComponent("shared-cache")
        let captured = try DiskChangeMonitor.capture(root: parent)
        do {
            let stage = try ResumableStage(cache: cache, root: parent,
                current: StageCheckpoint(rootPath: parent.path, identity: captured.identity, eventID: captured.eventID))
            _ = try stage.advance(progress: { _ in })
        }
        let attached = Gate()
        let resumed = DiskModel(cacheDirectory: cache, preferences: prefs)
        resumed.testAfterStageAttach = { attached.pause() }
        defer { attached.release() }
        resumed.scan(child)
        try await wait("new model finds parent stage") { attached.waiting && resumed.testActiveSessionRoot() == parent.path && resumed.current?.id == child.path }
        precondition(resumed.isPartialMap && !ResumableStage.hasCandidate(cache: cache, root: child))
        resumed.cancelScan(); attached.release()
        let again = Gate()
        resumed.testAfterStageAttach = { again.pause() }
        var rejectInitialPreview = true
        resumed.testBeforeLifecycleTransition = { phase in
            guard phase == "beginPreview", rejectInitialPreview else { return }
            rejectInitialPreview = false
            resumed.hoveredListID = nil
        }
        defer { again.release() }
        resumed.scan(child)
        try await wait("stopped parent resumes for child") { again.waiting && resumed.testActiveSessionRoot() == parent.path && resumed.current?.id == child.path && resumed.isScanning }
        precondition(!rejectInitialPreview, "The first resumed preview must reject its presentation transaction")
        resumed.testBeforeLifecycleTransition = nil
        precondition(!ResumableStage.hasCandidate(cache: cache, root: child))
        again.release()
        do {
            try await wait("shared parent finishes") { !resumed.isScanning && !resumed.isRefreshing && resumed.current?.id == child.path }
        } catch {
            fputs("FAIL: shared parent current=\(resumed.current?.id ?? "nil") scanning=\(resumed.isScanning) refreshing=\(resumed.isRefreshing) error=\(resumed.errorMessage ?? "nil")\n", stderr)
            throw error
        }
        let store = DiskIndexStore(directory: cache)
        guard store.savedSnapshot(for: parent) != nil && store.savedSnapshot(for: child) == nil else {
            throw NSError(domain: "Only the shared parent should have a saved map", code: 1)
        }
        print("PASS: rejected initial preview still observes the resumed parent through completion")
        resumed.cancelScan()
        let restarted = DiskModel(cacheDirectory: cache, preferences: prefs)
        restarted.testAfterStageAttach = {
            fputs("FAIL: saved ancestor started a new scan\n", stderr)
            preconditionFailure("Saved ancestor should not start a new scan")
        }
        restarted.scan(child)
        do {
            try await wait("new model uses saved ancestor") { restarted.current?.id == child.path && !restarted.isScanning && !restarted.isRefreshing }
        } catch {
            fputs("FAIL: saved ancestor current=\(restarted.current?.id ?? "nil") scanning=\(restarted.isScanning) refreshing=\(restarted.isRefreshing) error=\(restarted.errorMessage ?? "nil")\n", stderr)
            throw error
        }
        precondition(restarted.testActiveSessionRoot() == parent.path)
        restarted.cancelScan()
        print("PASS: relaunched and stopped child routes reuse the parent stage, then its saved map, without a child scan")
        try await rejectedSwitchbackChecks(base.appendingPathComponent("switchback"), preferences: prefs)
    }
}
