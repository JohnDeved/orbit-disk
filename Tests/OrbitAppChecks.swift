import AppKit
import Foundation
import OrbitCore

private final class PreviewGate: @unchecked Sendable {
    private let lock = NSLock()
    private let semaphore = DispatchSemaphore(value: 0)
    private var callbacks = 0
    private var pauseAt = 2
    private var waiting = false
    private var released = false

    var isWaiting: Bool { lock.lock(); defer { lock.unlock() }; return waiting }

    func observe(_ progress: ScanProgress) {
        lock.lock()
        callbacks += 1
        let shouldWait = callbacks == pauseAt && !released
        if shouldWait { waiting = true }
        lock.unlock()
        if shouldWait {
            semaphore.wait()
            lock.lock(); waiting = false; lock.unlock()
        }
    }

    func arm(callback: Int) {
        lock.lock()
        callbacks = 0; pauseAt = callback; released = false
        lock.unlock()
    }

    func release() {
        lock.lock()
        released = true
        lock.unlock()
        semaphore.signal()
    }
}

@main struct ModelCheck {
    static func checkCollectionCoverage() {
        let empty = CollectionCoverage()
        precondition(empty.state(for: "/") == .none && empty.state(for: [String]()) == .none)
        let folder = "/Users/Zoë/プロジェクト"
        let coverage = CollectionCoverage(paths: [folder + "/", "/Elsewhere/one.bin"])
        precondition(coverage.state(for: folder) == .included)
        precondition(coverage.state(for: folder + "/nested/file.bin") == .included)
        precondition(coverage.state(for: "/Users/Zoë") == .containsItems && coverage.state(for: "/") == .containsItems)
        precondition(coverage.state(for: folder + "-old/file.bin") == .none, "Collection coverage must respect path component boundaries")
        precondition(coverage.state(for: [folder, "/Elsewhere/one.bin"]) == .included)
        precondition(coverage.state(for: [folder, "/Elsewhere/two.bin"]) == .containsItems)
        precondition(coverage.state(for: ["/Users", "/Unrelated"]) == .containsItems)
        precondition(coverage.state(for: ["/Unrelated", "/Another"]) == .none)
        let root = CollectionCoverage(paths: ["/"])
        precondition(root.state(for: "/") == .included && root.state(for: "/Users/any/file") == .included)
        let composed = "/Unicode/\u{00E9}"
        let decomposed = "/Unicode/e\u{0301}"
        let exactPaths = CollectionCoverage(paths: [composed])
        precondition(exactPaths.state(for: composed + "/child") == .included)
        precondition(exactPaths.state(for: decomposed + "/child") == .none,
                     "Distinct Unicode path encodings must not be merged by string comparison")
        print("PASS: collection coverage boundaries, Unicode paths, inherited folders, ancestors, mixed groups, and root handling")
    }

    @MainActor static func wait(_ name: String, timeout: Double = 15, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            guard Date() < deadline else { throw NSError(domain: "ModelCheck", code: 1, userInfo: [NSLocalizedDescriptionKey: "Timeout: \(name)"]) }
            try await Task.sleep(nanoseconds: 30_000_000)
        }
    }

    @MainActor static func checkGroupedHoverCaching(base: URL, preferences: UserDefaults) {
        let rootURL = base.appendingPathComponent("hover-fixture")
        func leaf(_ name: String, identity: String = "1:1", size: Int64 = 1) -> DiskNode {
            DiskNode(url: rootURL.appendingPathComponent(name), name: name, isDirectory: false,
                     size: size, allocatedSize: size, fileCount: 1, fileIdentity: identity)
        }
        let zeta = leaf("zeta.bin")
        let alpha = leaf("alpha.bin", identity: "1:2")
        let members = [zeta, alpha]
        let groupID = "group:\(rootURL.path)"
        let cache = GroupedMapHoverCache()
        var computations = 0
        func presentation(_ nodes: [DiskNode]) -> MapHoverPresentation {
            cache.presentation(groupID: groupID, members: nodes) {
                computations += 1
                return MapHoverPresentation(relatedRowIDs: Set(nodes.map(\.id)),
                                            description: nodes.compactMap(\.fileIdentity).joined(separator: ","))
            }
        }
        precondition(presentation(members).relatedRowIDs == [zeta.id, alpha.id])
        for _ in 0..<100 { _ = presentation(members) }
        precondition(computations == 1, "Unchanged grouped members must reuse their presentation")
        var changed = members
        changed[0] = leaf("replacement.bin")
        precondition(presentation(changed).relatedRowIDs == [changed[0].id, alpha.id] && computations == 2,
                     "Equal-count membership changes must invalidate a same-ID group")
        changed[0] = leaf("replacement.bin", identity: "1:99")
        precondition(presentation(changed).description == "1:99,1:2" && computations == 3,
                     "Changed metadata must not reuse the previous member buffer")
        cache.removeAll()
        _ = presentation(changed)
        precondition(computations == 4)

        func folder(_ name: String, children: [DiskNode]) -> DiskNode {
            DiskNode(url: rootURL, name: name, isDirectory: true, size: 2, allocatedSize: 2,
                     fileCount: 2, children: children)
        }
        let root = folder("Fixture", children: members)
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("hover-cache"), preferences: preferences)
        model.root = root
        model.navigation = [root]
        let displayedItems = model.displayedItems
        precondition(displayedItems.map(\.id) == [zeta.id, alpha.id])
        model.hoveredListID = alpha.id
        model.selectedID = zeta.id
        precondition(displayedItems.withUnsafeBufferPointer { previous in
            model.displayedItems.withUnsafeBufferPointer { previous.baseAddress == $0.baseAddress }
        }, "Hover and selection must reuse the current list's cached identities")
        model.setMapHover(nodes: members, groupID: groupID)
        precondition(model.mapRelatedRowIDs == [zeta.id, alpha.id])
        model.setMapHover(nodes: nil, groupID: nil)
        precondition(model.mapRelatedRowIDs.isEmpty && model.mapHoverDescription == nil)
        model.setMapHover(nodes: members, groupID: groupID)
        precondition(model.mapRelatedRowIDs == [zeta.id, alpha.id])
        model.setMapHover(nodes: changed, groupID: groupID)
        precondition(model.mapRelatedRowIDs == [alpha.id], "Changed group membership must reach the model")
        model.setMapHover(nodes: members, groupID: groupID)
        model.navigation = [folder("Renamed fixture", children: [alpha])]
        precondition(model.displayedItems.map(\.id) == [alpha.id], "Navigation must replace cached list items")
        precondition(model.mapRelatedRowIDs == [alpha.id] && model.mapHoverDescription == "2 items in Renamed fixture",
                     "Same-path navigation metadata and children must invalidate cached group relations")
        model.navigation = [root]
        model.query = "bin"
        model.searchResults = [zeta]
        precondition(model.displayedItems.map(\.id) == [zeta.id])
        model.setMapHover(nodes: members, groupID: groupID)
        precondition(model.mapRelatedRowIDs == [zeta.id])
        model.searchResults = [alpha]
        precondition(model.displayedItems.map(\.id) == [alpha.id], "Search results must replace cached list items")
        precondition(model.mapRelatedRowIDs == [alpha.id], "Equal-count search changes must invalidate group relations")
        model.query = ""
        precondition(model.displayedItems.map(\.id) == [zeta.id, alpha.id], "Leaving search must restore folder items")
        model.setMapHover(nodes: members, groupID: groupID)
        precondition(model.mapRelatedRowIDs == [zeta.id, alpha.id], "Leaving search must restore folder relations")
        precondition(model.displayedNodes.first { model.mapRelatedRowIDs.contains($0.id) }?.id == zeta.id)
        model.sortByName = true
        precondition(model.displayedItems.map(\.id) == [alpha.id, zeta.id], "Sorting must reorder cached list items")
        precondition(model.displayedNodes.first { model.mapRelatedRowIDs.contains($0.id) }?.id == alpha.id,
                     "Cached group relations must not pin the scroll target to the previous sort order")
        model.sortByName = false
        precondition(model.displayedItems.map(\.id) == [zeta.id, alpha.id])
        model.navigation = [folder("Fixture", children: [leaf("zeta.bin", identity: "1:99", size: 99), alpha])]
        precondition(model.displayedItems[0].id == zeta.id && model.displayedItems[0].node.size == 99
                     && model.displayedItems[0].node.fileIdentity == "1:99",
                     "A same-ID folder refresh must replace cached row metadata")
        model.query = "bin"
        model.searchResults = [alpha]
        precondition(model.displayedItems[0].node.size == 1)
        model.searchResults = [leaf("alpha.bin", identity: "1:88", size: 88)]
        precondition(model.displayedItems[0].id == alpha.id && model.displayedItems[0].node.size == 88,
                     "A same-ID search refresh must replace cached row metadata")
        print("PASS: grouped hover reuse, equal-count membership and metadata changes, hover exit, navigation, filtering, and sorting")
        print("PASS: list identity reuse, navigation, search, sort changes, and same-ID metadata refreshes")
    }

    @MainActor static func checkPreparedGroupHover(base: URL, preferences: UserDefaults) {
        for rootPath in [base.appendingPathComponent("prepared-hover").path, "/"] {
            let prefix = rootPath == "/" ? "/" : rootPath + "/"
            func node(_ path: String, children: [DiskNode] = [], size: Int64 = 1) -> DiskNode {
                let url = URL(fileURLWithPath: path)
                return DiskNode(url: url, name: url.lastPathComponent, isDirectory: !children.isEmpty,
                                size: size, allocatedSize: size, fileCount: 1, children: children)
            }
            let descendant = node(prefix + "branch/deep/alpha/inside.bin")
            let alpha = node(prefix + "branch/deep/alpha", children: [descendant])
            let beta = node(prefix + "branch/deep/beta.bin")
            let sibling = node(prefix + "unrelated.bin")
            let deep = node(prefix + "branch/deep", children: [alpha, beta])
            let branch = node(prefix + "branch", children: [deep])
            let root = node(rootPath, children: [branch, sibling])
            let model = DiskModel(cacheDirectory: base.appendingPathComponent("prepared-hover-cache"), preferences: preferences)
            model.root = root
            model.navigation = [root]
            func prepared(_ members: [DiskNode], parent: DiskNode, revision: UUID? = nil) -> PreparedMapHoverGroup {
                PreparedMapHoverGroup(id: "group:\(parent.id)", parentID: parent.id, rootID: root.id,
                                      mapRevision: revision ?? model.mapRevision, memberIDs: Set(members.map(\.id)))
            }
            func compare(_ members: [DiskNode], parent: DiskNode) {
                let group = prepared(members, parent: parent)
                model.setMapHover(nodes: members, groupID: group.id)
                let expectedIDs = model.mapRelatedRowIDs
                let expectedDescription = model.mapHoverDescription
                model.setMapHover(nodes: members, groupID: group.id, preparedGroup: group)
                precondition(model.mapRelatedRowIDs == expectedIDs && model.mapHoverDescription == expectedDescription,
                             "Prepared group relationships must match the member-walk behavior")
            }
            compare([branch, sibling], parent: root)
            precondition(model.mapRelatedRowIDs == [branch.id, sibling.id])
            compare([alpha, beta], parent: deep)
            precondition(model.mapRelatedRowIDs == [branch.id])
            compare([beta], parent: deep)
            precondition(model.mapHoverDescription == "branch/deep/beta.bin")
            model.query = "bin"
            model.searchResults = [root, branch, deep, alpha, beta, descendant, sibling]
            compare([alpha, beta], parent: deep)
            precondition(model.mapRelatedRowIDs == [root.id, branch.id, deep.id, alpha.id, beta.id],
                         "Search must match group members and their ancestors, excluding descendants and unrelated siblings")
            model.searchResults = [beta, descendant, sibling]
            precondition(model.mapRelatedRowIDs == [beta.id], "Prepared hover must use fresh search results")
            let changedAlpha = node(alpha.id, children: [descendant], size: 99)
            compare([changedAlpha, beta], parent: deep)
            precondition(model.hoveredMapNodes[0].size == 99, "Same-ID metadata changes must not reuse old members")
            let stale = prepared([sibling], parent: root, revision: UUID())
            model.setMapHover(nodes: [sibling], groupID: stale.id, preparedGroup: stale)
            precondition(model.mapRelatedRowIDs == [beta.id] && model.hoveredMapNodes[0].size == 99,
                         "A stale graph revision must not replace the current hover")
            let wrongRoot = PreparedMapHoverGroup(id: "wrong-root", parentID: root.id, rootID: prefix + "other",
                                                 mapRevision: model.mapRevision, memberIDs: [sibling.id])
            model.setMapHover(nodes: [sibling], groupID: wrongRoot.id, preparedGroup: wrongRoot)
            precondition(model.mapRelatedRowIDs == [beta.id], "A different graph root must not replace the current hover")
            if rootPath != "/" {
                let outside = PreparedMapHoverGroup(id: "outside", parentID: rootPath + "-sibling", rootID: root.id,
                                                   mapRevision: model.mapRevision, memberIDs: [sibling.id])
                model.setMapHover(nodes: [sibling], groupID: outside.id, preparedGroup: outside)
                precondition(model.mapRelatedRowIDs == [beta.id], "Parent validation must respect path boundaries")
            }
            model.hoveredMapGroupID = nil
            model.hoveredMapNodes = [descendant]
            precondition(model.mapRelatedRowIDs == [descendant.id], "Direct hover writes must discard prepared membership")
            model.setMapHover(nodes: nil, groupID: nil)
            precondition(model.mapRelatedRowIDs.isEmpty && model.mapHoverDescription == nil)
        }
        print("PASS: prepared group relationships, root and deep groups, one-member labels, search ancestry, metadata refresh, stale revisions, and slash root")
    }

    @MainActor static func checkProgressiveMapping(base: URL, preferences: UserDefaults) async throws {
        let fm = FileManager.default
        let root = base.appendingPathComponent("progressive-fixture")
        let folder = root.appendingPathComponent("Pending")
        let leaf = root.appendingPathComponent("ready.bin")
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        try Data([1, 2, 3]).write(to: leaf)
        try Data(repeating: 4, count: 100).write(to: folder.appendingPathComponent("inside.bin"))
        let cache = base.appendingPathComponent("progressive-cache")
        let gate = PreviewGate()
        defer { gate.release() }
        let model = DiskModel(cacheDirectory: cache, preferences: preferences, scanProgressObserver: { gate.observe($0) })
        let started = Date()
        model.scan(root)
        try await wait("first partial listing", timeout: 4) {
            gate.isWaiting && model.current?.children.count == 2 && model.isPartialMap
        }
        let firstPreviewTime = Date().timeIntervalSince(started)
        precondition(model.isScanning && model.indexedAt == nil && model.scanResult == nil)
        precondition(DiskIndexStore(directory: cache).savedSnapshot(for: root) == nil,
                     "The first browsable map must precede scan completion and persistence")
        let pending = model.current!.children.first { $0.id == folder.path }!
        let ready = model.current!.children.first { $0.id == leaf.path }!
        precondition(!pending.isScanComplete && pending.children.isEmpty && ready.isScanComplete)
        precondition(!model.canCollect(pending) && !model.collect([pending]) && model.collection.isEmpty,
                     "Incomplete folders must not enter collection")
        precondition(model.canCollect(ready) && model.collect([ready]))
        model.moveCollectionToTrash()
        model.deleteCollectionPermanently()
        precondition(!model.isRemoving && fm.fileExists(atPath: leaf.path) && model.collection.count == 1,
                     "Both removal routes must stay blocked until the complete map is persisted")
        model.query = "ready"
        precondition(!model.isSearching && model.searchResults.isEmpty, "Partial maps must not search an earlier saved index")
        model.query = ""
        model.open(pending)
        try await wait("browse pending folder") { model.current?.id == folder.path && !model.isOpeningFolder }
        precondition(model.current?.scanState == .pending && model.current?.children.isEmpty == true)
        model.goBack()
        try await wait("browse partial parent") { model.current?.id == root.path && !model.isOpeningFolder }
        model.selectedID = ready.id
        let colors = model.childColorIndices
        let frozenRevision = model.mapRevision
        model.setMapInteractionActive(true)
        gate.release()
        try await wait("index saved during held interaction") { DiskIndexStore(directory: cache).savedSnapshot(for: root) != nil }
        try await Task.sleep(nanoseconds: 100_000_000)
        precondition(model.mapRevision == frozenRevision && model.isPartialMap && model.isScanning,
                     "Progress and the final handoff must not move the map beneath a held mouse gesture")
        model.setMapInteractionActive(false)
        try await wait("partial map final handoff") { !model.isScanning && !model.isOpeningFolder && !model.isPartialMap }
        precondition(model.root?.size == 103 && model.current?.id == root.path && model.selectedID == ready.id)
        precondition(model.collection.map(\.id) == [ready.id] && model.childColorIndices == colors,
                     "Completion must retain collection, selection, and color assignments when sizes reorder rows")
        precondition(model.current!.children.allSatisfy(\.isScanComplete) && model.indexedAt != nil)
        model.cancelScan()
        print(String(format: "PASS: browsable initial preview in %.3fs before scan completion; pending navigation, collection/removal safety, stable gesture geometry, final handoff", firstPreviewTime))

        let cancelGate = PreviewGate()
        defer { cancelGate.release() }
        let paused = DiskModel(cacheDirectory: base.appendingPathComponent("paused-preview-cache"), preferences: preferences,
                               scanProgressObserver: { cancelGate.observe($0) })
        paused.scan(root)
        try await wait("cancel fixture partial") { cancelGate.isWaiting && paused.current?.children.count == 2 }
        paused.open(paused.current!.children.first { $0.id == folder.path }!)
        try await wait("partial route before cancel") { paused.current?.id == folder.path && !paused.isOpeningFolder }
        paused.cancelScan()
        cancelGate.release()
        precondition(paused.isPartialMap && !paused.isScanning && paused.updatesPaused && paused.current?.id == folder.path)
        paused.goBack()
        try await wait("paused partial navigation") { paused.current?.id == root.path && !paused.isOpeningFolder }
        paused.open(paused.current!.children.first { $0.id == folder.path }!)
        try await wait("paused pending folder navigation") { paused.current?.id == folder.path && !paused.isOpeningFolder }
        paused.rescan()
        try await wait("restart partial scan") { !paused.isScanning && !paused.isOpeningFolder && !paused.isPartialMap }
        precondition(paused.current?.id == folder.path && paused.current?.children.first?.name == "inside.bin",
                     "Restart and final persistence must retain the folder visited in a partial map")
        paused.cancelScan()

        let selectionGate = PreviewGate()
        defer { selectionGate.release() }
        let selectionModel = DiskModel(cacheDirectory: base.appendingPathComponent("selection-preview-cache"), preferences: preferences,
                                       scanProgressObserver: { selectionGate.observe($0) })
        selectionModel.scan(root)
        try await wait("selection fixture partial") { selectionGate.isWaiting && selectionModel.current?.children.count == 2 }
        selectionModel.selectedID = leaf.path
        selectionModel.cancelScan()
        selectionGate.release()
        try await wait("cancelled selection worker") { !selectionGate.isWaiting }
        selectionGate.arm(callback: 1)
        selectionModel.rescan()
        try await wait("restart empty preview shell") {
            selectionGate.isWaiting && selectionModel.current?.children.isEmpty == true
        }
        precondition(selectionModel.selectedID == leaf.path && selectionModel.selected == nil,
                     "A restarted scan must retain selection while its new root shell has no children yet")
        selectionGate.release()
        try await wait("selection after restart") { !selectionModel.isScanning && !selectionModel.isPartialMap }
        precondition(selectionModel.selectedID == leaf.path && selectionModel.selected?.id == leaf.path)
        selectionModel.cancelScan()

        let switchedRoot = base.appendingPathComponent("switched-preview-fixture")
        try fm.createDirectory(at: switchedRoot, withIntermediateDirectories: true)
        try Data([8]).write(to: switchedRoot.appendingPathComponent("different.bin"))
        let switchGate = PreviewGate()
        defer { switchGate.release() }
        let switched = DiskModel(cacheDirectory: base.appendingPathComponent("switched-preview-cache"), preferences: preferences,
                                 scanProgressObserver: { switchGate.observe($0) })
        switched.scan(root)
        try await wait("switch fixture partial") { switchGate.isWaiting && switched.current?.children.count == 2 }
        switched.scan(switchedRoot)
        try await wait("new root after partial") { switched.root?.id == switchedRoot.path && !switched.isScanning }
        switchGate.release()
        try await Task.sleep(nanoseconds: 400_000_000)
        precondition(switched.root?.id == switchedRoot.path && switched.current?.children.map(\.name) == ["different.bin"]
                     && !switched.isPartialMap, "Cancelled preview work must never replace a newly chosen root")
        switched.cancelScan()

        let failureCache = base.appendingPathComponent("failed-save-preview-cache")
        let failureGate = PreviewGate()
        defer { failureGate.release() }
        let failed = DiskModel(cacheDirectory: failureCache, preferences: preferences, scanProgressObserver: { failureGate.observe($0) })
        failed.scan(root)
        try await wait("save failure fixture partial") { failureGate.isWaiting && failed.current?.children.count == 2 }
        failed.open(failed.current!.children.first { $0.id == folder.path }!)
        try await wait("save failure route") { failed.current?.id == folder.path && !failed.isOpeningFolder }
        try fm.removeItem(at: failureCache)
        try Data([0]).write(to: failureCache)
        failureGate.release()
        try await wait("index save failure") { !failed.isScanning && failed.errorMessage != nil }
        precondition(failed.isPartialMap && failed.updatesPaused && failed.current?.id == folder.path
                     && failed.current?.children.first?.name == "inside.bin",
                     "A completed in-memory scan must remain browsable when persistence fails")
        precondition(failed.collect([failed.current!.children[0]]))
        failed.deleteCollectionPermanently()
        precondition(!failed.isRemoving && fm.fileExists(atPath: folder.appendingPathComponent("inside.bin").path))
        failed.goBack()
        try await wait("failed-save parent navigation") { failed.current?.id == root.path && !failed.isOpeningFolder }
        failed.cancelScan()
        print("PASS: partial map retained after cancel, pending navigation after stop, restart route retention, stale root-switch suppression, failed-save browsing and removal lock")
    }

    @MainActor static func checkCachedRootValidation(base: URL, preferences: UserDefaults) async throws {
        let fm = FileManager.default
        let root = base.appendingPathComponent("cached-root-fixture")
        let cache = base.appendingPathComponent("cached-root-index")
        try fm.createDirectory(at: root, withIntermediateDirectories: true)
        try Data([1, 2, 3]).write(to: root.appendingPathComponent("indexed.bin"))
        let checkpoint = try DiskChangeMonitor.capture(root: root)
        let store = DiskIndexStore(directory: cache)
        let result = try DiskScanner().scan(root)
        _ = try store.save(result, eventID: checkpoint.eventID, eventIdentity: "expired-journal-identity")
        try Data([4, 5]).write(to: root.appendingPathComponent("new.bin"))

        let cached = DiskModel(cacheDirectory: cache, preferences: preferences)
        cached.scan(root)
        try await wait("cached root before journal validation") { cached.root != nil && !cached.isScanning }
        precondition(cached.isSavedMap && cached.root?.size == 3,
                     "An identity-matching cached root should display before background journal reconciliation")
        let revision = cached.mapRevision
        cached.selectedID = cached.current!.children[0].id
        cached.hoveredMapNodes = [cached.current!.children[0]]
        cached.statusMessage = "The existing map remains usable."
        precondition(cached.mapRevision == revision, "Hover, selection, and status must not invalidate prepared map data")
        try await wait("incompatible journal refresh") {
            !cached.isRefreshing && !cached.isOpeningFolder && !cached.isSavedMap && cached.root?.size == 5
        }
        precondition(cached.mapRevision != revision,
                     "A completed folder publication must invalidate prepared map data")
        precondition(store.savedSnapshot(for: root)?.eventIdentity != "expired-journal-identity")
        cached.cancelScan()

        let originalRoot = base.appendingPathComponent("original-cached-root")
        try fm.moveItem(at: root, to: originalRoot)
        try fm.createDirectory(at: root, withIntermediateDirectories: true)
        try Data([6, 7, 8, 9]).write(to: root.appendingPathComponent("replacement.bin"))
        let replacement = DiskModel(cacheDirectory: cache, preferences: preferences)
        replacement.scan(root)
        try await wait("replaced cached root") { replacement.root != nil && !replacement.isScanning }
        precondition(!replacement.isSavedMap && replacement.root?.size == 4
                     && replacement.current?.children.map(\.name) == ["replacement.bin"],
                     "A replaced root must be scanned instead of displaying the old root's saved map")
        replacement.cancelScan()
        print("PASS: immediate cached-root display, background journal validation, replaced-root rejection, stable map revision until publication")
    }

    @MainActor static func checkCollectionInteraction(base: URL, preferences: UserDefaults) async throws {
        let fm = FileManager.default
        let root = base.appendingPathComponent("collection-fixture")
        let branchURL = root.appendingPathComponent("Branch")
        let nested = branchURL.appendingPathComponent("Sub")
        let hidden = root.appendingPathComponent(".protected")
        let batchURL = root.appendingPathComponent("Batch")
        try fm.createDirectory(at: nested, withIntermediateDirectories: true)
        try fm.createDirectory(at: hidden, withIntermediateDirectories: true)
        try fm.createDirectory(at: batchURL, withIntermediateDirectories: true)
        for index in 0..<72 { try Data([UInt8(index)]).write(to: batchURL.appendingPathComponent("batch-\(index).bin")) }
        try Data([1, 2, 3]).write(to: nested.appendingPathComponent("item.bin"))
        try Data([4, 5]).write(to: root.appendingPathComponent("other.bin"))
        try Data([6]).write(to: root.appendingPathComponent("stable.bin"))
        try Data([7]).write(to: hidden.appendingPathComponent("secret.bin"))
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("collection-cache"), preferences: preferences)
        model.scan(root)
        try await wait("collection map") { model.root != nil && !model.isScanning }
        model.cancelScan()
        let branch = model.node(withID: branchURL.path)!
        let deep = model.node(withID: nested.appendingPathComponent("item.bin").path)!
        let other = model.node(withID: root.appendingPathComponent("other.bin").path)!
        let stable = model.node(withID: root.appendingPathComponent("stable.bin").path)!
        let protected = model.node(withID: hidden.appendingPathComponent("secret.bin").path)!
        let batch = model.node(withID: batchURL.path)!.children
        precondition(batch.count == 72)
        model.selectedID = other.id
        model.hoveredMapNodes = [deep]
        precondition(model.mapRelatedRowIDs == [branch.id], "A deep map segment must highlight its visible ancestor row")
        precondition(model.mapHoverDescription == "Branch/Sub/item.bin")
        precondition(model.selectedID == other.id, "Hover must not change the selection")
        model.hoveredMapNodes = [deep, other]
        precondition(model.mapRelatedRowIDs == [branch.id, other.id], "Grouped segments must relate to every represented visible row")
        model.query = "item"
        precondition(model.hoveredMapNodes.isEmpty && model.hoveredListID == nil, "Filtering must clear prior hover")
        try await wait("collection indexed search") { !model.isSearching && model.searchResults.count == 1 }
        model.hoveredMapNodes = [deep]
        precondition(model.mapRelatedRowIDs == [deep.id], "Search rows must relate to the exact hovered match")

        model.query = "bin"
        try await wait("search result row drag") {
            !model.isSearching && model.searchResults.contains(where: { $0.id == deep.id })
                && model.searchResults.contains(where: { $0.id == other.id })
        }
        let sourceRow = model.searchResults.first { $0.id == deep.id }!
        let completeResults = model.searchResults
        model.hoveredMapNodes = [deep]
        precondition(model.mapRelatedRowIDs == [deep.id])
        model.searchResults = completeResults.filter { $0.id != deep.id }
        precondition(model.mapRelatedRowIDs.isEmpty && !model.displayedNodes.contains(where: { $0.id == deep.id }),
                     "Updated search results must invalidate cached rows and hover relations even when the query stays the same")
        model.searchResults = completeResults
        precondition(model.mapRelatedRowIDs == [deep.id])
        model.sortByName = true
        let alphabeticNames = model.displayedNodes.map(\.name)
        precondition(alphabeticNames == completeResults.map(\.name).sorted { $0.localizedStandardCompare($1) == .orderedAscending })
        model.hoveredListID = other.id
        precondition(model.displayedNodes.map(\.name) == alphabeticNames && model.mapRelatedRowIDs == [deep.id],
                     "List hover must preserve the cached ordering and graph relation")
        model.sortByName = false
        precondition(model.displayedNodes.map(\.id) == completeResults.map(\.id), "Changing sort order must invalidate cached row ordering")
        let sourceContents = try Data(contentsOf: sourceRow.url)
        let selectedContents = try Data(contentsOf: other.url)
        model.selectedID = other.id
        let rowItem = model.beginCollectionDrag([sourceRow], label: sourceRow.name)!
        let rowToken = rowItem.data(forType: DiskModel.collectionDragType)!
        precondition(rowItem.types == [DiskModel.collectionDragType] && model.collection.isEmpty,
                     "A search row drag must stage only a private payload until it is dropped")
        precondition(model.selectedID == other.id && model.draggingItemCount == 1,
                     "The dragged search row must be independent of the existing selection")
        model.endCollectionDrag()
        precondition(model.collectDrag(data: rowToken) && model.collection.map(\.id) == [sourceRow.id],
                     "Dropping a search row must collect its exact source instead of the selected row")
        precondition(model.selectedID == other.id)
        precondition(model.beginCollectionDrag([sourceRow], label: sourceRow.name) == nil
                     && model.collection.map(\.id) == [sourceRow.id],
                     "An already collected search row must not start another drag")
        let sourceAfterDrop = try Data(contentsOf: sourceRow.url)
        let selectedAfterDrop = try Data(contentsOf: other.url)
        precondition(sourceAfterDrop == sourceContents && selectedAfterDrop == selectedContents,
                     "Dragging a search row into the collection must leave both files intact")
        model.collection = []
        model.statusMessage = nil
        model.errorMessage = nil
        model.query = ""

        precondition(model.collect([deep, branch, branch, other, model.root!, protected]))
        precondition(Set(model.collection.map(\.id)) == [branch.id, other.id], "Collection must collapse descendants and reject protected locations")
        precondition(model.statusMessage?.contains("2 items already included") == true)
        precondition(model.statusMessage?.contains("Skipped 2 items") == true)
        precondition(!model.canCollect(deep) && !model.canCollect(protected) && !model.canCollect(model.root!))
        precondition(model.collectionCoverage.state(for: branch.id) == .included
                     && model.collectionCoverage.state(for: deep.id) == .included,
                     "Collected folders must mark their visible descendants as included")
        precondition(model.collectionCoverage.state(for: root.path) == .containsItems)
        precondition(model.collectionCoverage.state(for: [deep.id, other.id]) == .included
                     && model.collectionCoverage.state(for: [deep.id, stable.id]) == .containsItems)
        model.collection.removeAll { $0.id == other.id }
        precondition(model.collectionCoverage.state(for: other.id) == .none
                     && model.collectionCoverage.state(for: branch.id) == .included,
                     "Removing a collected item must update cached coverage immediately")
        model.collection = []
        precondition(model.collectionCoverage.state(for: root.path) == .none
                     && model.collectionCoverage.state(for: deep.id) == .none,
                     "Clearing the collection must clear coverage immediately")
        precondition(model.collect([deep]))
        precondition(model.collectionCoverage.state(for: branch.id) == .containsItems
                     && model.collectionCoverage.state(for: deep.id) == .included,
                     "Collecting a nested file must mark its ancestors as containing collected items")
        model.collection = []
        model.errorMessage = nil

        let item = model.beginCollectionDrag([deep, other], label: "2 items")!
        precondition(item.types == [DiskModel.collectionDragType], "Collection drag must carry only its private token")
        let token = item.data(forType: DiskModel.collectionDragType)!
        precondition(model.draggingItemCount == 2 && model.draggingLabel == "2 items")
        precondition(model.collection.isEmpty, "Starting a drag must never collect")
        model.endCollectionDrag()
        precondition(model.draggingLabel == nil && model.draggingItemCount == 0 && model.collection.isEmpty,
                     "Cancelling or ending a drag must never collect")
        precondition(model.canAcceptCollectionDrop, "Drop data may arrive after the native dragging session ends")
        precondition(!model.collectDrag(data: Data("unknown token".utf8)) && model.collection.isEmpty)
        precondition(model.collectDrag(data: token) && model.collection.count == 2)
        precondition(!model.collectDrag(data: token), "A drop token must be consumed once")
        precondition(fm.fileExists(atPath: deep.id) && fm.fileExists(atPath: other.id), "Collecting must not move files")
        model.collection = []

        let directoryToken = model.beginCollectionDrag([branch], label: branch.name)!.data(forType: DiskModel.collectionDragType)!
        let originalBranch = root.appendingPathComponent("Original Branch")
        try fm.moveItem(at: branchURL, to: originalBranch)
        try fm.createDirectory(at: branchURL, withIntermediateDirectories: true)
        precondition(!model.collectDrag(data: directoryToken) && model.collection.isEmpty,
                     "A replacement directory must not be collected using an indexed directory's identity")
        precondition(model.errorMessage?.contains("changed since indexing") == true)
        let fileToken = model.beginCollectionDrag([other], label: other.name)!.data(forType: DiskModel.collectionDragType)!
        let originalOther = root.appendingPathComponent("original-other.bin")
        try fm.moveItem(at: other.url, to: originalOther)
        try Data([8, 9]).write(to: other.url)
        precondition(!model.collectDrag(data: fileToken) && model.collection.isEmpty,
                     "A replacement file must not be collected using an indexed file's identity")
        precondition(fm.fileExists(atPath: originalOther.path) && fm.fileExists(atPath: other.id)
                     && fm.fileExists(atPath: originalBranch.appendingPathComponent("Sub/item.bin").path),
                     "Rejected drops must leave fixture files untouched")

        let batchToken = model.beginCollectionDrag(batch, label: "72 items")!.data(forType: DiskModel.collectionDragType)!
        precondition(model.collectDrag(data: batchToken))
        precondition(model.isValidatingCollection && model.collection.isEmpty,
                     "Large drops must validate in the background before applying one batch")
        precondition(!model.canCollect(stable) && !model.canAcceptCollectionDrop && !model.collectDrag(data: batchToken),
                     "Busy validation must prevent duplicate collection actions")
        model.statusMessage = "A newer action status."
        try await wait("large grouped drop") { !model.isValidatingCollection }
        precondition(model.collection.count == 72 && Set(model.collection.map(\.id)) == Set(batch.map(\.id)))
        precondition(model.statusMessage == "A newer action status.", "A completed background validation must not overwrite a newer status")
        precondition(batch.allSatisfy { fm.fileExists(atPath: $0.id) }, "Grouped collection must not move files")
        model.collection = []
        let cancelledBatchToken = model.beginCollectionDrag(batch, label: "72 items")!.data(forType: DiskModel.collectionDragType)!
        precondition(model.collectDrag(data: cancelledBatchToken) && model.isValidatingCollection)
        model.cancelScan()
        precondition(!model.isValidatingCollection && model.collection.isEmpty, "Cancelling work must clear background collection validation")
        let newerStatus = model.statusMessage
        try await Task.sleep(nanoseconds: 100_000_000)
        precondition(model.collection.isEmpty && model.statusMessage == newerStatus,
                     "Stale background validation must not apply items or overwrite a newer status")

        let previousToken = model.beginCollectionDrag([stable], label: stable.name)!.data(forType: DiskModel.collectionDragType)!
        _ = model.beginCollectionDrag([stable], label: stable.name)
        precondition(!model.collectDrag(data: previousToken), "Starting a new drag must invalidate the old token")
        let rootChangeToken = model.beginCollectionDrag([stable], label: stable.name)!.data(forType: DiskModel.collectionDragType)!
        let newRoot = base.appendingPathComponent("new-map")
        try fm.createDirectory(at: newRoot, withIntermediateDirectories: true)
        model.scan(newRoot)
        precondition(!model.isValidatingCollection && !model.canAcceptCollectionDrop && !model.collectDrag(data: rootChangeToken),
                     "Changing the scan root must invalidate drag payloads")
        try await wait("replacement collection map") { model.root?.id == newRoot.path && !model.isScanning }
        model.cancelScan()
        precondition(model.collection.isEmpty)
        print("PASS: map/list hover relation, search row drag independent of selection, grouped collection deduplication, protected items, private one-use drag payloads, cancelled drag, stale directory/file rejection, background group validation")
    }

    @MainActor static func checkPermanentRemoval(base: URL, preferences: UserDefaults) async throws {
        let fm = FileManager.default
        let root = base.appendingPathComponent("removal-fixture")
        let cache = base.appendingPathComponent("removal-cache")
        let bundle = root.appendingPathComponent("Bundle")
        let staleDirectory = root.appendingPathComponent("StaleFolder")
        let protected = root.appendingPathComponent(".protected")
        let outside = base.appendingPathComponent("outside-removal-target")
        for directory in [bundle.appendingPathComponent("nested"), staleDirectory, protected, outside] {
            try fm.createDirectory(at: directory, withIntermediateDirectories: true)
        }
        let removeFile = root.appendingPathComponent("remove.bin")
        let staleFile = root.appendingPathComponent("stale.bin")
        let outsideFile = outside.appendingPathComponent("keep.bin")
        let outsideContents = Data([21, 22, 23])
        try Data([1, 2]).write(to: removeFile)
        try Data([3, 4]).write(to: bundle.appendingPathComponent("nested/data.bin"))
        try Data([5, 6]).write(to: staleFile)
        try Data([7, 8]).write(to: staleDirectory.appendingPathComponent("old.bin"))
        try Data([9]).write(to: protected.appendingPathComponent("keep.bin"))
        try outsideContents.write(to: outsideFile)
        try fm.createSymbolicLink(at: bundle.appendingPathComponent("external-link"), withDestinationURL: outside)

        let model = DiskModel(cacheDirectory: cache, preferences: preferences)
        model.scan(root)
        try await wait("permanent removal map") { model.root != nil && !model.isScanning }
        model.cancelScan()
        precondition(!model.collect([model.root!, model.node(withID: protected.path)!]) && model.collection.isEmpty,
                     "Mapped roots and protected folders must never enter the collection")
        model.deleteCollectionPermanently()
        precondition(!model.isRemoving && fm.fileExists(atPath: root.path) && fm.fileExists(atPath: protected.path))

        let removals = [removeFile, bundle, staleFile, staleDirectory].map { model.node(withID: $0.path)! }
        precondition(model.collect(removals) && model.collection.count == 4)
        let originalStaleFile = base.appendingPathComponent("original-stale.bin")
        let originalStaleDirectory = base.appendingPathComponent("original-stale-folder")
        try fm.moveItem(at: staleFile, to: originalStaleFile)
        try Data([10, 11, 12]).write(to: staleFile)
        try fm.moveItem(at: staleDirectory, to: originalStaleDirectory)
        try fm.createDirectory(at: staleDirectory, withIntermediateDirectories: true)
        try Data([13, 14]).write(to: staleDirectory.appendingPathComponent("replacement.bin"))

        model.deleteCollectionPermanently()
        precondition(model.isRemoving, "Removal must run in the background")
        try await wait("permanent removal result") { !model.isRemoving }
        precondition(!fm.fileExists(atPath: removeFile.path) && !fm.fileExists(atPath: bundle.path),
                     "Confirmed permanent removal must remove the selected file and directory")
        let outsideAfterRemoval = try Data(contentsOf: outsideFile)
        precondition(outsideAfterRemoval == outsideContents,
                     "Removing a directory must unlink nested symlinks without deleting their targets")
        precondition(fm.fileExists(atPath: staleFile.path) && fm.fileExists(atPath: staleDirectory.path)
                     && fm.fileExists(atPath: originalStaleFile.path) && fm.fileExists(atPath: originalStaleDirectory.path),
                     "Changed file and directory identities must be rejected without touching either version")
        precondition(Set(model.collection.map(\.id)) == [staleFile.path, staleDirectory.path] && model.showCollection,
                     "Failed removals must remain available for review")
        precondition(model.errorMessage?.contains("stale.bin") == true && model.errorMessage?.contains("StaleFolder") == true)
        try await wait("map after permanent removal") {
            !model.isRefreshing && !model.isOpeningFolder
                && model.current?.children.contains(where: { $0.id == removeFile.path || $0.id == bundle.path }) == false
        }
        precondition(model.statusMessage?.contains("Deleted 2 items permanently.") == true,
                     "The removal outcome must survive the ensuing map update")
        precondition(model.errorMessage?.contains("stale.bin") == true && model.collection.count == 2)
        let store = DiskIndexStore(directory: cache)
        let refreshed = store.savedSnapshot(for: root)!
        let removedFileEntry = try store.node(path: removeFile.path, snapshot: refreshed)
        let removedDirectoryEntry = try store.node(path: bundle.path, snapshot: refreshed)
        let replacedFileEntry = try store.node(path: staleFile.path, snapshot: refreshed)
        precondition(removedFileEntry == nil)
        precondition(removedDirectoryEntry == nil)
        precondition(replacedFileEntry?.size == 3,
                     "The index must refresh rejected replacement paths as well as successful removals")

        model.cancelScan()
        model.collection = []
        precondition(model.collect([model.node(withID: staleFile.path)!]))
        let originalRoot = base.appendingPathComponent("original-removal-root")
        try fm.moveItem(at: root, to: originalRoot)
        try fm.createDirectory(at: root, withIntermediateDirectories: true)
        try fm.moveItem(at: originalRoot.appendingPathComponent("stale.bin"), to: staleFile)
        model.deleteCollectionPermanently()
        try await wait("replaced root removal rejection") { !model.isRemoving }
        precondition(fm.fileExists(atPath: staleFile.path) && model.collection.count == 1 && model.updatesPaused,
                     "A replaced root must block removal even when the collected file still has the same identity")
        precondition(model.errorMessage?.contains("mapped folder changed") == true)
        let outsideAfterRejection = try Data(contentsOf: outsideFile)
        precondition(outsideAfterRejection == outsideContents)
        model.cancelScan()
        print("PASS: permanent file/directory removal, nested symlink target safety, protected roots, stale file/directory/root rejection, partial-result collection retention, index refresh")
    }

    @MainActor static func main() async throws {
        checkCollectionCoverage()
        let base = URL(fileURLWithPath: CommandLine.arguments[1]).standardizedFileURL
        let root = base.appendingPathComponent("fixture")
        let nested = root.appendingPathComponent("Projects/One/Assets")
        let cache = base.appendingPathComponent("cache")
        let fm = FileManager.default
        try fm.createDirectory(at: nested, withIntermediateDirectories: true)
        try Data(repeating: 1, count: 1234).write(to: nested.appendingPathComponent("first.bin"))
        try Data(repeating: 2, count: 4321).write(to: root.appendingPathComponent("other.bin"))
        let suite = "local.orbitdisk.modelcheck.\(UUID().uuidString)"
        let prefs = UserDefaults(suiteName: suite)!
        defer { prefs.removePersistentDomain(forName: suite) }
        checkGroupedHoverCaching(base: base, preferences: prefs)
        checkPreparedGroupHover(base: base, preferences: prefs)
        let model = DiskModel(cacheDirectory: cache, preferences: prefs)
        model.scan(root)
        try await wait("first map") { model.root != nil && !model.isScanning }
        precondition(model.errorMessage == nil, model.errorMessage ?? "")
        precondition(model.root!.size == 5555)
        let store = DiskIndexStore(directory: cache)
        let first = store.savedSnapshot(for: root)!
        model.scan(root)
        precondition(!model.isScanning, "Same-root click must not scan")
        precondition(store.savedSnapshot(for: root)!.id == first.id)
        model.query = "first"
        try await wait("indexed search") { !model.isSearching && model.searchResults.count == 1 }
        precondition(model.searchResults[0].name == "first.bin")
        model.query = ""
        model.scan(nested)
        precondition(!model.isScanning, "Opening a location already inside the index must not scan")
        precondition(store.savedSnapshot(for: nested) == nil, "Reuse the parent index instead of saving a duplicate")
        try await wait("deep folder") { model.current?.id == nested.path && !model.isOpeningFolder }
        precondition(model.current?.children.count == 1)
        model.selectedID = model.current?.children[0].id
        model.collectSelection()
        precondition(model.collection.count == 1)
        let hovered = model.current!.children[0]
        let revisionBeforeUpdate = model.mapRevision
        model.hoveredMapNodes = [hovered]
        model.hoveredListID = hovered.id
        precondition(model.mapRevision == revisionBeforeUpdate)
        try Data(repeating: 3, count: 777).write(to: nested.appendingPathComponent("live.bin"))
        try await wait("live incremental update") { model.root?.size == 6332 && !model.isRefreshing && !model.isOpeningFolder }
        precondition(model.collection.count == 1, "Refresh must preserve collection")
        precondition(model.current?.id == nested.path, "Refresh must preserve navigation")
        precondition(model.current?.children.count == 2)
        precondition(model.displayedNodes.count == 2 && Set(model.displayedNodes.map(\.id)) == Set(model.current!.children.map(\.id)),
                     "Publishing refreshed data for the same folder must replace cached list rows")
        precondition(model.hoveredMapNodes.map(\.id) == [hovered.id] && model.hoveredListID == hovered.id,
                     "Refreshing the same folder must preserve hover for an unchanged item")
        precondition(model.mapRelatedRowIDs == [hovered.id], "The list must remain synchronized with the hovered segment after refresh")
        precondition(model.mapRevision != revisionBeforeUpdate, "Published updates must replace the map revision")
        model.goBack()
        precondition(model.hoveredMapNodes.isEmpty && model.hoveredListID == nil,
                     "Navigating to another folder must clear the previous hover")
        try await wait("parent navigation") { model.current?.id == nested.deletingLastPathComponent().path && !model.isOpeningFolder }
        precondition(model.selectedID == nested.path)
        model.query = "pending search"
        model.cancelScan()
        precondition(!model.isSearching, "Pause must stop the search indicator")
        let prior = store.savedSnapshot(for: root)!
        let reopened = DiskModel(cacheDirectory: cache, preferences: prefs)
        let began = Date()
        reopened.restoreLastLocation()
        try await wait("cached relaunch") { reopened.root != nil && !reopened.isScanning }
        precondition(reopened.isSavedMap)
        precondition(store.savedSnapshot(for: root)!.id == prior.id, "Relaunch must reuse saved index")
        print(String(format: "Cached model reopened in %.3fs", Date().timeIntervalSince(began)))
        reopened.cancelScan()
        try Data(repeating: 4, count: 111).write(to: root.appendingPathComponent("offline.bin"))
        let offline = DiskModel(cacheDirectory: cache, preferences: prefs)
        offline.restoreLastLocation()
        try await wait("offline replay") { offline.root?.size == 6443 && !offline.isRefreshing && !offline.isOpeningFolder }
        precondition(offline.errorMessage == nil, offline.errorMessage ?? "")
        let offlineSnapshot = store.savedSnapshot(for: root)!
        offline.open(try store.node(path: nested.path, snapshot: offlineSnapshot)!)
        try await wait("navigate before full refresh") { offline.current?.id == nested.path && !offline.isOpeningFolder }
        offline.rescan()
        offline.goBack()
        precondition(!offline.isScanning && offline.isRefreshing && offline.root != nil, "Full refresh must retain map")
        try await wait("explicit full refresh") { !offline.isRefreshing && !offline.isOpeningFolder }
        precondition(offline.root!.size == 6443)
        precondition(offline.current?.id == nested.deletingLastPathComponent().path, "Full refresh must preserve navigation made during the scan")
        offline.cancelScan()
        print("PASS: cache reuse, deep navigation, indexed search, live update, collection/navigation retention, cached relaunch, offline replay, nonblocking full refresh")
        try await checkProgressiveMapping(base: base, preferences: prefs)
        try await checkCachedRootValidation(base: base, preferences: prefs)
        try await checkCollectionInteraction(base: base, preferences: prefs)
        try await checkPermanentRemoval(base: base, preferences: prefs)
    }
}
