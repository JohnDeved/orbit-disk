import AppKit
import Combine
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

    func pauseOnce() {
        lock.lock()
        let shouldWait = !released && callbacks == 0
        if shouldWait { callbacks = 1; waiting = true }
        lock.unlock()
        if shouldWait {
            semaphore.wait()
            lock.lock(); waiting = false; lock.unlock()
        }
    }

    func release() {
        lock.lock()
        released = true
        lock.unlock()
        semaphore.signal()
    }
}

final class CollectionGate: @unchecked Sendable {
    private let lock = NSLock()
    private let semaphore = DispatchSemaphore(value: 0)
    private var armed = false
    private var waiting = false
    func arm() { lock.lock(); armed = true; waiting = false; lock.unlock() }
    var isWaiting: Bool { lock.lock(); defer { lock.unlock() }; return waiting }
    func blockOnce() {
        lock.lock(); let take = armed; if take { armed = false; waiting = true }; lock.unlock()
        guard take else { return }
        semaphore.wait()
        lock.lock(); waiting = false; lock.unlock()
    }
    func release() { semaphore.signal() }
}

@main struct ModelCheck {
    static func checkCollectedAncestorParity() {
        func reference(_ path: String, _ collected: Set<String>, _ includingSelf: Bool) -> Bool {
            var candidate = includingSelf ? path : URL(fileURLWithPath: path).deletingLastPathComponent().path
            while true {
                if collected.contains(candidate) { return true }
                if candidate == "/" { return false }
                let next = URL(fileURLWithPath: candidate).deletingLastPathComponent().path
                if next == candidate { return false }
                candidate = next
            }
        }
        let paths = ["/", "/a/b/c", "/a//b", "/a/b/", "/a/./b", "/a/../b", "/a/.", "/a/..",
                     "/a/é/file", "/a/e\u{0301}/file", "/a/\u{0301}name", "/a/\0b/c", "relative/file", "../file"]
        for path in paths {
            let parent = URL(fileURLWithPath: path).deletingLastPathComponent().path
            for collected in [Set<String>(), ["/"], [parent], [path], ["/unrelated"], ["/a/é"]] {
                for includingSelf in [false, true] {
                    precondition(hasCollectedAncestor(path, in: collected, includingSelf: includingSelf)
                                 == reference(path, collected, includingSelf), "Collection path ancestry must match Foundation")
                }
            }
        }
        for path in ["/a/../b", "/a/.."] {
            precondition(!hasCollectedAncestor(path, in: []))
            precondition(hasCollectedAncestor(path, in: ["/a/.."]))
        }
        print("PASS: collection ancestor lookup matches Foundation for normalized, unusual, Unicode and relative paths")
    }

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
        let repeated = CollectionCoverage(paths: ["/a/b/one", "/a/b/two", "/a/c/three", "/K/one", "/\u{212A}/two"])
        for path in ["/", "/a", "/a/b", "/a/c", "/K", "/\u{212A}"] {
            precondition(repeated.state(for: path) == .containsItems, "Shared ancestors must remain discoverable")
        }
        precondition(repeated.state(for: "/a/b/one") == .included && repeated.state(for: "/a/b/missing") == .none)
        precondition(repeated.state(for: "/K/two") == .none && repeated.state(for: "/\u{212A}/one") == .none,
                     "Equivalent Unicode spellings must keep separate ancestor chains")
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

    static func checkRefreshPlanner(base: URL) throws {
        let composed = "/\u{00E9}"
        let decomposed = "/e\u{0301}"
        precondition(composed == decomposed && Data(composed.utf8) != Data(decomposed.utf8))
        var cache = RefreshIndexMetadataCache()
        var reads = 0
        let firstMetadata = cache.lookup(composed) { reads += 1; return (true, false) }
        let secondMetadata = cache.lookup(decomposed) { reads += 1; return (false, true) }
        precondition(firstMetadata?.isDirectory == true && firstMetadata?.isAccessible == false)
        precondition(secondMetadata?.isDirectory == false && secondMetadata?.isAccessible == true)
        _ = cache.lookup("missing" + composed) { reads += 1; return nil }
        let present = cache.lookup("missing" + decomposed) { reads += 1; return (true, true) }
        _ = cache.lookup(composed) { reads += 1; return nil }
        _ = cache.lookup("missing" + composed) { reads += 1; return (false, false) }
        precondition(present?.isDirectory == true && reads == 4, "Indexed paths and cached misses must use byte-exact keys")
        let fm = FileManager.default
        let root = base.appendingPathComponent("planner-root")
        let first = root.appendingPathComponent("a")
        let child = first.appendingPathComponent("child")
        let sibling = root.appendingPathComponent("ab")
        let outside = base.appendingPathComponent("planner-outside")
        for directory in [child, sibling, outside] {
            try fm.createDirectory(at: directory, withIntermediateDirectories: true)
        }
        let store = DiskIndexStore(directory: base.appendingPathComponent("planner-index"))
        let snapshot = try store.save(DiskScanner().scan(root), eventID: 1, eventIdentity: "fixture")
        let overlapping = try refreshDirectories([first.path, child.path, sibling.path, outside.path], store: store, snapshot: snapshot)
        precondition(Set(overlapping) == [first.path, sibling.path], "Only true descendants should be coalesced")
        let missing = try refreshDirectories([child.appendingPathComponent("new/file").path], store: store, snapshot: snapshot)
        precondition(missing == [child.path], "Missing paths must refresh their nearest indexed folder")

        let savedChild = base.appendingPathComponent("planner-saved-child")
        try fm.moveItem(at: child, to: savedChild)
        try fm.createSymbolicLink(at: child, withDestinationURL: outside)
        let replaced = try refreshDirectories([child.path], store: store, snapshot: snapshot)
        precondition(replaced == [first.path], "A replaced ancestor must be refreshed without following its link")
        try fm.removeItem(at: child)
        try fm.moveItem(at: savedChild, to: child)

        let cancelled = ScanCancellation()
        cancelled.cancel()
        do {
            _ = try refreshDirectories([first.path], store: store, snapshot: snapshot, cancellation: cancelled)
            preconditionFailure("Cancelled refresh planning must stop")
        } catch is CancellationError {}
        _ = try store.save(DiskScanner().scan(root), eventID: 2, eventIdentity: "fixture")
        do {
            _ = try refreshDirectories([first.path], store: store, snapshot: snapshot)
            preconditionFailure("Refresh planning must reject a stale snapshot")
        } catch DiskIndexError.staleSnapshot {}
        print("PASS: refresh planning coalesces paths, protects replaced ancestors, and honors cancellation and snapshot revisions")
    }

    @MainActor static func checkLargeListHoverLookup(base: URL, preferences: UserDefaults) {
        let rootURL = base.appendingPathComponent("large-list")
        let children = (0..<1_200).map { index in
            let name = String(format: "%04d", 1_199 - index)
            return DiskNode(url: rootURL.appendingPathComponent(name), name: name, isDirectory: false,
                            size: 1, allocatedSize: 1, fileCount: 1)
        }
        func folder(_ children: [DiskNode]) -> DiskNode {
            DiskNode(url: rootURL, name: "large-list", isDirectory: true, size: 1_200,
                     allocatedSize: 1_200, fileCount: 1_200, children: children)
        }
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("large-list-cache"), preferences: preferences)
        model.navigation = [folder(children)]
        let ids: Set<String> = [children[1_190].id, children[800].id]
        precondition(model.firstDisplayedRowIndex(matching: ids) == 800)
        precondition(model.firstDisplayedRowIndex(matching: ["/missing"]) == nil)
        model.sortByName = true
        precondition(model.firstDisplayedRowIndex(matching: ids) == 9, "Sorting must invalidate row indices")
        model.sortByName = false
        let reversed = Array(children.reversed())
        model.navigation = [folder(reversed)]
        precondition(model.firstDisplayedRowIndex(matching: ids) == 9, "Refreshing a folder must invalidate row indices")
        model.query = "fixture"
        model.searchResults = children
        precondition(model.firstDisplayedRowIndex(matching: ids) == 800)
        model.searchResults = reversed
        precondition(model.firstDisplayedRowIndex(matching: ids) == 9, "Search result changes must invalidate row indices")
        precondition(model.firstDisplayedRowIndex(matching: Set(children.map(\.id))) == 0)
        print("PASS: large-list hover row lookup follows sorting, refresh, and search changes")
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
        func gateAtListing(_ model: DiskModel, _ gate: PreviewGate, _ root: URL) {
            model.testAfterStageStep = { stage in
                guard let view = try? stage.view(path: nil, removals: [], cancellation: ScanCancellation()),
                      view.folder.id == root.path, view.folder.children.count == 2, !view.isListing else { return }
                gate.pauseOnce()
            }
        }
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
        let model = DiskModel(cacheDirectory: cache, preferences: preferences)
        gateAtListing(model, gate, root)
        let started = Date()
        model.scan(root)
        try await wait("first partial listing", timeout: 4) {
            gate.isWaiting && model.isPartialMap && model.current?.children.count == 2 &&
                model.current?.children.first(where: { $0.id == leaf.path })?.isScanComplete == true
        }
        let firstPreviewTime = Date().timeIntervalSince(started)
        precondition(model.isScanning && model.indexedAt == nil && model.scanResult == nil)
        precondition(DiskIndexStore(directory: cache).savedSnapshot(for: root) == nil,
                     "The first browsable map must precede scan completion and persistence")
        let pending = model.current!.children.first { $0.id == folder.path }!
        let ready = model.current!.children.first { $0.id == leaf.path }!
        precondition(!pending.isScanComplete && pending.children.isEmpty && ready.isScanComplete)
        precondition(model.canCollect(pending), "Visible incomplete folders must be collectable")
        precondition(model.canCollect(ready) && model.collect([ready]))
        try await wait("accepted collection") { !model.isValidatingCollection }
        model.query = "ready"
        try await wait("partial preview search") { !model.isSearching && model.searchResults.contains { $0.id == ready.id } }
        precondition(model.isPartialMap, "Partial search must use the current preview")
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
        let paused = DiskModel(cacheDirectory: base.appendingPathComponent("paused-preview-cache"), preferences: preferences)
        gateAtListing(paused, cancelGate, root)
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
        cancelGate.arm(callback: 1)
        paused.testAfterStageAttach = { cancelGate.pauseOnce() }
        paused.rescan()
        try await wait("restart before folder discovery") { cancelGate.isWaiting }
        try await Task.sleep(nanoseconds: 500_000_000)
        precondition(paused.current?.id == folder.path,
                     "Restart must not discard an existing route before rediscovering its parent listing")
        cancelGate.release()
        try await wait("restart partial scan") { !paused.isScanning && !paused.isOpeningFolder && !paused.isPartialMap }
        precondition(paused.current?.id == folder.path && paused.current?.children.first?.name == "inside.bin",
                     "Restart and final persistence must retain the folder visited in a partial map")
        paused.cancelScan()

        let selectionGate = PreviewGate()
        defer { selectionGate.release() }
        let selectionModel = DiskModel(cacheDirectory: base.appendingPathComponent("selection-preview-cache"), preferences: preferences)
        gateAtListing(selectionModel, selectionGate, root)
        selectionModel.scan(root)
        try await wait("selection fixture partial") { selectionGate.isWaiting && selectionModel.current?.children.count == 2 }
        selectionModel.selectedID = leaf.path
        selectionModel.cancelScan()
        selectionGate.release()
        try await wait("cancelled selection worker") { !selectionGate.isWaiting }
        selectionGate.arm(callback: 1)
        selectionModel.testAfterStageAttach = { selectionGate.pauseOnce() }
        selectionModel.rescan()
        try await wait("restart retains partial preview") {
            selectionGate.isWaiting && selectionModel.current?.children.count == 2
        }
        precondition(selectionModel.selectedID == leaf.path && selectionModel.selected?.id == leaf.path,
                     "A restarted scan must retain its partial map and selection")
        selectionGate.release()
        try await wait("selection after restart") { !selectionModel.isScanning && !selectionModel.isPartialMap }
        precondition(selectionModel.selectedID == leaf.path && selectionModel.selected?.id == leaf.path)
        selectionModel.cancelScan()

        let switchedRoot = base.appendingPathComponent("switched-preview-fixture")
        try fm.createDirectory(at: switchedRoot, withIntermediateDirectories: true)
        try Data([8]).write(to: switchedRoot.appendingPathComponent("different.bin"))
        let switchGate = PreviewGate()
        defer { switchGate.release() }
        let switched = DiskModel(cacheDirectory: base.appendingPathComponent("switched-preview-cache"), preferences: preferences)
        gateAtListing(switched, switchGate, root)
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
        let failed = DiskModel(cacheDirectory: failureCache, preferences: preferences)
        gateAtListing(failed, failureGate, root)
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
        try await wait("accepted collection") { !failed.isValidatingCollection }
        failed.deleteCollectionPermanently()
        try await wait("remove from failed-save partial map") { !failed.isRemoving && !fm.fileExists(atPath: folder.appendingPathComponent("inside.bin").path) }
        failed.goBack()
        try await wait("failed-save parent navigation") { failed.current?.id == root.path && !failed.isOpeningFolder }
        failed.cancelScan()
        print("PASS: partial map retained after cancel, pending navigation after stop, restart route retention, stale root-switch suppression, failed-save browsing and removal lock")
    }

    @MainActor static func checkLargeSavedFolder(base: URL, preferences: UserDefaults) async throws {
        let fm = FileManager.default
        let root = base.appendingPathComponent("large-saved-fixture")
        let cache = base.appendingPathComponent("large-saved-index")
        let firstURL = root.appendingPathComponent("first-folder", isDirectory: true)
        let secondURL = root.appendingPathComponent("second-folder", isDirectory: true)
        try fm.createDirectory(at: firstURL, withIntermediateDirectories: true)
        try fm.createDirectory(at: secondURL, withIntermediateDirectories: true)
        let fileCount = 3_000
        for index in 0..<fileCount {
            try Data([1]).write(to: root.appendingPathComponent("document-\(index).bin"))
        }
        let targetID = root.appendingPathComponent("document-2999.bin").path
        let checkpoint = try DiskChangeMonitor.capture(root: root)
        let scanned = try DiskScanner().scan(root)
        _ = try DiskIndexStore(directory: cache).save(scanned,
                                                     eventID: checkpoint.eventID, eventIdentity: checkpoint.identity)
        let model = DiskModel(cacheDirectory: cache, preferences: preferences)
        model.scan(root)
        try await wait("large saved root") { model.isSavedMap && !model.isScanning && model.current?.children.count == fileCount + 2 }
        precondition(model.displayedItems.count == fileCount + 2 && model.childColorIndices.count == fileCount + 2)
        let preparedItems = model.displayedItems
        precondition(!model.displayedNodes.isEmpty)
        precondition(preparedItems.withUnsafeBufferPointer { prepared in
            model.displayedItems.withUnsafeBufferPointer { prepared.baseAddress == $0.baseAddress }
        }, "Reading displayed nodes must retain the prepared row identities")
        precondition(model.node(withID: targetID)?.size == 1)
        model.query = "document-2999"
        try await wait("large saved search") { !model.isSearching && model.searchResults.count == 1 }
        precondition(model.displayedItems.first?.id == targetID)
        model.query = ""
        let first = model.current!.children.first { $0.id == firstURL.path }!
        let second = model.current!.children.first { $0.id == secondURL.path }!
        model.open(first)
        model.open(second)
        try await wait("latest large-folder navigation") { model.current?.id == secondURL.path && !model.isOpeningFolder }
        model.query = "document"
        try await wait("large filtered subfolder search") { !model.isSearching && model.searchResults.count == 300 }
        let searchOnlyID = model.searchResults[0].id
        model.selectedID = searchOnlyID
        try Data([8]).write(to: secondURL.appendingPathComponent("new.bin"))
        try await wait("large filtered subfolder refresh") {
            model.current?.children.count == 1 && !model.isRefreshing && !model.isOpeningFolder && !model.isSearching
        }
        precondition(model.isFiltering && model.searchResults.count == 300 && model.searchResults.allSatisfy { model.node(withID: $0.id) != nil }
                     && model.selectedID == searchOnlyID && model.node(withID: searchOnlyID) != nil,
                     "Refreshing a filtered folder must retain current and selected search-result lookups")
        model.query = ""
        model.goBack()
        precondition(model.isOpeningFolder, "A cached large folder must prepare off the main actor")
        model.open(first)
        try await wait("superseded large-folder navigation") { model.current?.id == firstURL.path && !model.isOpeningFolder }
        try await Task.sleep(nanoseconds: 200_000_000)
        precondition(model.current?.id == firstURL.path, "A stale background preparation must not replace newer navigation")
        model.goBack()
        try await wait("cached large-folder return") { model.current?.id == root.path && !model.isOpeningFolder }
        precondition(model.displayedItems.count == fileCount + 2 && model.node(withID: targetID)?.size == 1)
        model.selectedID = targetID
        let sizeItems = model.displayedItems
        let sizeOrder = sizeItems.map(\.id)
        model.sortByName = true
        precondition(model.isSortingRows && model.displayedItems.map(\.id) == sizeOrder && model.selectedID == targetID,
                     "The existing rows and selection must remain usable while a large name sort runs")
        precondition(sizeItems.withUnsafeBufferPointer { previous in
            model.displayedItems.withUnsafeBufferPointer { previous.baseAddress == $0.baseAddress }
        }, "Starting a background sort must reuse the displayed row identities")
        model.sortByName = false
        precondition(!model.isSortingRows && model.displayedItems.map(\.id) == sizeOrder)
        precondition(sizeItems.withUnsafeBufferPointer { previous in
            model.displayedItems.withUnsafeBufferPointer { previous.baseAddress == $0.baseAddress }
        }, "Cancelling the sort must retain the still-visible row identities")
        try await Task.sleep(nanoseconds: 200_000_000)
        precondition(!model.sortByName && model.displayedItems.map(\.id) == sizeOrder,
                     "A cancelled name sort must not replace a later size order")
        model.sortByName = true
        precondition(model.isSortingRows)
        model.query = "document-2999"
        try await wait("search supersedes large sort") { !model.isSearching && !model.isSortingRows && model.searchResults.count == 1 }
        precondition(model.displayedItems.map(\.id) == [targetID])
        model.setMapInteractionActive(true)
        model.query = ""
        try await Task.sleep(nanoseconds: 250_000_000)
        precondition(model.isSortingRows && model.displayedItems.map(\.id) == sizeOrder,
                     "A held map or row gesture must defer publishing a background sort")
        model.setMapInteractionActive(false)
        try await wait("large name sort") { !model.isSortingRows && model.displayedNodes.count == fileCount + 2 }
        let names = model.displayedNodes.map(\.name)
        precondition(zip(names, names.dropFirst()).allSatisfy { $0.localizedStandardCompare($1) != .orderedDescending },
                     "The completed background sort must use localized name ordering")
        model.sortByName = false
        precondition(sizeItems.withUnsafeBufferPointer { previous in
            model.displayedItems.withUnsafeBufferPointer { previous.baseAddress == $0.baseAddress }
        }, "Returning to size order must reuse the prepared rows")
        try Data([1, 2]).write(to: URL(fileURLWithPath: targetID))
        try await wait("same-count folder metadata refresh") {
            model.node(withID: targetID)?.size == 2 && !model.isRefreshing && !model.isOpeningFolder
        }
        precondition(model.current?.children.count == fileCount + 2
                     && model.displayedItems.first(where: { $0.id == targetID })?.node.size == 2,
                     "A same-count refresh must replace cached row metadata")
        model.query = "document-2999"
        try await wait("refreshed search metadata") { !model.isSearching && model.searchResults.count == 1 }
        precondition(model.displayedItems.first?.node.size == 2)
        model.query = ""
        precondition(model.displayedItems.first(where: { $0.id == targetID })?.node.size == 2,
                     "Leaving search must restore the refreshed size-order rows")
        model.cancelScan()

        let previewCache = base.appendingPathComponent("large-preview-index")
        let gate = PreviewGate()
        defer { gate.release() }
        let preview = DiskModel(cacheDirectory: previewCache, preferences: preferences)
        gate.arm(callback: 1)
        preview.testAfterStageStep = { stage in
            guard let view = try? stage.view(path: nil, removals: [], cancellation: ScanCancellation()),
                  view.folder.children.count == fileCount + 2 else { return }
            gate.pauseOnce()
        }
        preview.scan(root)
        try await wait("large preview listing") {
            gate.isWaiting && preview.isPartialMap && preview.current?.children.count == fileCount + 2
        }
        precondition(preview.displayedItems.count == fileCount + 2 && preview.node(withID: targetID)?.size == 2)
        let previewColors = preview.childColorIndices
        let heldRevision = preview.mapRevision
        preview.setMapInteractionActive(true)
        gate.release()
        try await wait("large preview index save") { DiskIndexStore(directory: previewCache).savedSnapshot(for: root) != nil }
        precondition(preview.isPartialMap && preview.mapRevision == heldRevision,
                     "A held graph gesture must defer a large preview and its final handoff")
        preview.setMapInteractionActive(false)
        try await wait("large preview completion") { !preview.isScanning && !preview.isPartialMap && !preview.isOpeningFolder }
        precondition(preview.childColorIndices == previewColors && preview.node(withID: targetID)?.size == 2)
        preview.cancelScan()
        print("PASS: large saved and preview folders, filtered refresh, search, cached reopening, stale navigation, deferred gesture, and cancellable name sorting")
    }

    @MainActor static func checkSearchResultRetention(base: URL, preferences: UserDefaults) async throws {
        let root = base.appendingPathComponent("search-retention-fixture", isDirectory: true)
        let nested = root.appendingPathComponent("nested", isDirectory: true)
        try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: true)
        for index in 0..<110 {
            try Data([1, 2]).write(to: nested.appendingPathComponent("filler-\(index).bin"))
        }
        let alphaURL = nested.appendingPathComponent("alpha-target.bin")
        let hoveredURL = nested.appendingPathComponent("alpha-target-two.bin")
        let betaURL = nested.appendingPathComponent("beta-target.bin")
        try Data([1]).write(to: alphaURL)
        try Data([1]).write(to: hoveredURL)
        try Data([1]).write(to: betaURL)
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("search-retention-cache"), preferences: preferences)
        model.scan(root)
        try await wait("search retention index") { model.current?.id == root.path && !model.isScanning && !model.isPartialMap }
        model.cancelScan()
        precondition(model.node(withID: alphaURL.path) == nil && model.node(withID: hoveredURL.path) == nil && model.node(withID: betaURL.path) == nil,
                     "Search targets must be outside the bounded folder preview")

        model.query = "alpha-target"
        try await wait("first search") { !model.isSearching && model.searchResults.count == 2 }
        let alpha = model.searchResults.first { $0.id == alphaURL.path }!
        model.query = "beta-target"
        model.selectedID = alpha.id
        model.hoveredListID = hoveredURL.path
        precondition(model.searchResults.contains { $0.id == alpha.id } && model.selected?.id == alpha.id,
                     "Previous rows must remain usable while the next search runs")
        precondition(model.collect([alpha]), "A previous search row must remain collectable")
        try await wait("accepted collection") { !model.isValidatingCollection }
        try await wait("replacement search") { !model.isSearching && model.searchResults.first?.id == betaURL.path }
        precondition(model.selected?.id == alpha.id && model.hoveredListID == hoveredURL.path &&
                     model.node(withID: hoveredURL.path) != nil && model.collection.first?.id == alpha.id,
                     "Replacement must preserve the selected, hovered and collected prior row")

        model.query = "alpha-target"
        try await wait("third search") { !model.isSearching && model.searchResults.count == 2 }
        precondition(model.node(withID: betaURL.path) == nil, "Unreferenced previous results must be released")
        model.query = ""
        precondition(model.node(withID: alpha.id) == nil && model.node(withID: hoveredURL.path) == nil && model.collection.first?.id == alpha.id,
                     "Clearing search must release its lookup without removing collected items")
        print("PASS: search result retention, pending selection and hover, collection and query clearing")
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
        let reconciliationGate = PreviewGate()
        defer { reconciliationGate.release() }
        cached.testBeforeScannerStart = { reconciliationGate.pauseOnce() }
        cached.scan(root)
        try await wait("cached root before journal validation") {
            reconciliationGate.isWaiting && cached.root != nil && !cached.isScanning
        }
        precondition(cached.isSavedMap && cached.root?.size == 3,
                     "An identity-matching cached root should display before background journal reconciliation")
        let originalID = root.appendingPathComponent("indexed.bin").path
        let originalColor = cached.childColorIndices[originalID]
        precondition(originalColor != nil && cached.displayedItems.map(\.id) == [originalID])
        precondition(cached.mapChildIDs == [originalID])
        cached.searchResults = [DiskNode(url: URL(fileURLWithPath: originalID), name: "indexed.bin",
                                         isDirectory: false, size: 999, allocatedSize: 999, fileCount: 1)]
        let revision = cached.mapRevision
        cached.selectedID = cached.current!.children[0].id
        cached.hoveredMapNodes = [cached.current!.children[0]]
        cached.statusMessage = "The existing map remains usable."
        precondition(cached.mapRevision == revision, "Hover, selection, and status must not invalidate prepared map data")
        reconciliationGate.release()
        try await wait("incompatible journal refresh") {
            !cached.isRefreshing && !cached.isOpeningFolder && !cached.isSavedMap && cached.root?.size == 5
        }
        precondition(cached.mapRevision != revision,
                     "A completed folder publication must invalidate prepared map data")
        let newID = root.appendingPathComponent("new.bin").path
        precondition(cached.childColorIndices[originalID] == originalColor
                     && cached.childColorIndices[newID] != nil
                     && cached.childColorIndices[newID] != originalColor,
                     "Refreshing a folder must preserve existing colors and assign new ones")
        precondition(cached.displayedItems.map(\.id) == cached.current?.children.map(\.id)
                     && cached.node(withID: originalID)?.size == 3 && cached.node(withID: newID)?.size == 2,
                     "Refreshed folder metadata must take precedence over older search results")
        precondition(cached.mapChildIDs == cached.current?.children.map(\.id),
                     "A refreshed folder must publish its own graph child IDs")
        precondition(store.savedSnapshot(for: root)?.eventIdentity != "expired-journal-identity")
        cached.cancelScan()
        let current = cached.current!
        let alternateChildren = current.children.reversed().map { $0 }
        cached.navigation = [DiskNode(url: current.url, name: current.name, isDirectory: true, size: current.size,
                                      allocatedSize: current.allocatedSize, fileCount: current.fileCount,
                                      children: alternateChildren)]
        precondition(cached.mapChildIDs == nil,
                     "Direct navigation replacement must invalidate graph IDs even for the same folder and child count")

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
        precondition(model.selectedID == other.id && model.draggingLabel == sourceRow.name,
                     "The dragged search row must be independent of the existing selection")
        model.endCollectionDrag()
        precondition(model.collectDrag(data: rowToken))
        try await wait("search row collection") { !model.isValidatingCollection }
        precondition(model.collection.map(\.id) == [sourceRow.id],
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
        try await wait("accepted collection") { !model.isValidatingCollection }
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
        precondition(model.collectionSize == model.collection.reduce(0) { $0 + $1.size }, "Collection total must track the current snapshot")
        model.collection.removeAll { $0.id == other.id }
        precondition(model.collectionSize == model.collection.reduce(0) { $0 + $1.size }, "Removing an item must update the total")
        precondition(model.collectionCoverage.state(for: other.id) == .none
                     && model.collectionCoverage.state(for: branch.id) == .included,
                     "Removing a collected item must update cached coverage immediately")
        model.collection = []
        precondition(model.collectionSize == model.collection.reduce(0) { $0 + $1.size }, "Collection total must track the current snapshot")
        precondition(model.collectionCoverage.state(for: root.path) == .none
                     && model.collectionCoverage.state(for: deep.id) == .none,
                     "Clearing the collection must clear coverage immediately")
        precondition(model.collect([deep]))
        try await wait("accepted collection") { !model.isValidatingCollection }
        precondition(model.collectionCoverage.state(for: branch.id) == .containsItems
                     && model.collectionCoverage.state(for: deep.id) == .included,
                     "Collecting a nested file must mark its ancestors as containing collected items")
        model.collection = []
        model.errorMessage = nil

        let item = model.beginCollectionDrag([deep, other], label: "2 items")!
        precondition(item.types == [DiskModel.collectionDragType], "Collection drag must carry only its private token")
        let token = item.data(forType: DiskModel.collectionDragType)!
        precondition(model.draggingLabel == "2 items")
        precondition(model.collection.isEmpty, "Starting a drag must never collect")
        model.endCollectionDrag()
        precondition(model.draggingLabel == nil && model.collection.isEmpty,
                     "Cancelling or ending a drag must never collect")
        precondition(model.canAcceptCollectionDrop, "Drop data may arrive after the native dragging session ends")
        precondition(!model.collectDrag(data: Data("unknown token".utf8)) && model.collection.isEmpty)
        precondition(model.collectDrag(data: token))
        try await wait("two-item drop") { !model.isValidatingCollection }
        precondition(model.collection.count == 2)
        precondition(!model.collectDrag(data: token), "A drop token must be consumed once")
        precondition(fm.fileExists(atPath: deep.id) && fm.fileExists(atPath: other.id), "Collecting must not move files")
        model.collection = []

        let directoryToken = model.beginCollectionDrag([branch], label: branch.name)!.data(forType: DiskModel.collectionDragType)!
        let originalBranch = root.appendingPathComponent("Original Branch")
        try fm.moveItem(at: branchURL, to: originalBranch)
        try fm.createDirectory(at: branchURL, withIntermediateDirectories: true)
        precondition(model.collectDrag(data: directoryToken))
        try await wait("replacement directory rejection") { !model.isValidatingCollection }
        precondition(model.collection.isEmpty,
                     "A replacement directory must not be collected using an indexed directory's identity")
        precondition(model.errorMessage?.contains("changed since indexing") == true)
        let fileToken = model.beginCollectionDrag([other], label: other.name)!.data(forType: DiskModel.collectionDragType)!
        let originalOther = root.appendingPathComponent("original-other.bin")
        try fm.moveItem(at: other.url, to: originalOther)
        try Data([8, 9]).write(to: other.url)
        precondition(model.collectDrag(data: fileToken))
        try await wait("replacement file rejection") { !model.isValidatingCollection }
        precondition(model.collection.isEmpty,
                     "A replacement file must not be collected using an indexed file's identity")
        precondition(fm.fileExists(atPath: originalOther.path) && fm.fileExists(atPath: other.id)
                     && fm.fileExists(atPath: originalBranch.appendingPathComponent("Sub/item.bin").path),
                     "Rejected drops must leave fixture files untouched")

        let batchToken = model.beginCollectionDrag(batch, label: "72 items")!.data(forType: DiskModel.collectionDragType)!
        precondition(model.collectDrag(data: batchToken))
        precondition(model.isValidatingCollection && model.collection.isEmpty,
                     "Large drops must validate in the background before applying one batch")
        precondition(model.canCollect(stable) && !model.canAcceptCollectionDrop && !model.collectDrag(data: batchToken),
                     "Validation must leave new items actionable while consuming each drop token once")
        model.statusMessage = "A newer action status."
        try await wait("large grouped drop") { !model.isValidatingCollection }
        precondition(model.collection.count == 72 && Set(model.collection.map(\.id)) == Set(batch.map(\.id)))
        precondition(model.statusMessage == "A newer action status.", "A completed background validation must not overwrite a newer status")
        precondition(batch.allSatisfy { fm.fileExists(atPath: $0.id) }, "Grouped collection must not move files")
        model.collection = []
        precondition(model.collect([stable]))
        try await wait("accepted collection") { !model.isValidatingCollection }
        let mixed = [stable, protected, model.root!] + batch + [batch[0]]
        let mixedToken = model.beginCollectionDrag(mixed, label: "mixed items")!.data(forType: DiskModel.collectionDragType)!
        model.endCollectionDrag()
        precondition(model.collectDrag(data: mixedToken) && model.isValidatingCollection)
        try await wait("mixed grouped drop") { !model.isValidatingCollection }
        precondition(Set(model.collection.map(\.id)) == Set(([stable] + batch).map(\.id)))
        precondition(model.statusMessage?.contains("2 items already included") == true && model.statusMessage?.contains("Skipped 2 items") == true,
                     "Deferred grouping must preserve duplicate and rejected-item reporting")
        let includedDrag = model.beginCollectionDrag(Array(repeating: stable, count: 72), label: "already collected")!
        precondition(model.collectDrag(data: includedDrag.data(forType: DiskModel.collectionDragType)!))
        try await wait("already included group") { !model.isValidatingCollection }
        precondition(Set(model.collection.map(\.id)) == Set(([stable] + batch).map(\.id)),
                     "Large group eligibility runs off-main and must not duplicate collected items")
        model.collection = []
        var directArmed = true
        let directObserver = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if directArmed { directArmed = false; model.collection = [stable] }
            }
        }
        model.collection = [batch[0]]
        directObserver.cancel()
        precondition(model.collectionSize == model.collection.reduce(0) { $0 + $1.size }, "Collection total must track the current snapshot")
        precondition(!directArmed && model.collection.map(\.id) == [stable.id]
                     && model.collectionCoverage.state(for: stable.id) == .included,
                     "Later reentrant direct assignment must win without inconsistent coverage")
        model.collection = []
        var collectArmed = true
        let collectObserver = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if collectArmed { collectArmed = false; model.collection = [batch[0]] }
            }
        }
        precondition(model.collect([stable]))
        try await wait("accepted collection") { !model.isValidatingCollection }
        collectObserver.cancel()
        precondition(model.collectionSize == model.collection.reduce(0) { $0 + $1.size }, "Collection total must track the current snapshot")
        precondition(!collectArmed && Set(model.collection.map(\.id)) == Set([stable.id, batch[0].id])
                     && model.collectionCoverage.state(for: stable.id) == .included
                     && model.collectionCoverage.state(for: batch[0].id) == .included,
                     "Queued collection must retry against reentrant changes")
        model.collection = []
        precondition(model.collect([stable]))
        try await wait("accepted collection") { !model.isValidatingCollection }
        let publicationGate = CollectionGate()
        model.testCollectionPrepare = { publicationGate.blockOnce() }
        publicationGate.arm()
        model.selectedID = stable.id
        let revisionToken = model.beginCollectionDrag(batch, label: "revision")!.data(forType: DiskModel.collectionDragType)!
        precondition(model.collectDrag(data: revisionToken))
        try await wait("prepare gate") { publicationGate.isWaiting }
        let replacement = DiskNode(url: stable.url, name: stable.name, isDirectory: false, size: 999,
                                   allocatedSize: stable.allocatedSize, fileCount: stable.fileCount,
                                   fileIdentity: stable.fileIdentity)
        model.collection = [replacement]
        publicationGate.release()
        try await wait("changed collection retry") { !model.isValidatingCollection }
        precondition(model.collectionSize == model.collection.reduce(0) { $0 + $1.size }, "Collection total must track the current snapshot")
        precondition(model.collection.count == 73 && model.collection.first(where: { $0.id == stable.id })?.size == 999)
        precondition(model.collectionCoverage.state(for: stable.id) == .included && model.selectedID == stable.id)
        model.collection = []
        publicationGate.arm()
        let switchedToken = model.beginCollectionDrag(batch, label: "switch")!.data(forType: DiskModel.collectionDragType)!
        precondition(model.collectDrag(data: switchedToken))
        try await wait("switch prepare gate") { publicationGate.isWaiting }
        let originalRoot = model.root
        model.root = nil
        model.root = originalRoot
        publicationGate.release()
        try await wait("same-root collection resumes") { !model.isValidatingCollection && model.collection.count == batch.count }
        precondition(Set(model.collection.map(\.id)) == Set(batch.map(\.id)),
                     "An accepted collection must resume when the same root is restored")
        model.collection = [stable]
        publicationGate.arm()
        let reentrantToken = model.beginCollectionDrag(batch, label: "reentrant revision")!.data(forType: DiskModel.collectionDragType)!
        precondition(model.collectDrag(data: reentrantToken))
        try await wait("reentrant prepare gate") { publicationGate.isWaiting }
        var armedRevision = true
        let revisionObserver = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if armedRevision { armedRevision = false; model.collection = [replacement] }
            }
        }
        publicationGate.release()
        try await wait("reentrant collection retry") { !model.isValidatingCollection }
        revisionObserver.cancel()
        precondition(model.collectionSize == model.collection.reduce(0) { $0 + $1.size }, "Collection total must track the current snapshot")
        precondition(!armedRevision && model.collection.count == 73 && model.collection.first(where: { $0.id == stable.id })?.size == 999)
        precondition(model.collectionCoverage.state(for: stable.id) == .included)
        model.collection = []
        publicationGate.arm()
        let reentrantRootToken = model.beginCollectionDrag(batch, label: "reentrant root")!.data(forType: DiskModel.collectionDragType)!
        precondition(model.collectDrag(data: reentrantRootToken))
        try await wait("reentrant root prepare gate") { publicationGate.isWaiting }
        var armedRoot = true
        let rootObserver = model.objectWillChange.sink { _ in
            MainActor.assumeIsolated {
                if armedRoot { armedRoot = false; model.root = nil }
            }
        }
        publicationGate.release()
        try await wait("reentrant root detaches") { !armedRoot && model.root == nil && !model.isValidatingCollection }
        rootObserver.cancel()
        model.root = originalRoot
        try await wait("reentrant root collection resumes") { !model.isValidatingCollection && model.collection.count == batch.count }
        precondition(Set(model.collection.map(\.id)) == Set(batch.map(\.id)),
                     "An accepted collection must return to its original root after a reentrant detachment")
        print("PASS: reentrant collection mutation retries and collection survives root detachment")
        model.collection = []
        publicationGate.arm()
        let cancelledBatchToken = model.beginCollectionDrag(batch, label: "72 items")!.data(forType: DiskModel.collectionDragType)!
        precondition(model.collectDrag(data: cancelledBatchToken) && model.isValidatingCollection)
        try await wait("paused scan collection gate") { publicationGate.isWaiting }
        model.cancelScan()
        precondition(model.isValidatingCollection && model.collection.isEmpty, "Pausing mapping must retain accepted collection work")
        let newerStatus = model.statusMessage
        publicationGate.release()
        try await wait("paused scan collection finishes") { !model.isValidatingCollection && model.collection.count == batch.count }
        precondition(Set(model.collection.map(\.id)) == Set(batch.map(\.id)) && model.statusMessage == newerStatus,
                     "Collection work must finish without overwriting the newer scan status")
        model.testCollectionPrepare = {}
        model.collection = []

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
        precondition(model.collect([model.root!, model.node(withID: protected.path)!]))
        try await wait("protected item rejection") { !model.isValidatingCollection }
        precondition(model.collection.isEmpty,
                     "Mapped roots and protected folders must never enter the collection")
        model.deleteCollectionPermanently()
        precondition(!model.isRemoving && fm.fileExists(atPath: root.path) && fm.fileExists(atPath: protected.path))

        let removals = [removeFile, bundle, staleFile, staleDirectory].map { model.node(withID: $0.path)! }
        precondition(model.collect(removals))
        try await wait("removal collection") { !model.isValidatingCollection }
        precondition(model.collection.count == 4)
        let originalStaleFile = base.appendingPathComponent("original-stale.bin")
        let originalStaleDirectory = base.appendingPathComponent("original-stale-folder")
        try fm.moveItem(at: staleFile, to: originalStaleFile)
        try Data([10, 11, 12]).write(to: staleFile)
        try fm.moveItem(at: staleDirectory, to: originalStaleDirectory)
        try fm.createDirectory(at: staleDirectory, withIntermediateDirectories: true)
        try Data([13, 14]).write(to: staleDirectory.appendingPathComponent("replacement.bin"))

        model.showCollection = true
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
        let removalStore = DiskIndexStore(directory: cache)
        try await wait("map after permanent removal") {
            guard !model.isRefreshing, !model.isOpeningFolder,
                  model.current?.children.contains(where: { $0.id == removeFile.path || $0.id == bundle.path }) == false,
                  let snapshot = removalStore.savedSnapshot(for: root) else { return false }
            return (try? removalStore.node(path: removeFile.path, snapshot: snapshot)) == nil
        }
        precondition(model.statusMessage?.contains("Deleted 2 items permanently.") == true,
                     "The removal outcome must survive the ensuing map update")
        precondition(model.collectionSize == model.collection.reduce(0) { $0 + $1.size }, "Collection total must track the current snapshot")
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
        try await wait("accepted collection") { !model.isValidatingCollection }
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
        setbuf(stdout, nil)
        checkCollectedAncestorParity()
        checkCollectionCoverage()
        let oversized = (0..<2).map { i in
            DiskNode(url: URL(fileURLWithPath: "/synthetic/oversized-\(i)"), name: "oversized-\(i)", isDirectory: false,
                     size: i == 0 ? Int64.max : 1, allocatedSize: 0, fileCount: 1)
        }
        let totalModel = DiskModel(cacheDirectory: URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("orbit-total-\(UUID())"))
        totalModel.collection = oversized
        precondition(totalModel.testCollectionPathIDs() == Set(oversized.map(\.id)), "Membership must match the collection")
        let unicode = ["/synthetic/é", "/synthetic/e\u{0301}/file", "/synthetic/nested/file"].map { path in
            DiskNode(url: URL(fileURLWithPath: path), name: path, isDirectory: false, size: 1, allocatedSize: 1, fileCount: 1)
        }
        totalModel.collection = unicode
        precondition(totalModel.testCollectionPathIDs() == Set(unicode.map(\.id)))
        for path in ["/synthetic/é", "/synthetic/e\u{0301}/file", "/synthetic/nested/file/child", "/synthetic/not-present"] {
            precondition(hasCollectedAncestor(path, in: totalModel.testCollectionPathIDs()) == hasCollectedAncestor(path, in: Set(unicode.map(\.id))))
        }
        totalModel.collection = oversized
        precondition(totalModel.collectionSize == Int64.max, "Collection totals must not overflow")
        totalModel.collection.removeLast()
        precondition(totalModel.collectionSize == Int64.max)
        totalModel.collection = []
        precondition(totalModel.collectionSize == 0)
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
        checkLargeListHoverLookup(base: base, preferences: prefs)
        try checkRefreshPlanner(base: base)
        checkPreparedGroupHover(base: base, preferences: prefs)
        try await checkLargeSavedFolder(base: base, preferences: prefs)
        try await checkSearchResultRetention(base: base, preferences: prefs)
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
        try await wait("selected item validation") { !model.isValidatingCollection }
        precondition(model.collection.count == 1)
        let hovered = model.current!.children[0]
        let revisionBeforeUpdate = model.mapRevision
        model.hoveredMapNodes = [hovered]
        model.hoveredListID = hovered.id
        precondition(model.mapRevision == revisionBeforeUpdate)
        model.setMapInteractionActive(true)
        let beforeHeldRefresh = store.savedSnapshot(for: root)!.id
        try Data(repeating: 3, count: 777).write(to: nested.appendingPathComponent("live.bin"))
        try await wait("held incremental update prepared") {
            store.savedSnapshot(for: root)?.id != beforeHeldRefresh && !model.isRefreshing && model.hasDeferredIndexedFolder
        }
        precondition(model.root?.size == 5555 && model.current?.children.count == 1 && model.mapRevision == revisionBeforeUpdate,
                     "An automatic refresh must not replace the map or rows during a held drag")
        model.setMapInteractionActive(false)
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
        model.setMapInteractionActive(true)
        let beforeSupersededRefresh = store.savedSnapshot(for: root)!.id
        try Data().write(to: nested.appendingPathComponent("stale.bin"))
        try await wait("superseded incremental update prepared") {
            store.savedSnapshot(for: root)?.id != beforeSupersededRefresh && !model.isRefreshing && model.hasDeferredIndexedFolder
        }
        model.goBack()
        model.setMapInteractionActive(false)
        precondition(model.hoveredMapNodes.isEmpty && model.hoveredListID == nil,
                     "Navigating to another folder must clear the previous hover")
        try await wait("parent navigation") { model.current?.id == nested.deletingLastPathComponent().path && !model.isOpeningFolder }
        try await Task.sleep(nanoseconds: 200_000_000)
        precondition(model.current?.id == nested.deletingLastPathComponent().path,
                     "A deferred refresh must not overwrite newer navigation")
        precondition(model.selectedID == nested.path)
        model.query = "pending search"
        model.cancelScan()
        try await wait("search after pause") { !model.isSearching && !model.isSearchResultOutdated }
        precondition(model.searchResults.isEmpty && !model.isScanning,
                     "Pause must finish the search on the retained map without restarting mapping")
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
