import AppKit
import OrbitCore
import OrbitAutomation

@MainActor
final class AutomationController: ObservableObject {
    private let model: DiskModel
    private var server: LocalAutomationServer?
    private var lifecycle = UUID()
    private struct Page {
        let id = UUID().uuidString
        let kind: String
        let context: String
        let folder: String
        let query: String
        let created = Date()
        let nodes: [DiskNode]
    }
    private var page: Page?
    private var expiry: Task<Void, Never>?
    private struct Plan {
        let id = UUID().uuidString
        let review: CollectionReviewToken
        let session: UUID
        let mode: String
        let expires = Date().addingTimeInterval(300)
    }
    private var plan: Plan?
    private var lastRemoval: (plan: Plan, receiptID: UUID)?

    init(model: DiskModel) { self.model = model }

    func start() {
        guard server == nil else { return }
        let generation = lifecycle
        let candidate = LocalAutomationServer { [weak self] request in
            guard let self else {
                return AutomationCatalog.encode(AutomationCatalog.failure(AutomationCatalog.Failure("unavailable", "Orbit Disk is closing.")))
            }
            let response = await MainActor.run { () -> [String: Any] in
                guard !Task.isCancelled, self.lifecycle == generation else {
                    return AutomationCatalog.failure(AutomationCatalog.Failure("unavailable", "Orbit Disk is closing."))
                }
                return self.handle(request)
            }
            return AutomationCatalog.encode(response)
        }
        do { try candidate.start(); server = candidate }
        catch { model.errorMessage = "Couldn’t start the automation interface: \(error.localizedDescription)" }
    }
    func stop() { lifecycle = UUID(); server?.stop(); server = nil; expiry?.cancel(); page = nil; plan = nil }

    func handle(_ data: Data) -> [String: Any] {
        do {
            guard let request = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  Set(request.keys).isSubset(of: ["command", "arguments"]),
                  let command = request["command"] as? String,
                  let arguments = request["arguments"] as? [String: Any] else { throw failure("invalid_request", "Expected command and arguments.") }
            try AutomationCatalog.validate(command: command, arguments: arguments)
            return ["ok": true, "result": try execute(command, arguments)]
        } catch { return AutomationCatalog.failure(error) }
    }

    private func execute(_ command: String, _ args: [String: Any]) throws -> [String: Any] {
        if let page, Date().timeIntervalSince(page.created) > 30 { self.page = nil }
        switch command {
        case "status": return status()
        case "scan":
            let path = args["path"] as! String
            guard path.hasPrefix("/") else { throw failure("invalid_path", "Use an absolute folder path.") }
            model.scan(URL(fileURLWithPath: path, isDirectory: true))
            return accepted()
        case "list", "collection": return try listing(command, args)
        case "remove": return try remove(args)
        default: try requireContext(args)
        }
        switch command {
        case "search":
            let query = args["query"] as! String
            if query == model.query { model.retryFailedSearch() }
            else { model.query = query }
        case "back":
            guard model.navigation.count > 1 else { throw failure("at_root", "Already at the map root.") }
            model.goBack()
        case "refresh":
            guard !model.isScanning, !model.isRefreshing, !model.isRemoving else { throw failure("busy", "The map is already scanning or removing files.") }
            model.rescan()
        case "stop": model.cancelScan()
        case "select", "open", "reveal", "preview":
            let node = try resolve(args["item"] as! [String: Any])
            if command == "open" {
                guard node.isDirectory, !node.isSymbolicLink else { throw failure("not_directory", "Choose a regular folder from list.") }
                model.open(node)
            } else {
                if command == "preview", node.isDirectory { throw failure("not_file", "Quick Look requires a file.") }
                model.selectedID = node.id
                guard model.selected?.fileIdentity == node.fileIdentity,
                      model.selected?.id.utf8.elementsEqual(node.id.utf8) == true else { throw failure("stale_item", "The selection changed. List again.") }
                if command == "reveal" { model.revealSelection() }
                if command == "preview" { model.previewSelection() }
            }
        case "collect":
            guard !model.isRemoving else { throw failure("busy", "Files are being removed.") }
            let nodes = try (args["items"] as! [[String: Any]]).map { try resolve($0) }
            guard nodes.allSatisfy(model.canCollect), model.collect(nodes) else { throw failure("not_collectable", "These items cannot be collected. Check collection and list again.") }
        case "uncollect":
            try requireCollectionReady()
            let nodes = try (args["items"] as! [[String: Any]]).map { try resolve($0, collection: true) }
            for node in nodes { model.removeFromCollection(node.id) }
        case "clear_collection":
            try requireCollectionReady()
            if !model.displayedCollection.isEmpty {
                let review = try reviewToken()
                model.clearCollection(review: review)
            }
        case "prepare_removal":
            try requireCollectionReady()
            guard model.displayedCollection.count <= 200 else { throw failure("too_many_items", "Automated removal supports up to 200 items per review. Reduce the collection or use the app's review.") }
            let review = try reviewToken()
            guard let session = model.automationSessionID else { throw failure("stale_context", "The active map changed.") }
            let candidate = Plan(review: review, session: session, mode: args["mode"] as! String)
            plan = candidate
            return ["token": candidate.id, "mode": candidate.mode, "expiresAt": date(candidate.expires),
                    "items": model.displayedCollection.map(nodeJSON), "count": model.displayedCollection.count,
                    "logicalBytes": model.displayedCollectionSize,
                    "sizesComplete": !model.isPartialMap && !model.isUpdatingRemovalSizes,
                    "requiresUserAuthorization": true]
        default: throw failure("unknown_command", "Unknown command.")
        }
        return accepted()
    }

    private func remove(_ args: [String: Any]) throws -> [String: Any] {
        guard let plan, plan.id == args["token"] as? String, plan.expires > Date(),
              args["confirmed"] as? Bool == true else { throw failure("invalid_plan", "Prepare a new removal plan and authorize its exact items and mode.") }
        guard model.automationSessionID == plan.session, model.collectionReviewMatches(plan.review) else {
            self.plan = nil
            throw failure("stale_plan", "The map or collection changed. Prepare a new removal plan.")
        }
        try requireCollectionReady()
        guard !model.hasActiveRemovals, let confirmation = model.beginCollectionConfirmation(review: plan.review) else { throw failure("busy", "A removal or confirmation is already in progress.") }
        defer { model.endCollectionConfirmation(confirmation) }
        self.plan = nil
        if plan.mode == "permanent" { model.deleteCollectionPermanently(review: plan.review) }
        else { model.moveCollectionToTrash(review: plan.review) }
        guard let receipt = model.latestAutomationRemovalReceipt(sessionID: plan.session), receipt.running else {
            throw failure("not_started", "The removal was not started. Review the collection again.")
        }
        lastRemoval = (plan, receipt.id)
        return ["accepted": true, "operationID": plan.id, "mode": plan.mode, "state": "running"]
    }

    private func listing(_ kind: String, _ args: [String: Any]) throws -> [String: Any] {
        if kind == "list", model.isFiltering, model.searchFailed, !model.isSearching {
            throw failure("search_failed", "Search could not finish. Retry search or refresh the map.")
        }
        let limit = (args["limit"] as? Int) ?? 100
        let offset: Int
        let snapshot: Page
        if let cursor = args["cursor"] as? String {
            let parts = cursor.split(separator: ":", omittingEmptySubsequences: false)
            guard parts.count == 2, let index = Int(parts[1]), index >= 0,
                  let existing = page, existing.id == String(parts[0]), existing.kind == kind,
                  existing.context == context, existing.folder.utf8.elementsEqual((model.current?.id ?? "").utf8),
                  existing.query == model.query, index <= existing.nodes.count else { throw failure("stale_cursor", "The page expired or the view changed. List again without a cursor.") }
            offset = index; snapshot = existing
        } else {
            guard let context else { return ["pending": model.isOpeningFolder, "items": [], "context": NSNull()] }
            if kind == "list", model.isSortingRows || (model.isFiltering && (model.isSearching || model.isSearchResultOutdated)) {
                return ["pending": true, "items": [], "context": context, "query": model.query, "searchFailed": model.searchFailed]
            }
            snapshot = Page(kind: kind, context: context, folder: model.current?.id ?? "", query: model.query,
                            nodes: kind == "collection" ? model.displayedCollection : model.displayedNodes)
            page = snapshot; offset = 0
            expiry?.cancel()
            let id = snapshot.id
            expiry = Task { [weak self] in
                do { try await Task.sleep(nanoseconds: 30_000_000_000) } catch { return }
                if self?.page?.id == id { self?.page = nil }
            }
        }
        let end = offset + min(limit, snapshot.nodes.count - offset)
        var result: [String: Any] = ["items": snapshot.nodes[offset..<end].map(nodeJSON), "context": snapshot.context,
                "folder": snapshot.folder, "query": snapshot.query, "snapshotAt": date(snapshot.created),
                "total": snapshot.nodes.count, "offset": offset,
                "nextCursor": end < snapshot.nodes.count ? snapshot.id + ":" + String(end) : NSNull(),
                "pending": kind == "collection" && (model.isValidatingCollection || model.isUpdatingCollection),
                "mappingIncomplete": model.isPartialMap || model.isScanning]
        if kind == "list" {
            result["searchMatchCount"] = model.isFiltering ? model.searchMatchCount : snapshot.nodes.count
            result["searchTruncated"] = model.isFiltering && model.searchMatchCount > snapshot.nodes.count
        }
        return result
    }

    private var context: String? { model.automationSessionID?.uuidString }
    private func requireContext(_ args: [String: Any]) throws {
        guard let context, args["context"] as? String == context else { throw failure("stale_context", "The active map changed or is opening. Read status/list again.") }
    }
    private func requireCollectionReady() throws {
        guard !model.isRemoving, !model.isValidatingCollection, !model.isUpdatingCollection else { throw failure("busy", "The collection is still changing. Poll collection before continuing.") }
    }
    private func reviewToken() throws -> CollectionReviewToken {
        guard let review = model.collectionReviewToken ?? model.beginCollectionReview() else { throw failure("empty_collection", "The collection is empty or unavailable.") }
        return review
    }
    private func resolve(_ item: [String: Any], collection: Bool = false) throws -> DiskNode {
        let path = item["path"] as! String, identity = item["identity"] as! String
        let node = collection ? model.displayedCollection.first { $0.id.utf8.elementsEqual(path.utf8) } : model.node(withID: path)
        guard let node, node.id.utf8.elementsEqual(path.utf8), node.fileIdentity == identity else { throw failure("stale_item", "The item is no longer in this view or its identity changed. List again.") }
        return node
    }
    private func nodeJSON(_ node: DiskNode) -> [String: Any] {
        ["path": node.id, "identity": node.fileIdentity as Any? ?? NSNull(), "name": node.name,
         "directory": node.isDirectory, "symbolicLink": node.isSymbolicLink, "accessible": node.isAccessible,
         "logicalBytes": node.size, "allocatedBytes": node.allocatedSize, "fileCount": node.fileCount,
         "scanState": String(describing: node.scanState), "sizesComplete": node.isScanComplete && !model.isUpdatingRemovalSize(of: node),
         "canCollect": model.canCollect(node)]
    }
    private func status() -> [String: Any] {
        var value: [String: Any] = ["context": context as Any? ?? NSNull(), "root": model.root.map(nodeJSON) as Any? ?? NSNull(),
            "folder": model.current.map(nodeJSON) as Any? ?? NSNull(), "opening": model.isOpeningFolder,
            "scanning": model.isScanning, "refreshing": model.isRefreshing, "mappingIncomplete": model.isPartialMap,
            "discoveredFiles": model.progressCount, "discoveredBytes": model.progressBytes,
            "query": model.query, "searching": model.isSearching, "searchOutdated": model.isSearchResultOutdated,
            "searchFailed": model.searchFailed,
            "collectionCount": model.displayedCollection.count, "collectionBytes": model.displayedCollectionSize,
            "collectionPending": model.isValidatingCollection || model.isUpdatingCollection,
            "removing": model.hasActiveRemovals, "updatingSizes": model.isUpdatingRemovalSizes,
            "message": model.statusMessage as Any? ?? NSNull(), "error": model.errorMessage as Any? ?? NSNull()]
        if let lastRemoval {
            if let state = model.automationRemovalReceipt(lastRemoval.receiptID) {
                value["lastRemoval"] = ["operationID": lastRemoval.plan.id, "mode": lastRemoval.plan.mode,
                    "state": state.running ? "running" : "finished", "removedCount": state.removedCount as Any? ?? NSNull(),
                    "message": state.status, "error": state.error as Any? ?? NSNull()]
            } else { value["lastRemoval"] = ["operationID": lastRemoval.plan.id, "state": "unavailable"] }
        }
        return value
    }
    private func accepted() -> [String: Any] { ["accepted": true, "status": status()] }
    private func date(_ value: Date) -> String { ISO8601DateFormatter().string(from: value) }
    private func failure(_ code: String, _ message: String) -> AutomationCatalog.Failure { .init(code, message) }
}
