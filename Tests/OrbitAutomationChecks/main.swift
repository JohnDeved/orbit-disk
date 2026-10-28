import AppKit
import Foundation
import OrbitCore
import OrbitAutomation

private final class Gate: @unchecked Sendable {
    private let lock = NSLock()
    private let semaphore = DispatchSemaphore(value: 0)
    private var used = false
    private var blocked = false
    var waiting: Bool { lock.lock(); defer { lock.unlock() }; return blocked }
    func pause() {
        lock.lock()
        let shouldWait = !used
        used = true
        if shouldWait { blocked = true }
        lock.unlock()
        if shouldWait {
            semaphore.wait()
            lock.lock(); blocked = false; lock.unlock()
        }
    }
    func release() { semaphore.signal() }
}

@main
struct AutomationChecks {
    @MainActor private static var diagnostics: () -> String = { "" }
    @MainActor
    static func wait(_ label: String, _ condition: () -> Bool) async throws {
        for _ in 0..<1_000 {
            if condition() { return }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        print("TIMEOUT: \(label): \(diagnostics())")
        throw NSError(domain: "Timed out: " + label, code: 1)
    }

    @MainActor
    static func request(_ controller: AutomationController, _ command: String, _ arguments: [String: Any] = [:]) -> [String: Any] {
        let data = try! JSONSerialization.data(withJSONObject: ["command": command, "arguments": arguments])
        return controller.handle(data)
    }

    @MainActor
    @discardableResult
    static func success(_ controller: AutomationController, _ command: String, _ arguments: [String: Any] = [:]) -> [String: Any] {
        let response = request(controller, command, arguments)
        precondition(response["ok"] as? Bool == true, "\(command): \(response)")
        return response["result"] as! [String: Any]
    }

    @MainActor
    static func rejected(_ controller: AutomationController, _ command: String, _ arguments: [String: Any], _ code: String) {
        let response = request(controller, command, arguments)
        precondition(response["ok"] as? Bool == false, "Accepted \(command): \(arguments)")
        precondition((response["error"] as? [String: Any])?["code"] as? String == code, "\(command): \(response)")
    }

    @MainActor
    static func context(_ controller: AutomationController) -> String { success(controller, "status")["context"] as! String }

    static func item(_ node: DiskNode) -> [String: Any] { ["path": node.id, "identity": node.fileIdentity!] }

    @MainActor
    static func ready(_ model: DiskModel) -> Bool { !model.isValidatingCollection && !model.isUpdatingCollection }

    @MainActor
    static func main() async throws {
        setbuf(stdout, nil)
        for (error, code) in [(LocalAutomationError.timeout, "request_timeout"), (.disconnected, "connection_closed")] {
            let failure = AutomationCatalog.failure(error)["error"] as! [String: Any]
            precondition(failure["code"] as? String == code && failure["outcomeUnknown"] as? Bool == true)
        }
        let peerFailure = AutomationCatalog.failure(LocalAutomationError.peerMismatch)["error"] as! [String: Any]
        precondition(peerFailure["code"] as? String == "peer_mismatch" && peerFailure["outcomeUnknown"] == nil)
        try AutomationCatalog.validate(command: "search", arguments: ["context": "x", "query": String(repeating: "é", count: 4096)])
        do {
            try AutomationCatalog.validate(command: "search", arguments: ["context": "x", "query": String(repeating: "é", count: 4097)])
            preconditionFailure("Accepted a query beyond the schema limit")
        } catch let failure as AutomationCatalog.Failure { precondition(failure.code == "invalid_arguments") }
        print("PASS: transport uncertainty and Unicode schema limits")
        let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let fm = FileManager.default
        let a = directory.appendingPathComponent("a"), b = directory.appendingPathComponent("b")
        let first = a.appendingPathComponent("first"), second = a.appendingPathComponent("second")
        let nativeLater = a.appendingPathComponent("native-later")
        let outside = b.appendingPathComponent("untouched")
        let folder = b.appendingPathComponent("folder")
        try fm.createDirectory(at: a, withIntermediateDirectories: true)
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        try Data([1]).write(to: first)
        try Data([2, 2]).write(to: second)
        try Data().write(to: nativeLater)
        try Data([9]).write(to: outside)
        let suite = "automation-check-\(UUID().uuidString)"
        let preferences = UserDefaults(suiteName: suite)!
        defer { preferences.removePersistentDomain(forName: suite) }
        let model = DiskModel(cacheDirectory: directory.appendingPathComponent("index"), preferences: preferences)
        let controller = AutomationController(model: model)
        diagnostics = { String(describing: success(controller, "status")) }
        defer { controller.stop(); model.cancelScan() }

        rejected(controller, "scan", ["path": "relative"], "invalid_path")
        rejected(controller, "list", ["limit": true], "invalid_arguments")
        rejected(controller, "remove", ["token": "unknown", "confirmed": false], "invalid_arguments")
        rejected(controller, "remove", ["token": "unknown", "confirmed": true], "invalid_plan")
        rejected(controller, "search", ["context": "unknown", "query": "first"], "stale_context")
        success(controller, "scan", ["path": a.path])
        try await wait("initial map") { model.current?.id == a.path && !model.isOpeningFolder && !model.isScanning }
        let initialContext = context(controller)
        let oldFirst = item(model.current!.children.first { $0.name == "first" }!)
        let secondItem = item(model.current!.children.first { $0.name == "second" }!)
        let page = success(controller, "list", ["limit": 1])
        precondition(page["total"] as? Int == 3)
        precondition((page["items"] as! [[String: Any]])[0]["path"] as? String == second.path)
        let cursor = page["nextCursor"] as! String
        let nextPage = success(controller, "list", ["limit": 1, "cursor": cursor])
        precondition((nextPage["items"] as! [[String: Any]])[0]["path"] as? String == first.path)
        rejected(controller, "select", ["context": initialContext, "item": ["path": first.path, "identity": "wrong"]], "stale_item")
        success(controller, "select", ["context": initialContext, "item": oldFirst])
        precondition(model.selectedID == first.path)
        success(controller, "search", ["context": initialContext, "query": "second"])
        rejected(controller, "list", ["cursor": cursor], "stale_cursor")
        success(controller, "search", ["context": initialContext, "query": ""])
        print("PASS: validated schema, identity selection and immutable list cursors")

        let opening = Gate()
        model.testBeforeResolution = { opening.pause() }
        success(controller, "scan", ["path": b.path])
        try await wait("opening map") { opening.waiting }
        precondition(success(controller, "status")["context"] is NSNull)
        rejected(controller, "collect", ["context": initialContext, "items": [oldFirst]], "stale_context")
        opening.release()
        try await wait("second map") { model.root?.id == b.path && !model.isOpeningFolder && !model.isScanning }
        success(controller, "select", ["context": context(controller), "item": item(model.current!.children.first { $0.id == outside.path }!)])
        rejected(controller, "preview", ["context": context(controller), "item": item(model.current!.children.first { $0.id == folder.path }!)], "not_file")
        precondition(model.selectedID == outside.path && model.previewURL == nil)
        print("PASS: invalid Quick Look request preserves the selected item")
        for command in ["select", "open", "reveal", "preview"] {
            rejected(controller, command, ["context": initialContext, "item": oldFirst], "stale_context")
        }
        rejected(controller, "prepare_removal", ["context": initialContext, "mode": "permanent"], "stale_context")
        success(controller, "scan", ["path": a.path])
        try await wait("return to first map") { model.root?.id == a.path && !model.isOpeningFolder && !model.isScanning }
        print("PASS: opening and switched maps reject stale context")

        try fm.moveItem(at: first, to: directory.appendingPathComponent("old-first"))
        try Data([3, 3, 3]).write(to: first)
        model.scan(a, force: true)
        try await wait("replacement identity") {
            model.root?.id == a.path && !model.isOpeningFolder && !model.isScanning && !model.isRefreshing &&
            model.current?.children.first { $0.name == "first" }?.fileIdentity != oldFirst["identity"] as? String
        }
        let currentContext = context(controller)
        rejected(controller, "collect", ["context": currentContext, "items": [oldFirst]], "stale_item")
        let replacement = item(model.current!.children.first { $0.name == "first" }!)
        success(controller, "collect", ["context": currentContext, "items": [replacement, secondItem]])
        try await wait("collection validation") { ready(model) && model.displayedCollection.count == 2 }
        success(controller, "search", ["context": currentContext, "query": "second"])
        try await wait("search alongside collection") { !model.isSearching && !model.isSearchResultOutdated }
        precondition(success(controller, "list")["searchMatchCount"] as? Int == 1)
        let collected = success(controller, "collection")
        precondition(collected["total"] as? Int == 2 && collected["searchMatchCount"] == nil && collected["searchTruncated"] == nil)
        success(controller, "search", ["context": currentContext, "query": ""])
        print("PASS: collection pages do not report unrelated search counts")
        let stalePlan = success(controller, "prepare_removal", ["context": currentContext, "mode": "permanent"])
        precondition(stalePlan["count"] as? Int == 2 && stalePlan["requiresUserAuthorization"] as? Bool == true)
        success(controller, "uncollect", ["context": currentContext, "items": [replacement]])
        try await wait("collection edit") { ready(model) && model.displayedCollection.count == 1 }
        rejected(controller, "remove", ["token": stalePlan["token"]!, "confirmed": true], "stale_plan")
        precondition(fm.fileExists(atPath: first.path) && fm.fileExists(atPath: second.path))
        let switchedPlan = success(controller, "prepare_removal", ["context": currentContext, "mode": "permanent"])
        success(controller, "scan", ["path": b.path])
        try await wait("leave prepared map") { model.root?.id == b.path && !model.isOpeningFolder && !model.isScanning }
        rejected(controller, "remove", ["token": switchedPlan["token"]!, "confirmed": true], "stale_plan")
        success(controller, "scan", ["path": a.path])
        try await wait("restore collection") { model.root?.id == a.path && !model.isOpeningFolder && !model.isScanning && ready(model) }
        print("PASS: replaced identities and changed collection/map reject stale removal plans")

        let finalPlan = success(controller, "prepare_removal", ["context": context(controller), "mode": "permanent"])
        let removal = Gate()
        model.testBeforeRemoval = { removal.pause() }
        let accepted = success(controller, "remove", ["token": finalPlan["token"]!, "confirmed": true])
        precondition(accepted["accepted"] as? Bool == true && accepted["state"] as? String == "running")
        rejected(controller, "remove", ["token": finalPlan["token"]!, "confirmed": true], "invalid_plan")
        try await wait("removal started") { removal.waiting }
        precondition(fm.fileExists(atPath: second.path))
        removal.release()
        try await wait("temporary file removal") { !model.hasActiveRemovals && !fm.fileExists(atPath: second.path) }
        precondition(fm.fileExists(atPath: first.path) && fm.fileExists(atPath: outside.path))
        let completed = success(controller, "status")["lastRemoval"] as! [String: Any]
        precondition(completed["operationID"] as? String == finalPlan["token"] as? String && completed["state"] as? String == "finished")
        print("PASS: authorized plan removes only its temporary item and cannot be replayed")

        try await wait("post-removal ready") { !model.isScanning && !model.isRefreshing && !model.isOpeningFolder && ready(model) }
        let beforeSwap = item(model.current!.children.first { $0.name == "first" }!)
        success(controller, "collect", ["context": context(controller), "items": [beforeSwap]])
        try await wait("replacement collection") { ready(model) && model.displayedCollection.count == 1 }
        let swapPlan = success(controller, "prepare_removal", ["context": context(controller), "mode": "permanent"])
        let reviewedFirst = directory.appendingPathComponent("reviewed-first")
        try fm.moveItem(at: first, to: reviewedFirst)
        let newContents = Data([7, 7, 7, 7])
        try newContents.write(to: first)
        let reviewedInode = try fm.attributesOfItem(atPath: reviewedFirst.path)[.systemFileNumber] as! NSNumber
        let replacementInode = try fm.attributesOfItem(atPath: first.path)[.systemFileNumber] as! NSNumber
        precondition(reviewedInode != replacementInode)
        success(controller, "remove", ["token": swapPlan["token"]!, "confirmed": true])
        try await wait("changed identity refusal") { !model.hasActiveRemovals }
        let preservedContents = try Data(contentsOf: first)
        precondition(preservedContents == newContents)
        let refused = success(controller, "status")["lastRemoval"] as! [String: Any]
        precondition((refused["error"] as? String)?.contains("changed") == true)
        print("PASS: filesystem replacement after preparation is preserved by native identity validation")

        try await wait("native follow-up ready") { !model.isScanning && !model.isRefreshing && !model.isOpeningFolder && ready(model) }
        let failedOperationID = swapPlan["token"] as! String
        success(controller, "clear_collection", ["context": context(controller)])
        let nativeItem = item(model.current!.children.first { $0.name == "native-later" }!)
        success(controller, "collect", ["context": context(controller), "items": [nativeItem]])
        try await wait("native follow-up collection") { ready(model) && model.displayedCollection.count == 1 }
        let nativeRemoval = Gate()
        model.testBeforeRemoval = { nativeRemoval.pause() }
        model.deleteCollectionPermanently()
        try await wait("native follow-up started") { nativeRemoval.waiting }
        let duringNative = success(controller, "status")["lastRemoval"] as! [String: Any]
        precondition(duringNative["operationID"] as? String == failedOperationID && duringNative["state"] as? String == "finished")
        precondition(duringNative["error"] as? String == refused["error"] as? String)
        nativeRemoval.release()
        try await wait("native follow-up finished") { !model.hasActiveRemovals }
        let afterNative = success(controller, "status")["lastRemoval"] as! [String: Any]
        precondition(afterNative["operationID"] as? String == failedOperationID && afterNative["error"] as? String == refused["error"] as? String)
        print("PASS: later native removal cannot overwrite an automated operation receipt")

        let partial = directory.appendingPathComponent("partial"), partialFile = partial.appendingPathComponent("visible")
        try fm.createDirectory(at: partial, withIntermediateDirectories: true)
        try Data([8]).write(to: partialFile)
        let scan = Gate()
        let live = DiskModel(cacheDirectory: directory.appendingPathComponent("live-index"), preferences: preferences,
                             scanProgressObserver: { if $0.currentPath == partial.path && $0.itemCount > 0 { scan.pause() } })
        let liveController = AutomationController(model: live)
        diagnostics = { String(describing: success(liveController, "status")) }
        defer { scan.release(); liveController.stop(); live.cancelScan() }
        success(liveController, "scan", ["path": partial.path])
        try await wait("partial visible file") { scan.waiting && live.current?.children.count == 1 && !live.isOpeningFolder }
        precondition(live.isPartialMap && live.isScanning)
        let livePage = success(liveController, "list")
        precondition(livePage["mappingIncomplete"] as? Bool == true)
        let liveNode = item(live.current!.children[0])
        success(liveController, "collect", ["context": context(liveController), "items": [liveNode]])
        try await wait("partial collection") { ready(live) && live.displayedCollection.count == 1 }
        let partialPlan = success(liveController, "prepare_removal", ["context": context(liveController), "mode": "permanent"])
        precondition(partialPlan["sizesComplete"] as? Bool == false)
        success(liveController, "remove", ["token": partialPlan["token"]!, "confirmed": true])
        try await wait("delete while discovery is held") { !live.hasActiveRemovals && !fm.fileExists(atPath: partialFile.path) }
        precondition(scan.waiting && live.isScanning)
        scan.release()
        try await wait("partial map finishes") { !live.isScanning && !live.isOpeningFolder }
        precondition(fm.fileExists(atPath: outside.path))
        print("PASS: partial-map items can be collected and deleted without waiting for discovery")

        let failedRoot = directory.appendingPathComponent("failed-search"), failedCache = directory.appendingPathComponent("failed-search-index")
        try fm.createDirectory(at: failedRoot, withIntermediateDirectories: true)
        try Data([1]).write(to: failedRoot.appendingPathComponent("searchable-file"))
        let failedModel = DiskModel(cacheDirectory: failedCache, preferences: preferences)
        failedModel.testMonitorUnavailable = true
        let failedController = AutomationController(model: failedModel)
        diagnostics = { String(describing: success(failedController, "status")) }
        defer { failedController.stop(); failedModel.cancelScan() }
        success(failedController, "scan", ["path": failedRoot.path])
        try await wait("failed-search fixture ready") { failedModel.root?.id == failedRoot.path && !failedModel.isOpeningFolder && !failedModel.isScanning }
        let failedContext = context(failedController)
        success(failedController, "search", ["context": failedContext, "query": "searchable"])
        try await wait("initial search result") { !failedModel.isSearching && !failedModel.isSearchResultOutdated && failedModel.searchMatchCount == 1 }
        let saved = DiskIndexStore(directory: failedCache).savedSnapshot(for: failedRoot)!
        let unavailable = directory.appendingPathComponent("temporarily-unavailable-index")
        try fm.moveItem(at: saved.databaseURL, to: unavailable)
        failedModel.testRetrySearch()
        try await wait("search failure") { failedModel.searchFailed && !failedModel.isSearching }
        precondition(success(failedController, "status")["searchFailed"] as? Bool == true)
        rejected(failedController, "list", [:], "search_failed")
        precondition(success(failedController, "collection")["total"] as? Int == 0)
        try fm.moveItem(at: unavailable, to: saved.databaseURL)
        success(failedController, "search", ["context": failedContext, "query": "searchable"])
        precondition(failedModel.isSearching)
        try await wait("same-query retry recovers") { !failedModel.isSearching && !failedModel.searchFailed && !failedModel.isSearchResultOutdated }
        precondition(success(failedController, "status")["searchFailed"] as? Bool == false)
        precondition(success(failedController, "list")["total"] as? Int == 1)
        print("PASS: failed search reports an error and the same query can retry successfully")
        print("PASS: automation controller checks")
    }
}
