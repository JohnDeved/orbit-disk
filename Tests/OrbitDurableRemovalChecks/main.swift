import AppKit
import Darwin
import Foundation
import OrbitCore

final class Gate: @unchecked Sendable {
    private let lock = NSLock(), signal = DispatchSemaphore(value: 0)
    private var used = false, blocked = false, calls = 0
    var waiting: Bool { lock.lock(); defer { lock.unlock() }; return blocked }
    func pauseOnce() {
        lock.lock(); let shouldWait = !used; used = true; if shouldWait { blocked = true }; lock.unlock()
        if shouldWait { signal.wait(); lock.lock(); blocked = false; lock.unlock() }
    }
    func release() { signal.signal() }
    func pauseSecond() {
        lock.lock(); calls += 1; let shouldWait = calls == 2; if shouldWait { blocked = true }; lock.unlock()
        if shouldWait { signal.wait(); lock.lock(); blocked = false; lock.unlock() }
    }
}
final class BlockingGate: @unchecked Sendable {
    private let condition = NSCondition()
    private var open = false, count = 0
    var waiting: Bool { condition.lock(); defer { condition.unlock() }; return count > 0 }
    func wait() {
        condition.lock(); count += 1
        while !open { condition.wait() }
        count -= 1; condition.unlock()
    }
    func release() { condition.lock(); open = true; condition.broadcast(); condition.unlock() }
}

@main struct DurableRemovalChecks {
    @MainActor static func wait(_ label: String, _ condition: () -> Bool) async throws {
        for _ in 0..<1000 { if condition() { return }; try await Task.sleep(nanoseconds: 20_000_000) }
        throw NSError(domain: label, code: 1)
    }
    @MainActor static func main() async throws {
        let args = CommandLine.arguments
        if args.count == 4 && args[1] == "bulk-remove-and-die" {
            let root = URL(fileURLWithPath: args[2]), cache = URL(fileURLWithPath: args[3])
            let store = DiskIndexStore(directory: cache)
            guard let saved = store.savedSnapshot(for: root), let rootIdentity = try store.node(path: root.path, snapshot: saved)?.fileIdentity else { throw NSError(domain: "bulk snapshot", code: 1) }
            let folder = try store.loadFolder(path: root.appendingPathComponent("target").path, snapshot: saved)
            let removals = folder.children.compactMap { node in node.fileIdentity.map { ScanRemoval(path: node.id, identity: $0) } }
            guard removals.count == 10_001 else { throw NSError(domain: "bulk fixture", code: 1) }
            let operation = try RemovalJournal(cache: cache).begin(root: root, rootIdentity: rootIdentity, items: removals)
            for node in folder.children { try FileManager.default.removeItem(at: node.url) }
            _ = withExtendedLifetime(operation) { kill(getpid(), SIGKILL) }
            return
        }
        if args.count == 5 && args[1] == "remove-and-die" {
            let root = URL(fileURLWithPath: args[2]), cache = URL(fileURLWithPath: args[3]), staged = args[4] == "stage"
            let model = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-child-\(UUID())")!)
            model.testMonitorUnavailable = true
            let gate = Gate()
            if staged {
                model.testAfterStageStep = { stage in
                    guard let view = try? stage.view(path: nil, removals: [], cancellation: ScanCancellation()),
                          view.folder.children.count == 2, !view.isListing else { return }
                    gate.pauseOnce()
                }
            }
            model.scan(root)
            let victim = root.appendingPathComponent("victim")
            try await wait("child map") {
                (staged ? gate.waiting : !model.isScanning) && model.current?.children.contains(where: { $0.id == victim.path }) == true
            }
            if staged { model.cancelScan(); gate.release(); try await wait("child stopped") { !gate.waiting } }
            guard let node = model.current?.children.first(where: { $0.id == victim.path }), model.collect([node]) else { throw NSError(domain: "child collect", code: 1) }
            model.deleteCollectionPermanently()
            try await wait("child removal") { !model.isRemoving && !FileManager.default.fileExists(atPath: victim.path) }
            kill(getpid(), SIGKILL)
            return
        }
        guard args.count == 2 || args.count == 3 else { throw NSError(domain: "expected fixture path", code: 1) }
        let base = URL(fileURLWithPath: args[1]), fm = FileManager.default
        if args.count == 3 && args[2] == "direct-large" {
            let root = base.appendingPathComponent("direct/root"), cache = base.appendingPathComponent("direct/cache")
            try fm.createDirectory(at: root, withIntermediateDirectories: true)
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            for index in 0..<10_001 { try Data([1]).write(to: root.appendingPathComponent("file-\(index)")) }
            let cp = try DiskChangeMonitor.capture(root: root), store = DiskIndexStore(directory: cache)
            _ = try store.save(DiskScanner().scan(root), eventID: cp.eventID, eventIdentity: cp.identity)
            let model = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-direct-\(UUID())")!)
            model.testMonitorUnavailable = true
            let gate = Gate(); model.testBeforeRefreshSave = { gate.pauseOnce() }
            model.scan(root)
            try await wait("direct map") { model.current?.id == root.path && !model.isScanning && !model.isRefreshing }
            guard model.collect(model.current!.children) else { throw NSError(domain: "direct collection", code: 1) }
            model.deleteCollectionPermanently()
            try await wait("direct large removal") { !model.isRemoving && gate.waiting && model.root == nil }
            gate.release()
            try await wait("direct large reconciliation") { model.current?.id == root.path && model.current?.children.isEmpty == true && !model.isRefreshing }
            model.cancelScan()
        }
        if args.count == 3 && args[2] == "many-parents" {
            let root = base.appendingPathComponent("parents/root"), cache = base.appendingPathComponent("parents/cache")
            try fm.createDirectory(at: root, withIntermediateDirectories: true)
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            var removals: [ScanRemoval] = []
            for index in 0..<7 {
                let dir = root.appendingPathComponent("dir-\(index)")
                try fm.createDirectory(at: dir, withIntermediateDirectories: true)
                let file = dir.appendingPathComponent("item")
                try Data([1]).write(to: file)
                var info = stat()
                guard lstat(file.path, &info) == 0 else { throw NSError(domain: "parent identity", code: 1) }
                removals.append(ScanRemoval(path: file.path, identity: "\(info.st_dev):\(info.st_ino)"))
            }
            let cp = try DiskChangeMonitor.capture(root: root), store = DiskIndexStore(directory: cache)
            let saved = try store.save(DiskScanner().scan(root), eventID: cp.eventID, eventIdentity: cp.identity)
            guard let identity = try store.node(path: root.path, snapshot: saved)?.fileIdentity else { throw NSError(domain: "parent root", code: 1) }
            let model = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-parents-\(UUID())")!)
            model.testMonitorUnavailable = true
            model.testRecoveryParentLimit = 4
            let gate = Gate(); model.testBeforeRefreshSave = { gate.pauseOnce() }
            model.scan(root)
            try await wait("parents map") { model.current?.id == root.path && !model.isScanning && !model.isRefreshing }
            let journal = try RemovalJournal(cache: cache)
            let first = try journal.begin(root: root, rootIdentity: identity, items: Array(removals.prefix(4)))
            try first.complete()
            try await wait("parents first refresh") { gate.waiting }
            let second = try journal.begin(root: root, rootIdentity: identity, items: Array(removals.dropFirst(4)))
            try second.complete()
            try await wait("parents overflow withheld") { model.root == nil }
            gate.release()
            try await wait("parents fallback complete") { model.current?.id == root.path && !model.isScanning && !model.isRefreshing }
            guard model.current?.children.count == 7 else { throw NSError(domain: "parent fallback lost folders", code: 1) }
            model.cancelScan()
        }
        if args.count == 3 && args[2] == "deferred" {
            let root = base.appendingPathComponent("deferred/root"), cache = base.appendingPathComponent("deferred/cache")
            try fm.createDirectory(at: root.appendingPathComponent("pending"), withIntermediateDirectories: true)
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            let victim = root.appendingPathComponent("victim")
            try Data([1]).write(to: victim); try Data([2]).write(to: root.appendingPathComponent("pending/child"))
            let model = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-deferred-\(UUID())")!)
            model.testMonitorUnavailable = true
            let stageGate = Gate()
            model.testAfterStageStep = { stage in
                guard let view = try? stage.view(path: nil, removals: [], cancellation: ScanCancellation()),
                      view.folder.children.count == 2, !view.isListing else { return }
                stageGate.pauseOnce()
            }
            model.scan(root)
            try await wait("deferred map") { stageGate.waiting && model.current?.children.contains(where: { $0.id == victim.path }) == true }
            guard let rootIdentity = model.root?.fileIdentity,
                  let node = model.current?.children.first(where: { $0.id == victim.path }), let identity = node.fileIdentity else {
                throw NSError(domain: "deferred identities", code: 1)
            }
            model.setMapInteractionActive(true)
            model.testRefreshPreview()
            try await wait("old preview deferred") { model.testHasDeferredPreview }
            let revision = model.mapRevision, readGate = BlockingGate()
            model.testBeforePreviewRead = { readGate.wait() }
            let operation = try RemovalJournal(cache: cache).begin(root: root, rootIdentity: rootIdentity, items: [ScanRemoval(path: victim.path, identity: identity)])
            try fm.removeItem(at: victim); try operation.complete()
            try await wait("new preview gated") { readGate.waiting && !model.testHasDeferredPreview }
            model.setMapInteractionActive(false)
            guard model.mapRevision == revision else { throw NSError(domain: "stale deferred preview installed", code: 1) }
            readGate.release(); model.testBeforePreviewRead = {}
            try await wait("deferred removal shown") { model.current?.children.contains(where: { $0.id == victim.path }) == false }
            model.cancelScan(); stageGate.release()
        }
        if args.count == 3 && args[2] == "journal-replaced" {
            let root = base.appendingPathComponent("identity/root"), cache = base.appendingPathComponent("identity/cache")
            try fm.createDirectory(at: root, withIntermediateDirectories: true)
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            let victim = root.appendingPathComponent("victim")
            try Data([1]).write(to: victim)
            let first = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "identity-first-\(UUID())")!)
            first.testMonitorUnavailable = true; first.scan(root)
            try await wait("identity initial map") { first.root?.id == root.path && !first.isScanning && !first.isRefreshing }
            first.cancelScan()
            let store = DiskIndexStore(directory: cache), journal = store.lockDirectory.appendingPathComponent(".orbit-removals")
            try fm.moveItem(at: journal, to: store.lockDirectory.appendingPathComponent("old-journal"))
            try fm.removeItem(at: victim)
            let second = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "identity-second-\(UUID())")!)
            second.testMonitorUnavailable = true; second.scan(root)
            try await wait("identity rebuilt map") { second.root?.id == root.path && !second.isScanning && !second.isRefreshing }
            guard second.current?.children.contains(where: { $0.id == victim.path }) == false else { throw NSError(domain: "journal replacement showed stale row", code: 1) }
            second.cancelScan()
        }
        if args.count == 3 && (args[2] == "cancel-copy" || args[2] == "cancel-rename") {
            let root = base.appendingPathComponent("cancel/root"), cache = base.appendingPathComponent("cancel/cache")
            for name in ["a", "b"] {
                let dir = root.appendingPathComponent(name); try fm.createDirectory(at: dir, withIntermediateDirectories: true)
                try Data([1]).write(to: dir.appendingPathComponent("victim"))
            }
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            let cp = try DiskChangeMonitor.capture(root: root), store = DiskIndexStore(directory: cache)
            let saved = try store.save(DiskScanner().scan(root), eventID: cp.eventID, eventIdentity: cp.identity)
            let rootNode = try store.loadFolder(path: root.path, snapshot: saved)
            guard let identity = rootNode.fileIdentity else { throw NSError(domain: "cancel root", code: 1) }
            let targets = try ["a", "b"].map { try store.loadFolder(path: root.appendingPathComponent($0).path, snapshot: saved).children[0] }
            let entries = targets.compactMap { node in node.fileIdentity.map { ScanRemoval(path: node.id, identity: $0) } }
            let model = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-cancel-\(UUID())")!)
            model.testMonitorUnavailable = true
            let gate = Gate()
            if args[2] == "cancel-copy" { model.testBeforeRefreshSave = { gate.pauseSecond() } }
            else { DiskIndexStore.testBeforeRecoveryPublish = { gate.pauseOnce() } }
            defer { DiskIndexStore.testBeforeRecoveryPublish = nil }
            model.scan(root)
            try await wait("cancel map") { model.current?.id == root.path && !model.isScanning && !model.isRefreshing }
            let operation = try RemovalJournal(cache: cache).begin(root: root, rootIdentity: identity, items: entries)
            for node in targets { try fm.removeItem(at: node.url) }
            try operation.complete()
            try await wait("private recovery publication") { gate.waiting }
            guard store.savedSnapshot(for: root)?.id == saved.id else { throw NSError(domain: "published intermediate snapshot", code: 1) }
            model.cancelScan(); gate.release()
            try await wait("cancel worker released") { !gate.waiting }
            guard store.savedSnapshot(for: root)?.id == saved.id else { throw NSError(domain: "cancel changed published snapshot", code: 1) }
            model.rescan()
            try await wait("cancel retry") { model.current?.id == root.path && !model.isScanning && !model.isRefreshing }
            guard let final = store.savedSnapshot(for: root), try ["a", "b"].allSatisfy({ try store.loadFolder(path: root.appendingPathComponent($0).path, snapshot: final).children.isEmpty }) else {
                throw NSError(domain: "cancel retry stale", code: 1)
            }
            model.cancelScan()
        }
        if args.count == 3 && args[2] == "stopped-overflow" {
            let root = base.appendingPathComponent("stopped/root"), cache = base.appendingPathComponent("stopped/cache")
            try fm.createDirectory(at: root, withIntermediateDirectories: true)
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            for index in 0..<10_001 { try Data([1]).write(to: root.appendingPathComponent("file-\(index)")) }
            let model = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-stopped-\(UUID())")!)
            model.testMonitorUnavailable = true
            let gate = Gate()
            model.testAfterStageStep = { stage in
                guard let view = try? stage.view(path: nil, removals: [], cancellation: ScanCancellation()),
                      view.folder.children.count == 10_001, !view.isListing else { return }
                gate.pauseOnce()
            }
            model.scan(root)
            try await wait("stopped partial map") { gate.waiting && model.current?.children.count == 10_001 }
            guard let rootIdentity = model.root?.fileIdentity else { throw NSError(domain: "stopped identity", code: 1) }
            let all = model.current!.children.compactMap { node in node.fileIdentity.map { ScanRemoval(path: node.id, identity: $0) } }
            guard all.count == 10_001 else { throw NSError(domain: "stopped fixture", code: 1) }
            model.cancelScan(); gate.release()
            try await wait("stopped gate released") { !gate.waiting }
            let operation = try RemovalJournal(cache: cache).begin(root: root, rootIdentity: rootIdentity, items: all)
            for removal in all { try fm.removeItem(atPath: removal.path) }
            try operation.complete()
            try await wait("stopped overflow withheld") { model.root == nil }
            model.rescan()
            try await wait("stopped overflow resumed") { model.current?.id == root.path && model.current?.children.isEmpty == true && !model.isScanning && !model.isRefreshing }
            model.cancelScan()
        }
        if args.count == 3 && args[2] == "cumulative" {
            let root = base.appendingPathComponent("cumulative/root"), cache = base.appendingPathComponent("cumulative/cache")
            try fm.createDirectory(at: root, withIntermediateDirectories: true)
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            for index in 0..<10_001 { try Data([1]).write(to: root.appendingPathComponent("file-\(index)")) }
            let checkpoint = try DiskChangeMonitor.capture(root: root)
            let store = DiskIndexStore(directory: cache)
            let baseline = try store.save(DiskScanner().scan(root), eventID: checkpoint.eventID, eventIdentity: checkpoint.identity)
            let folder = try store.loadFolder(path: root.path, snapshot: baseline)
            guard let rootIdentity = folder.fileIdentity else { throw NSError(domain: "cumulative identity", code: 1) }
            let all = folder.children.compactMap { node in node.fileIdentity.map { ScanRemoval(path: node.id, identity: $0) } }
            guard all.count == 10_001 else { throw NSError(domain: "cumulative fixture", code: 1) }
            let model = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-cumulative-\(UUID())")!)
            model.testMonitorUnavailable = true
            let gate = Gate(); model.testBeforeRefreshSave = { gate.pauseOnce() }
            model.scan(root)
            try await wait("cumulative map") { model.current?.id == root.path && !model.isScanning && !model.isRefreshing }
            let journal = try RemovalJournal(cache: cache)
            let first = Array(all.prefix(6_000)), second = Array(all.dropFirst(6_000))
            let firstOperation = try journal.begin(root: root, rootIdentity: rootIdentity, items: first)
            for removal in first { try fm.removeItem(atPath: removal.path) }
            try firstOperation.complete()
            try await wait("cumulative first refresh") { gate.waiting }
            let secondOperation = try journal.begin(root: root, rootIdentity: rootIdentity, items: second)
            for removal in second { try fm.removeItem(atPath: removal.path) }
            try secondOperation.complete()
            try await wait("cumulative overflow withheld") { model.root == nil }
            gate.release()
            try await wait("cumulative map reconciled") { model.current?.id == root.path && model.current?.children.isEmpty == true && !model.isRefreshing }
            guard let saved = store.savedSnapshot(for: root), try store.loadFolder(path: root.path, snapshot: saved).children.isEmpty else {
                throw NSError(domain: "cumulative stale snapshot", code: 1)
            }
            model.cancelScan()
        }
        if args.count == 3 && args[2] == "large" {
            let root = base.appendingPathComponent("large/root"), target = root.appendingPathComponent("target"), cache = base.appendingPathComponent("large/cache")
            try fm.createDirectory(at: target, withIntermediateDirectories: true)
            try fm.createDirectory(at: root.appendingPathComponent("untouched"), withIntermediateDirectories: true)
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            for index in 0..<10_001 { try Data([1]).write(to: target.appendingPathComponent("file-\(index)")) }
            try Data([7]).write(to: root.appendingPathComponent("untouched/keep"))
            let checkpoint = try DiskChangeMonitor.capture(root: root)
            _ = try DiskIndexStore(directory: cache).save(DiskScanner().scan(root), eventID: checkpoint.eventID, eventIdentity: checkpoint.identity)
            let child = Process(); child.executableURL = URL(fileURLWithPath: args[0]).standardizedFileURL
            child.arguments = ["bulk-remove-and-die", root.path, cache.path]
            try child.run(); child.waitUntilExit()
            guard child.terminationReason == .uncaughtSignal, child.terminationStatus == SIGKILL else { throw NSError(domain: "bulk child", code: 1) }
            let reopened = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-large-\(UUID())")!)
            reopened.testMonitorUnavailable = true
            let scannerGate = Gate(); reopened.testBeforeScannerStart = { scannerGate.pauseOnce() }
            reopened.scan(root)
            try await wait("large parent reconciliation") { reopened.root?.id == root.path && !reopened.isScanning && !reopened.isRefreshing }
            guard !scannerGate.waiting else { scannerGate.release(); throw NSError(domain: "large recovery started full stage scan", code: 1) }
            let store = DiskIndexStore(directory: cache)
            guard let saved = store.savedSnapshot(for: root), try store.loadFolder(path: target.path, snapshot: saved).children.isEmpty,
                  try store.loadFolder(path: root.appendingPathComponent("untouched").path, snapshot: saved).children.count == 1 else {
                throw NSError(domain: "large recovery published stale rows", code: 1)
            }
            reopened.cancelScan()
        }
        if args.count == 2 || args[2] == "live" {
            let root = base.appendingPathComponent("live/root"), cache = base.appendingPathComponent("live/cache")
            try fm.createDirectory(at: root, withIntermediateDirectories: true)
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            let one = root.appendingPathComponent("one"), two = root.appendingPathComponent("two"), failed = root.appendingPathComponent("failed")
            try Data([1]).write(to: one); try Data([2]).write(to: two); try Data([3]).write(to: failed)
            let model = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-live-\(UUID())")!)
            model.testMonitorUnavailable = true
            model.scan(root)
            try await wait("live map") { model.root?.id == root.path && !model.isScanning && !model.isRefreshing }
            guard let rootIdentity = model.root?.fileIdentity,
                  let first = model.current?.children.first(where: { $0.id == one.path }), let firstIdentity = first.fileIdentity,
                  let second = model.current?.children.first(where: { $0.id == two.path }), let secondIdentity = second.fileIdentity,
                  let failedNode = model.current?.children.first(where: { $0.id == failed.path }), let failedIdentity = failedNode.fileIdentity else {
                throw NSError(domain: "live identities", code: 1)
            }
            let gate = Gate()
            model.testBeforeRefreshSave = { gate.pauseOnce() }
            let journal = try RemovalJournal(cache: cache)
            let operation = try journal.begin(root: root, rootIdentity: rootIdentity, items: [ScanRemoval(path: one.path, identity: firstIdentity)])
            let heldOperation = try journal.begin(root: root, rootIdentity: rootIdentity, items: [ScanRemoval(path: failed.path, identity: failedIdentity)])
            try await wait("live overlay") {
                model.current?.children.contains(where: { $0.id == one.path || $0.id == failed.path }) == false
            }
            let liveEpoch = model.testRemovalEpoch, pollCount = model.testJournalPollCount
            try await wait("unchanged live polls") { model.testJournalPollCount >= pollCount + 2 }
            guard model.testRemovalEpoch == liveEpoch else { throw NSError(domain: "unchanged live poll invalidated map", code: 1) }
            try fm.removeItem(at: one); try operation.complete()
            let afterFirst = model.testJournalPollCount
            try await wait("overlapping live polls") { model.testJournalPollCount >= afterFirst + 2 }
            guard model.current?.children.contains(where: { $0.id == failed.path }) == false else {
                throw NSError(domain: "live barrier passed an overlapping operation", code: 1)
            }
            try heldOperation.complete()
            try await wait("first refresh gated") { gate.waiting }
            let secondOperation = try journal.begin(root: root, rootIdentity: rootIdentity, items: [ScanRemoval(path: two.path, identity: secondIdentity)])
            try fm.removeItem(at: two); try secondOperation.complete()
            gate.release()
            try await wait("later removal overlay") { model.current?.children.contains(where: { $0.id == two.path }) == false }
            let store = DiskIndexStore(directory: cache)
            try await wait("refresh retry published both removals") {
                guard let saved = store.savedSnapshot(for: root),
                      let folder = try? store.loadFolder(path: root.path, snapshot: saved) else { return false }
                return !folder.children.contains(where: { $0.id == one.path || $0.id == two.path })
            }
            let failedOperation = try journal.begin(root: root, rootIdentity: rootIdentity, items: [ScanRemoval(path: failed.path, identity: failedIdentity)])
            try await wait("pending intent overlay") { model.current?.children.contains(where: { $0.id == failed.path }) == false }
            try await Task.sleep(nanoseconds: 1_300_000_000)
            try failedOperation.complete()
            try await wait("failed removal restored") { model.current?.children.contains(where: { $0.id == failed.path }) == true }
            try await wait("failed removal saved") {
                guard let saved = store.savedSnapshot(for: root), let folder = try? store.loadFolder(path: root.path, snapshot: saved) else { return false }
                return folder.children.contains(where: { $0.id == failed.path })
            }
            var abandoned: RemovalJournal.Operation? = try journal.begin(root: root, rootIdentity: rootIdentity,
                items: [ScanRemoval(path: failed.path, identity: failedIdentity)])
            try await wait("abandoned intent overlay") { model.current?.children.contains(where: { $0.id == failed.path }) == false }
            let abandonedPoll = model.testJournalPollCount
            try await wait("abandoned intent held across polls") { model.testJournalPollCount >= abandonedPoll + 2 }
            withExtendedLifetime(abandoned) {}
            abandoned = nil
            try await wait("abandoned intent restored") { model.current?.children.contains(where: { $0.id == failed.path }) == true }
            model.cancelScan()
        }
        for mode in ["stage", "stage-recreated", "saved", "overlap"] where args.count == 2 || mode == args[2] {
            let container = base.appendingPathComponent(mode), root = container.appendingPathComponent("root")
            let selected = mode == "overlap" ? root.appendingPathComponent("nested") : root
            let cache = container.appendingPathComponent("cache"), victim = selected.appendingPathComponent("victim")
            try fm.createDirectory(at: selected.appendingPathComponent("pending-folder"), withIntermediateDirectories: true)
            try fm.createDirectory(at: cache, withIntermediateDirectories: true)
            try Data([1]).write(to: victim)
            if mode == "stage-recreated" { try fm.linkItem(at: victim, to: container.appendingPathComponent("old-inode")) }
            try Data([2]).write(to: selected.appendingPathComponent("pending-folder/child"))
            if !mode.hasPrefix("stage") {
                for mapRoot in mode == "overlap" ? [root, selected] : [root] {
                    let cp = try DiskChangeMonitor.capture(root: mapRoot)
                    _ = try DiskIndexStore(directory: cache).save(DiskScanner().scan(mapRoot), eventID: cp.eventID, eventIdentity: cp.identity)
                }
            }
            let child = Process(); child.executableURL = URL(fileURLWithPath: args[0]).standardizedFileURL
            child.arguments = ["remove-and-die", selected.path, cache.path, mode.hasPrefix("stage") ? "stage" : mode]
            try child.run(); child.waitUntilExit()
            guard child.terminationReason == .uncaughtSignal, child.terminationStatus == SIGKILL else { throw NSError(domain: "child did not die after \(mode) removal", code: 1) }
            if mode == "stage-recreated" { try Data(repeating: 7, count: 7).write(to: victim) }
            let reopened = DiskModel(cacheDirectory: cache, preferences: UserDefaults(suiteName: "durable-parent-\(UUID())")!)
            reopened.testMonitorUnavailable = true
            reopened.scan(root)
            try await wait("reopen \(mode)") { reopened.root?.id == root.path && !reopened.isScanning && !reopened.isRefreshing }
            if mode == "overlap" {
                reopened.open(reopened.current!.children.first { $0.id == selected.path }!)
                try await wait("open overlapping folder") { reopened.current?.id == selected.path && !reopened.isOpeningFolder }
            }
            let visibleVictim = reopened.current?.children.first(where: { $0.id == victim.path })
            guard visibleVictim == nil || (mode == "stage-recreated" && visibleVictim?.size == 7) else {
                throw NSError(domain: "stale deleted file after \(mode) relaunch without event history", code: 1)
            }
            let store = DiskIndexStore(directory: cache)
            try await wait("durable reconciliation \(mode)") {
                guard let saved = store.savedSnapshot(for: root),
                      let result = try? store.loadFolder(path: selected.path, snapshot: saved) else { return false }
                return mode == "stage-recreated" ? result.children.contains(where: { $0.id == victim.path && $0.size == 7 })
                    : !result.children.contains(where: { $0.id == victim.path })
            }
            reopened.cancelScan()
        }
    }
}
