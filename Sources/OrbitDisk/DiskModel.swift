import SwiftUI
import AppKit
import Darwin
import OrbitCore

struct DiskVolume: Identifiable {
    let url: URL
    let name: String
    let capacity: Int64
    let available: Int64
    var id: String { url.path }
}

private struct IndexedFolder: Sendable {
    let root: DiskNode
    let folder: DiskNode
    let ancestors: [DiskNode]
}

struct MapHoverPresentation {
    let relatedRowIDs: Set<String>
    let description: String?
}

struct DiskListItem: Identifiable {
    let id: String
    let node: DiskNode
}

final class GroupedMapHoverCache {
    private struct Entry {
        let members: [DiskNode]
        let presentation: MapHoverPresentation
    }
    private var entries: [String: Entry] = [:]

    func presentation(groupID: String, members: [DiskNode], make: () -> MapHoverPresentation) -> MapHoverPresentation {
        if let entry = entries[groupID], entry.members.count == members.count,
           entry.members.withUnsafeBufferPointer({ cached in
               members.withUnsafeBufferPointer { cached.baseAddress == $0.baseAddress }
           }) {
            return entry.presentation
        }
        let presentation = make()
        // Retaining the immutable members keeps their storage alive; changed values use a different buffer.
        entries[groupID] = Entry(members: members, presentation: presentation)
        return presentation
    }

    func removeAll() { entries.removeAll(keepingCapacity: true) }
}

private struct CollectionEligibility {
    let identity: String?
    let rejection: String?
}

private struct CollectionDrag {
    let token: Data
    let rootPath: String
    let rootIdentity: String?
    let nodes: [DiskNode]
    let omitted: [DiskNode]
}

private struct CollectionValidation: Sendable {
    var nodes: [DiskNode] = []
    var rejections: [String] = []
}

private func hasCollectedAncestor(_ path: String, in paths: Set<String>, includingSelf: Bool = true) -> Bool {
    var candidate = includingSelf ? path : URL(fileURLWithPath: path).deletingLastPathComponent().path
    while true {
        if paths.contains(candidate) { return true }
        if candidate == "/" { return false }
        candidate = URL(fileURLWithPath: candidate).deletingLastPathComponent().path
    }
}

private func validateCollectionNodes(_ nodes: [DiskNode], root: URL,
                                     cancellation: ScanCancellation? = nil) -> CollectionValidation? {
    var result = CollectionValidation()
    for node in nodes {
        if cancellation?.isCancelled == true { return nil }
        guard node.isScanComplete else {
            result.rejections.append("\(node.name): still mapping. Collect this item when it finishes.")
            continue
        }
        guard node.isAccessible, !node.isSymbolicLink else {
            result.rejections.append("\(node.name): this item cannot be collected.")
            continue
        }
        if let reason = TrashPolicy.rejectionReason(for: node.url, scanRoot: root) {
            result.rejections.append("\(node.name): \(reason)")
            continue
        }
        guard let expected = node.fileIdentity, currentFileIdentity(node.url) == expected else {
            result.rejections.append("\(node.name): changed since indexing. Refresh the map before collecting it.")
            continue
        }
        result.nodes.append(node)
    }
    return result
}

private func isWithin(_ path: String, root: String) -> Bool {
    path == root || root == "/" || path.hasPrefix(root + "/")
}

private func currentFileIdentity(_ url: URL) -> String? {
    var info = stat()
    guard url.withUnsafeFileSystemRepresentation({ lstat($0!, &info) }) == 0 else { return nil }
    return "\(info.st_dev):\(info.st_ino)"
}

private func indexedFolder(_ path: String, store: DiskIndexStore, snapshot: IndexSnapshot) throws -> IndexedFolder {
    var target = path
    while try store.node(path: target, snapshot: snapshot)?.isDirectory != true {
        guard target != snapshot.rootURL.path else { throw DiskIndexError.invalid("The indexed folder is unavailable.") }
        target = URL(fileURLWithPath: target).deletingLastPathComponent().path
        if !isWithin(target, root: snapshot.rootURL.path) { target = snapshot.rootURL.path }
    }
    let folder = try store.loadFolder(path: target, snapshot: snapshot)
    var ancestors = try store.ancestors(path: target, snapshot: snapshot)
    if !ancestors.isEmpty { ancestors[ancestors.count - 1] = folder }
    return IndexedFolder(root: ancestors.first ?? folder, folder: folder, ancestors: ancestors)
}

@MainActor
final class DiskModel: ObservableObject {
    static let collectionDragType = NSPasteboard.PasteboardType("local.orbitdisk.collection-items")

    @Published var root: DiskNode? {
        didSet {
            if root?.id != oldValue?.id {
                clearCollectionDrag()
                cancelCollectionValidation()
                collectionEligibility.removeAll()
                clearHover()
            }
        }
    }
    @Published var navigation: [DiskNode] = [] {
        didSet { preparedMapHoverGroup = nil; invalidateDisplayedNodes() }
    }
    @Published private(set) var mapRevision = UUID() {
        didSet {
            mapHoverPresentationCache = nil
            groupedMapHoverCache.removeAll()
            preparedMapHoverGroup = nil
        }
    }
    @Published var selectedID: String?
    @Published var isScanning = false
    @Published private(set) var isPartialMap = false
    @Published var isRefreshing = false
    @Published var isOpeningFolder = false
    @Published var scanTitle = "Mapping your files…"
    @Published var scanPath = ""
    @Published var progressCount = 0
    @Published var progressBytes: Int64 = 0
    @Published var scanResult: ScanResult?
    @Published var indexedAt: Date?
    @Published var isSavedMap = false
    @Published var updatesPaused = false
    @Published var errorMessage: String?
    @Published var statusMessage: String?
    @Published var query = "" {
        didSet { if query != oldValue { selectedID = nil; invalidateDisplayedNodes(); clearHover(); search() } }
    }
    @Published var searchResults: [DiskNode] = [] {
        didSet { invalidateDisplayedNodes() }
    }
    @Published var searchMatchCount = 0
    @Published var isSearching = false
    @Published var collection: [DiskNode] = [] {
        didSet { collectionCoverage = CollectionCoverage(paths: collection.map(\.id)) }
    }
    private(set) var collectionCoverage = CollectionCoverage()
    @Published private(set) var draggingLabel: String?
    @Published private(set) var draggingItemCount = 0
    @Published private(set) var isValidatingCollection = false
    @Published var hoveredMapNodes: [DiskNode] = [] {
        didSet { mapHoverPresentationCache = nil; preparedMapHoverGroup = nil }
    }
    @Published var hoveredMapGroupID: String? {
        didSet {
            if hoveredMapGroupID != oldValue { mapHoverPresentationCache = nil; preparedMapHoverGroup = nil }
        }
    }
    @Published var hoveredListID: String?
    @Published var showCollection = false
    @Published var confirmTrash = false
    @Published var isRemoving = false
    @Published var previewURL: URL?
    @Published var showIssues = false
    @Published var volumes: [DiskVolume] = []
    @Published var sortByName = false {
        didSet { if sortByName != oldValue { displayedNodesCache = nil } }
    }
    var didLaunch = false
    private let store: DiskIndexStore
    private let preferences: UserDefaults
    private let scanProgressObserver: @Sendable (ScanProgress) -> Void
    private var snapshot: IndexSnapshot?
    private var scanCancellation: ScanCancellation?
    private var searchCancellation: ScanCancellation?
    private var generation = UUID()
    private var browseGeneration = UUID()
    private var requestedFolderPath: String?
    private var scanPreview: DiskScanPreview?
    private var previewTask: Task<Void, Never>?
    private var lastPreviewRevision: UInt64?
    private var lastPreviewPath: String?
    private var mapInteractionActive = false
    private var deferredPreview: (view: ScanPreviewView, scanID: UUID, browseID: UUID)?
    private var deferredScanCompletion: (() -> Void)?
    private var searchTask: Task<Void, Never>?
    private var refreshTask: Task<Void, Never>?
    private var workCompletion: Task<Void, Never>?
    private var monitor: DiskChangeMonitor?
    private var monitorTask: Task<Void, Never>?
    private var pendingDirectories = Set<String>()
    private var pendingFullRefresh = false
    private var pendingCheckpoint: DiskChangeCheckpoint?
    private var nodesByID: [String: DiskNode] = [:]
    private var folderCache: [String: IndexedFolder] = [:]
    private var folderOrder: [String] = []
    private var collectionIdentities: [String: String] = [:]
    private var collectionEligibility: [String: CollectionEligibility] = [:]
    private var collectionDrag: CollectionDrag?
    private var collectionValidationID: UUID?
    private var collectionValidationTask: Task<Void, Never>?
    private var collectionValidationCancellation: ScanCancellation?
    private var collectionValidationStatus: String?
    private var removalResultStatus: String?
    private var displayedNodesCache: [DiskNode]? {
        didSet { displayedItemsCache = nil }
    }
    private var displayedItemsCache: [DiskListItem]?
    private var displayedIDsCache: Set<String>?
    private var mapHoverPresentationCache: MapHoverPresentation?
    private var preparedMapHoverGroup: PreparedMapHoverGroup?
    private let groupedMapHoverCache = GroupedMapHoverCache()
    private(set) var childColorIndices: [String: Int] = [:]

    init(cacheDirectory: URL? = nil, preferences: UserDefaults = .standard,
         scanProgressObserver: @escaping @Sendable (ScanProgress) -> Void = { _ in }) {
        self.preferences = preferences
        self.scanProgressObserver = scanProgressObserver
        let directory = cacheDirectory ?? FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(Bundle.main.bundleIdentifier ?? "local.orbitdisk.app")
            .appendingPathComponent("Indexes", isDirectory: true)
        store = DiskIndexStore(directory: directory)
        refreshVolumes()
    }

    var current: DiskNode? { navigation.last }
    var selected: DiskNode? { selectedID.flatMap { nodesByID[$0] } }
    func node(withID id: String) -> DiskNode? { nodesByID[id] }
    var isFiltering: Bool { !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    var displayedNodes: [DiskNode] {
        if let cached = displayedNodesCache { return cached }
        let nodes = isFiltering ? searchResults : current?.children ?? []
        let displayed = sortByName ? nodes.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending } : nodes
        displayedNodesCache = displayed
        return displayed
    }
    var displayedItems: [DiskListItem] {
        if let cached = displayedItemsCache { return cached }
        let items = displayedNodes.map { DiskListItem(id: $0.id, node: $0) }
        displayedItemsCache = items
        return items
    }
    var collectionSize: Int64 { collection.reduce(0) { $0 + $1.size } }
    var canCollectSelection: Bool {
        selected.map(canCollect) ?? false
    }
    var canAcceptCollectionDrop: Bool {
        guard let drag = collectionDrag, let root, (!isScanning || isPartialMap), !isRemoving, !isValidatingCollection else { return false }
        return drag.rootPath == root.id && drag.rootIdentity == root.fileIdentity
    }
    var mapRelatedRowIDs: Set<String> { mapHoverPresentation.relatedRowIDs }
    var mapHoverDescription: String? { mapHoverPresentation.description }

    func setMapHover(nodes: [DiskNode]?, groupID: String?, preparedGroup: PreparedMapHoverGroup? = nil) {
        if let preparedGroup {
            guard preparedGroup.id == groupID, preparedGroup.rootID == current?.id,
                  preparedGroup.mapRevision == mapRevision,
                  isWithin(preparedGroup.parentID, root: preparedGroup.rootID) else { return }
        }
        if let nodes { hoveredMapNodes = nodes }
        else if !hoveredMapNodes.isEmpty { hoveredMapNodes = [] }
        if hoveredMapGroupID != groupID { hoveredMapGroupID = groupID }
        preparedMapHoverGroup = nodes?.isEmpty == false ? preparedGroup : nil
        if nodes != nil, hoveredListID != nil { hoveredListID = nil }
    }

    private var mapHoverPresentation: MapHoverPresentation {
        if let cached = mapHoverPresentationCache { return cached }
        let presentation: MapHoverPresentation
        if let group = preparedMapHoverGroup {
            presentation = makePreparedMapHoverPresentation(group)
        } else if let groupID = hoveredMapGroupID, !hoveredMapNodes.isEmpty {
            presentation = groupedMapHoverCache.presentation(groupID: groupID, members: hoveredMapNodes) {
                makeMapHoverPresentation()
            }
        } else {
            presentation = makeMapHoverPresentation()
        }
        mapHoverPresentationCache = presentation
        return presentation
    }

    private func makePreparedMapHoverPresentation(_ group: PreparedMapHoverGroup) -> MapHoverPresentation {
        let related: Set<String>
        if isFiltering {
            related = Set(searchResults.lazy.map(\.id).filter {
                group.memberIDs.contains($0)
                    || (isWithin(group.parentID, root: $0) && isWithin($0, root: group.rootID))
            })
        } else if group.parentID == group.rootID {
            related = group.memberIDs
        } else {
            let prefix = group.rootID == "/" ? "/" : group.rootID + "/"
            let child = group.parentID.dropFirst(prefix.count).split(separator: "/", maxSplits: 1).first
            related = child.map { [prefix + String($0)] } ?? []
        }
        let description = group.memberIDs.count == 1 ? group.memberIDs.first.map(relativeHoverPath)
            : "\(group.memberIDs.count) items in \(relativeHoverPath(group.parentID))"
        return MapHoverPresentation(relatedRowIDs: related,
                                    description: description)
    }

    private func makeMapHoverPresentation() -> MapHoverPresentation {
        guard let first = hoveredMapNodes.first, let current else {
            return MapHoverPresentation(relatedRowIDs: [], description: nil)
        }
        let displayedIDs: Set<String>
        if let cached = displayedIDsCache { displayedIDs = cached }
        else {
            displayedIDs = Set((isFiltering ? searchResults : current.children).map(\.id))
            displayedIDsCache = displayedIDs
        }
        let currentPath = current.id
        let firstPath = first.id
        let prefix = currentPath == "/" ? "/" : currentPath + "/"
        var commonParent = Self.parentPath(firstPath)
        var related = Set<String>()
        for (index, node) in hoveredMapNodes.enumerated() {
            let nodePath = index == 0 ? firstPath : node.id
            if isFiltering {
                var path = nodePath
                while isWithin(path, root: currentPath) {
                    if displayedIDs.contains(path) { related.insert(path) }
                    if path == currentPath { break }
                    path = Self.parentPath(path)
                }
            } else if displayedIDs.contains(nodePath) {
                related.insert(nodePath)
            } else if nodePath.hasPrefix(prefix), let first = nodePath.dropFirst(prefix.count).split(separator: "/", maxSplits: 1).first {
                let rowID = prefix + String(first)
                if displayedIDs.contains(rowID) { related.insert(rowID) }
            }
            while commonParent != "/", !isWithin(nodePath, root: commonParent) {
                commonParent = Self.parentPath(commonParent)
            }
        }
        let description = hoveredMapNodes.count == 1 ? relativeHoverPath(firstPath)
            : "\(hoveredMapNodes.count) items in \(relativeHoverPath(commonParent))"
        return MapHoverPresentation(relatedRowIDs: related, description: description)
    }

    private static func parentPath(_ path: String) -> String {
        guard path != "/", let separator = path.lastIndex(of: "/"), separator != path.startIndex else { return "/" }
        return String(path[..<separator])
    }

    private func invalidateDisplayedNodes() {
        displayedNodesCache = nil
        displayedIDsCache = nil
        mapHoverPresentationCache = nil
        groupedMapHoverCache.removeAll()
    }

    private func relativeHoverPath(_ path: String) -> String {
        if path == current?.id { return current?.name ?? path }
        if let current {
            let prefix = current.id == "/" ? "/" : current.id + "/"
            if path.hasPrefix(prefix) { return String(path.dropFirst(prefix.count)) }
        }
        return (path as NSString).abbreviatingWithTildeInPath
    }

    private func clearHover() {
        setMapHover(nodes: nil, groupID: nil)
        if hoveredListID != nil { hoveredListID = nil }
    }

    func canCollect(_ node: DiskNode) -> Bool {
        guard let root, (!isScanning || isPartialMap), !isRemoving, !isValidatingCollection, node.isScanComplete, node.isAccessible, !node.isSymbolicLink,
              node.fileIdentity != nil, !isCollected(node.id) else { return false }
        if let eligibility = collectionEligibility[node.id], eligibility.identity == node.fileIdentity {
            return eligibility.rejection == nil
        }
        let rejection = TrashPolicy.rejectionReason(for: node.url, scanRoot: root.url)
        collectionEligibility[node.id] = CollectionEligibility(identity: node.fileIdentity, rejection: rejection)
        return rejection == nil
    }

    private func isCollected(_ path: String) -> Bool {
        collectionCoverage.state(for: path) == .included
    }

    private func canBeginGroupedCollection(_ node: DiskNode, root: DiskNode, collected: Set<String>) -> Bool {
        guard node.isScanComplete, node.isAccessible, !node.isSymbolicLink, node.fileIdentity != nil,
              node.id != root.id, isWithin(node.id, root: root.id),
              !hasCollectedAncestor(node.id, in: collected) else { return false }
        if let cached = collectionEligibility[node.id], cached.identity == node.fileIdentity {
            return cached.rejection == nil
        }
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        guard !isWithin(home, root: node.id) else { return false }
        let protected = ["/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc", home + "/Library"]
        guard !protected.contains(where: { isWithin(node.id, root: $0) }) else { return false }
        let components = node.url.pathComponents
        guard !components.contains(".Trash"), !components.contains(".Trashes") else { return false }
        let directories = node.isDirectory ? components[...] : components.dropLast()
        return !directories.contains { $0.hasPrefix(".") }
    }

    func restoreLastLocation() {
        if let path = preferences.string(forKey: "lastIndexedRoot") { scan(URL(fileURLWithPath: path)) }
    }

    func chooseFolder() {
        let panel = NSOpenPanel()
        panel.title = "Choose a folder to map"
        panel.prompt = "Open Map"
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url { scan(url) }
    }

    func scan(_ url: URL, force: Bool = false) {
        guard !isRemoving else { return }
        let url = url.standardizedFileURL
        if !force, let saved = snapshot, !isScanning, isWithin(url.path, root: saved.rootURL.path) {
            if let node = nodesByID[url.path], node.isDirectory {
                open(node)
                return
            }
            if let node = try? store.node(path: url.path, snapshot: saved), node.isDirectory {
                open(node)
                return
            }
        }
        let sameRoot = root?.id == url.path
        let desiredPath = sameRoot ? requestedFolderPath ?? current?.id ?? url.path : url.path
        if !sameRoot { clearCollectionDrag() }
        stopWork()
        let scanID = UUID()
        generation = scanID
        let token = ScanCancellation()
        scanCancellation = token
        let keepMap = sameRoot && snapshot != nil
        isScanning = !keepMap
        isRefreshing = keepMap
        updatesPaused = false
        scanTitle = force ? "Mapping your files…" : "Opening saved map…"
        scanPath = url.path
        progressCount = 0; progressBytes = 0
        errorMessage = nil
        statusMessage = keepMap ? "Refreshing the full map…" : nil
        if !keepMap { query = "" }
        requestedFolderPath = desiredPath
        let store = store
        let observer = scanProgressObserver
        let preview = keepMap ? nil : DiskScanPreview()
        let worker = Task.detached(priority: .userInitiated) { [weak self] in
            guard let model = self else { throw CancellationError() }
            try FileManager.default.createDirectory(at: store.directory, withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            if !force, let saved = store.savedSnapshot(for: url) {
                do {
                    let view = try indexedFolder(url.path, store: store, snapshot: saved)
                    if let identity = view.root.fileIdentity, currentFileIdentity(url) == identity {
                        return (saved, view, true)
                    }
                }
                catch {
                    if token.isCancelled { throw CancellationError() }
                    // An unreadable cache must not prevent rebuilding the location.
                }
            }
            if token.isCancelled { throw CancellationError() }
            let checkpoint = try DiskChangeMonitor.capture(root: url)
            await MainActor.run {
                guard model.generation == scanID, !token.isCancelled else { return }
                model.scanTitle = "Mapping your files…"
                if let preview { model.beginPreview(preview, rootURL: url, path: desiredPath, scanID: scanID, token: token) }
            }
            let result = try DiskScanner().scan(url, cancellation: token, preview: preview) { progress in
                observer(progress)
                Task { @MainActor [weak model] in
                    guard let model, model.generation == scanID, !token.isCancelled else { return }
                    model.progressCount = progress.itemCount
                    model.progressBytes = progress.bytesScanned
                    model.scanPath = progress.currentPath
                }
            }
            await MainActor.run {
                guard model.generation == scanID, !token.isCancelled else { return }
                model.scanTitle = "Saving the file index…"
                model.statusMessage = keepMap ? "Saving the refreshed index…" : nil
            }
            if let preview { await model.readPreview(preview, scanID: scanID, token: token, force: true) }
            let saved = try store.save(result, eventID: checkpoint.eventID,
                                       eventIdentity: checkpoint.identity, cancellation: token)
            return (saved, try indexedFolder(desiredPath, store: store, snapshot: saved), false)
        }
        workCompletion = Task { [weak self] in
            do {
                let (saved, view, reused) = try await worker.value
                guard let self, self.generation == scanID, !token.isCancelled else { return }
                if self.mapInteractionActive {
                    self.deferredScanCompletion = { [weak self] in
                        guard let self, self.generation == scanID, !token.isCancelled else { return }
                        self.finishScan(saved, view: view, reused: reused)
                    }
                } else { self.finishScan(saved, view: view, reused: reused) }
            } catch {
                guard let self, self.generation == scanID, !token.isCancelled else { return }
                self.previewTask?.cancel(); self.previewTask = nil
                self.isScanning = false; self.isRefreshing = false
                self.errorMessage = error.localizedDescription
                self.updatesPaused = self.snapshot != nil || self.isPartialMap
                if self.isPartialMap { self.statusMessage = "Partial map kept. Restart mapping to finish." }
            }
        }
    }

    private func finishScan(_ saved: IndexSnapshot, view: IndexedFolder, reused: Bool) {
        previewTask?.cancel(); previewTask = nil
        deferredPreview = nil; deferredScanCompletion = nil
        let preservesRoute = root?.id == saved.rootURL.path
        let destination = preservesRoute ? requestedFolderPath ?? current?.id : nil
        let selection = preservesRoute ? selectedID : nil
        snapshot = saved
        isPartialMap = false
        scanPreview = nil
        lastPreviewRevision = nil; lastPreviewPath = nil
        folderCache.removeAll(); folderOrder.removeAll()
        if let destination, destination != view.folder.id {
            loadFolder(destination, keepingSelection: selection)
        } else {
            publish(view, snapshot: saved, selection: selection)
            isOpeningFolder = false
        }
        isSavedMap = reused
        isScanning = false; isRefreshing = false
        statusMessage = nil
        preferences.set(saved.rootURL.path, forKey: "lastIndexedRoot")
        beginMonitoring(saved)
        refreshVolumes()
    }

    private func beginPreview(_ preview: DiskScanPreview, rootURL: URL, path: String,
                              scanID: UUID, token: ScanCancellation) {
        if root?.id != rootURL.path {
            root = nil; navigation = []; selectedID = nil
            nodesByID.removeAll(); childColorIndices.removeAll()
        }
        snapshot = nil; scanResult = nil; indexedAt = nil; isSavedMap = false
        folderCache.removeAll(); folderOrder.removeAll()
        isPartialMap = true
        scanPreview = preview
        requestedFolderPath = path
        lastPreviewRevision = nil; lastPreviewPath = nil
        previewTask = Task { [weak self] in
            while !Task.isCancelled, !token.isCancelled {
                guard let self, self.generation == scanID, self.scanPreview === preview else { return }
                await self.readPreview(preview, scanID: scanID, token: token)
                let delay: UInt64 = self.lastPreviewRevision == nil ? 30_000_000 : 350_000_000
                do { try await Task.sleep(nanoseconds: delay) } catch { return }
            }
        }
    }

    private func readPreview(_ preview: DiskScanPreview, scanID: UUID, token: ScanCancellation, force: Bool = false) async {
        guard generation == scanID, !token.isCancelled, scanPreview === preview else { return }
        let path = requestedFolderPath ?? current?.id
        let browseID = browseGeneration
        let lastRevision = lastPreviewRevision
        let lastPath = lastPreviewPath
        let view = await Task.detached(priority: .userInitiated) { () -> ScanPreviewView? in
            if !force, lastRevision == preview.revision, lastPath == path { return nil }
            return preview.view(path: path) ?? (force ? preview.view() : nil)
        }.value
        guard !Task.isCancelled, !token.isCancelled, generation == scanID, browseGeneration == browseID,
              scanPreview === preview, let view else { return }
        receivePreview(view, scanID: scanID, browseID: browseID)
    }

    private func receivePreview(_ view: ScanPreviewView, scanID: UUID, browseID: UUID) {
        guard generation == scanID, browseGeneration == browseID, isPartialMap else { return }
        if lastPreviewPath == view.folder.id, let revision = lastPreviewRevision {
            if revision > view.revision || (revision == view.revision && !isOpeningFolder) { return }
        }
        lastPreviewRevision = view.revision; lastPreviewPath = view.folder.id
        if mapInteractionActive {
            deferredPreview = (view, scanID, browseID)
            return
        }
        publishPreview(view)
    }

    private func publishPreview(_ view: ScanPreviewView) {
        installFolder(IndexedFolder(root: view.root, folder: view.folder, ancestors: view.ancestors),
                      selection: selectedID, retainingPendingSelection: true)
        isOpeningFolder = false
    }

    func setMapInteractionActive(_ active: Bool) {
        mapInteractionActive = active
        guard !active else { return }
        if let completion = deferredScanCompletion {
            deferredScanCompletion = nil
            completion()
        } else if let pending = deferredPreview {
            deferredPreview = nil
            guard generation == pending.scanID, browseGeneration == pending.browseID, isPartialMap else { return }
            publishPreview(pending.view)
        }
    }

    private func stopWork() {
        previewTask?.cancel(); previewTask = nil
        deferredPreview = nil; deferredScanCompletion = nil
        mapInteractionActive = false
        cancelCollectionValidation()
        removalResultStatus = nil
        scanCancellation?.cancel()
        searchCancellation?.cancel(); searchTask?.cancel(); searchTask = nil
        isSearching = false
        refreshTask?.cancel(); refreshTask = nil
        monitorTask?.cancel(); monitorTask = nil
        monitor?.stop(); monitor = nil
        pendingDirectories.removeAll(); pendingFullRefresh = false; pendingCheckpoint = nil
        browseGeneration = UUID()
        requestedFolderPath = nil
        isOpeningFolder = false
    }

    func cancelScan() {
        stopWork()
        generation = UUID()
        isScanning = false; isRefreshing = false
        updatesPaused = snapshot != nil || isPartialMap
        statusMessage = isPartialMap ? "Partial map kept. Restart mapping to finish."
            : snapshot == nil ? "Scan cancelled." : "Updates paused. Refresh to resume."
    }

    func rescan() { if let url = root?.url { scan(url, force: true) } }

    private func beginMonitoring(_ saved: IndexSnapshot) {
        monitorTask?.cancel(); monitorTask = nil
        monitor?.stop(); monitor = nil
        do {
            let preferences = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Preferences/\(Bundle.main.bundleIdentifier ?? "local.orbitdisk.app").plist")
            let watcher = try DiskChangeMonitor(root: saved.rootURL, excluding: [store.directory, preferences])
            monitor = watcher
            let scanID = generation
            monitorTask = Task { [weak self] in
                do {
                    try await watcher.start(sinceEventID: saved.eventID, eventIdentity: saved.eventIdentity) { [weak self, weak watcher] batch in
                        guard let self, let watcher, self.monitor === watcher,
                              self.generation == scanID, !self.updatesPaused else { return }
                        self.pendingDirectories.formUnion(batch.directoryPaths)
                        self.pendingFullRefresh = self.pendingFullRefresh || batch.fullRefreshRequired
                        self.pendingCheckpoint = batch.checkpoint
                        self.scheduleRefresh()
                    }
                    guard let self, self.monitor === watcher, self.generation == scanID, !Task.isCancelled else { return }
                    self.monitorTask = nil
                } catch is CancellationError {
                    return
                } catch {
                    guard let self, self.monitor === watcher, self.generation == scanID, !Task.isCancelled else { return }
                    self.monitorTask = nil
                    self.updatesPaused = true
                    self.statusMessage = "Saved map available. Automatic updates unavailable; use Refresh."
                }
            }
        } catch {
            updatesPaused = true
            statusMessage = "Saved map available. Automatic updates unavailable; use Refresh."
        }
    }

    private func scheduleRefresh() {
        guard !isRefreshing, !isScanning, !isRemoving, !updatesPaused,
              refreshTask == nil, pendingFullRefresh || !pendingDirectories.isEmpty else { return }
        refreshTask = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: 1_500_000_000) } catch { return }
            self?.refreshTask = nil
            self?.refreshChangedFolders()
        }
    }

    private func refreshChangedFolders() {
        guard let saved = snapshot, !isRefreshing, !isScanning, !isRemoving else { return }
        if pendingFullRefresh { scan(saved.rootURL, force: true); return }
        let paths = pendingDirectories
        guard !paths.isEmpty else { return }
        let checkpoint = pendingCheckpoint ?? DiskChangeCheckpoint(rootPath: saved.rootURL.path,
            identity: saved.eventIdentity, eventID: saved.eventID)
        pendingDirectories.removeAll(); pendingCheckpoint = nil
        let token = ScanCancellation()
        scanCancellation = token
        let scanID = generation
        let store = store
        isRefreshing = true
        statusMessage = removalResultStatus.map { $0 + " Updating the map…" } ?? "Updating changed folders…"
        let worker = Task.detached(priority: .utility) { () throws -> IndexSnapshot in
            let minimal = try refreshDirectories(paths, store: store, snapshot: saved)
            var replacements: [IndexReplacement] = []
            for path in minimal {
                if token.isCancelled { throw CancellationError() }
                do {
                    let result = try DiskScanner().scan(URL(fileURLWithPath: path), cancellation: token)
                    replacements.append(IndexReplacement(path: path, result: result))
                } catch ScanError.filesystem(_, let code) where code == ENOENT || code == ENOTDIR {
                    replacements.append(IndexReplacement(path: path, result: nil))
                }
            }
            return try store.update(snapshot: saved, replacements: replacements, eventID: checkpoint.eventID,
                                    eventIdentity: checkpoint.identity, cancellation: token)
        }
        workCompletion = Task { [weak self] in
            do {
                let updated = try await worker.value
                guard let self, self.generation == scanID, !token.isCancelled else { return }
                self.snapshot = updated
                self.folderCache.removeAll(); self.folderOrder.removeAll()
                self.isRefreshing = false; self.isSavedMap = false
                self.statusMessage = self.removalResultStatus ?? "Updated changed folders."
                self.removalResultStatus = nil
                self.loadFolder(self.requestedFolderPath ?? self.current?.id ?? updated.rootURL.path,
                                keepingSelection: self.selectedID)
                self.scheduleRefresh()
            } catch {
                guard let self, self.generation == scanID, !token.isCancelled else { return }
                self.isRefreshing = false
                self.pendingDirectories.formUnion(paths)
                self.updatesPaused = true
                self.monitor?.stop()
                let removalStatus = self.removalResultStatus.map { $0 + " " } ?? ""
                let priorFailure = self.removalResultStatus == nil ? nil : self.errorMessage
                self.statusMessage = removalStatus + "Map kept. Update failed; use Refresh to retry."
                self.removalResultStatus = nil
                self.errorMessage = priorFailure.map { $0 + "\nMap update failed: " + error.localizedDescription }
                    ?? error.localizedDescription
            }
        }
    }

    private func installFolder(_ view: IndexedFolder, selection: String? = nil, retainingPendingSelection: Bool = false) {
        let sameFolder = current?.id == view.folder.id
        let preservesHover = current?.id == view.folder.id && root?.id == view.root.id
            && root?.fileIdentity == view.root.fileIdentity
        if !preservesHover { clearHover() }
        mapRevision = UUID()
        root = view.root
        navigation = view.ancestors
        requestedFolderPath = view.folder.id
        if !sameFolder { childColorIndices.removeAll(keepingCapacity: true) }
        var nextColor = (childColorIndices.values.max() ?? -1) + 1
        for child in view.folder.children where childColorIndices[child.id] == nil {
            childColorIndices[child.id] = nextColor
            nextColor += 1
        }
        if !isFiltering { displayedIDsCache = Set(view.folder.children.map(\.id)) }
        nodesByID.removeAll(keepingCapacity: true)
        var pending = [view.folder] + view.ancestors.dropLast() + searchResults
        while let node = pending.popLast() {
            nodesByID[node.id] = node
            pending.append(contentsOf: node.children)
        }
        if preservesHover {
            let refreshedHover = hoveredMapNodes.compactMap { previous -> DiskNode? in
                guard let node = nodesByID[previous.id], node.fileIdentity == previous.fileIdentity else { return nil }
                return node
            }
            if refreshedHover.count == hoveredMapNodes.count {
                hoveredMapNodes = refreshedHover
            } else {
                hoveredMapNodes = []
                hoveredMapGroupID = nil
            }
            if let id = hoveredListID, nodesByID[id] == nil { hoveredListID = nil }
        }
        selectedID = selection.flatMap { retainingPendingSelection || nodesByID[$0] != nil ? $0 : nil }
    }

    private func publish(_ view: IndexedFolder, snapshot saved: IndexSnapshot, selection: String? = nil) {
        installFolder(view, selection: selection)
        indexedAt = saved.scannedAt
        scanResult = ScanResult(root: view.root, elapsed: saved.elapsed, skippedCount: saved.skippedCount, issues: saved.issues)
        folderCache[view.folder.id] = view
        folderOrder.removeAll { $0 == view.folder.id }; folderOrder.append(view.folder.id)
        while folderOrder.count > 24 { folderCache.removeValue(forKey: folderOrder.removeFirst()) }
        if isFiltering { search() }
    }

    private func loadFolder(_ path: String, keepingSelection selection: String? = nil) {
        if isPartialMap, let preview = scanPreview {
            if path != current?.id { clearHover() }
            requestedFolderPath = path
            browseGeneration = UUID()
            let browseID = browseGeneration
            let scanID = generation
            selectedID = selection
            isOpeningFolder = true
            Task { [weak self] in
                let view = await Task.detached(priority: .userInitiated) { preview.view(path: path) }.value
                guard let self, self.generation == scanID, self.browseGeneration == browseID,
                      self.scanPreview === preview, self.isPartialMap else { return }
                if let view { self.receivePreview(view, scanID: scanID, browseID: browseID) }
                else { self.isOpeningFolder = false }
            }
            return
        }
        guard let saved = snapshot else { return }
        if path != current?.id { clearHover() }
        requestedFolderPath = path
        browseGeneration = UUID()
        let browseID = browseGeneration
        if let cached = folderCache[path] {
            publish(cached, snapshot: saved, selection: selection)
            isOpeningFolder = false
            return
        }
        isOpeningFolder = true
        let store = store
        let scanID = generation
        let worker = Task.detached(priority: .userInitiated) { try indexedFolder(path, store: store, snapshot: saved) }
        Task { [weak self] in
            do {
                let view = try await worker.value
                guard let self, self.generation == scanID, self.browseGeneration == browseID else { return }
                guard self.snapshot?.id == saved.id else { self.loadFolder(path, keepingSelection: selection); return }
                self.publish(view, snapshot: saved, selection: selection)
                self.isOpeningFolder = false
            } catch {
                guard let self, self.generation == scanID, self.browseGeneration == browseID else { return }
                self.isOpeningFolder = false
                if self.adoptLatestSnapshot() || self.snapshot?.id != saved.id { self.loadFolder(path, keepingSelection: selection) }
                else { self.errorMessage = error.localizedDescription }
            }
        }
    }

    private func adoptLatestSnapshot() -> Bool {
        guard let saved = snapshot, let latest = store.savedSnapshot(for: saved.rootURL), latest.id != saved.id else { return false }
        snapshot = latest
        folderCache.removeAll(); folderOrder.removeAll()
        return true
    }

    func open(_ node: DiskNode) {
        guard node.isDirectory, !node.isSymbolicLink else { selectedID = node.id; return }
        query = ""
        loadFolder(node.id)
    }
    func goBack() {
        guard navigation.count > 1, let departed = current else { return }
        query = ""
        loadFolder(navigation[navigation.count - 2].id, keepingSelection: departed.id)
    }
    func openSelection() { if let selected { open(selected) } }
    func revealSelection() { if let selected { NSWorkspace.shared.activateFileViewerSelecting([selected.url]) } }
    func previewSelection() { if let selected, !selected.isDirectory { previewURL = selected.url } }

    func collectSelection() {
        if let selected { collect([selected]) }
    }

    @discardableResult
    func collect(_ nodes: [DiskNode]) -> Bool {
        guard let root, (!isScanning || isPartialMap), !isRemoving, !isValidatingCollection else { return false }
        guard let identity = root.fileIdentity, currentFileIdentity(root.url) == identity else {
            statusMessage = "The mapped folder changed. Refresh the map before collecting items."
            return false
        }
        let prepared = prepareCollection(nodes)
        guard let validated = validateCollectionNodes(prepared.nodes, root: root.url) else { return false }
        return applyCollection(validated, includedCount: prepared.includedCount)
    }

    private func prepareCollection(_ nodes: [DiskNode]) -> (nodes: [DiskNode], includedCount: Int) {
        let collected = Set(collection.map(\.id))
        var candidates: [DiskNode] = []
        var includedCount = 0
        var seen = Set<String>()
        for node in nodes {
            guard seen.insert(node.id).inserted, !hasCollectedAncestor(node.id, in: collected) else {
                includedCount += 1
                continue
            }
            candidates.append(node)
        }
        return (candidates, includedCount)
    }

    private func applyCollection(_ validated: CollectionValidation, includedCount: Int, publishStatus: Bool = true) -> Bool {
        let candidates = Set(validated.nodes.map(\.id))
        let collected = Set(collection.map(\.id))
        var includedCount = includedCount
        var added: [DiskNode] = []
        for node in validated.nodes {
            if hasCollectedAncestor(node.id, in: collected)
                || hasCollectedAncestor(node.id, in: candidates, includingSelf: false) {
                includedCount += 1
                continue
            }
            added.append(node)
        }
        if !added.isEmpty {
            let addedIDs = Set(added.map(\.id))
            let retained = collection.filter { !hasCollectedAncestor($0.id, in: addedIDs) }
            collection = retained + added
            let retainedIDs = Set(retained.map(\.id))
            collectionIdentities = collectionIdentities.filter { retainedIDs.contains($0.key) }
            for node in added { collectionIdentities[node.id] = node.fileIdentity }
        }
        var messages: [String] = []
        if added.count == 1 { messages.append("Added \(added[0].name) to collection.") }
        else if !added.isEmpty { messages.append("Added \(added.count) items to collection.") }
        if includedCount > 0 { messages.append("\(includedCount) \(includedCount == 1 ? "item already included" : "items already included").") }
        if !validated.rejections.isEmpty {
            messages.append("Skipped \(validated.rejections.count) \(validated.rejections.count == 1 ? "item" : "items").")
            errorMessage = validated.rejections.prefix(5).joined(separator: "\n")
                + (validated.rejections.count > 5 ? "\n…and \(validated.rejections.count - 5) more." : "")
        }
        if publishStatus, !messages.isEmpty { statusMessage = messages.joined(separator: " ") }
        return !added.isEmpty
    }

    func beginCollectionDrag(_ nodes: [DiskNode], label: String) -> NSPasteboardItem? {
        clearCollectionDrag()
        guard let root, (!isScanning || isPartialMap), !isRemoving, !isValidatingCollection else { return nil }
        let collected = Set(collection.map(\.id))
        let eligible = nodes.count == 1 ? nodes.filter(canCollect)
            : nodes.filter { canBeginGroupedCollection($0, root: root, collected: collected) }
        guard !eligible.isEmpty else {
            if !nodes.isEmpty && nodes.allSatisfy({ !$0.isScanComplete }) {
                statusMessage = "Still mapping. Collect this item when it finishes."
            } else {
                statusMessage = nodes.allSatisfy { hasCollectedAncestor($0.id, in: collected) }
                    ? "These items are already included in the collection."
                    : "These items cannot be collected. Refresh the map if they have changed."
            }
            return nil
        }
        let token = Data(UUID().uuidString.utf8)
        let eligibleIDs = Set(eligible.map(\.id))
        collectionDrag = CollectionDrag(token: token, rootPath: root.id, rootIdentity: root.fileIdentity,
                                        nodes: eligible, omitted: nodes.filter { !eligibleIDs.contains($0.id) })
        draggingLabel = label
        var seen = Set<String>()
        draggingItemCount = eligible.filter { seen.insert($0.id).inserted }.count
        let item = NSPasteboardItem()
        item.setData(token, forType: Self.collectionDragType)
        return item
    }

    func endCollectionDrag() {
        draggingLabel = nil
        draggingItemCount = 0
    }

    private func clearCollectionDrag() {
        collectionDrag = nil
        endCollectionDrag()
    }

    @discardableResult
    func collectDrag(data: Data) -> Bool {
        guard let drag = collectionDrag, data == drag.token else { return false }
        let canAccept = canAcceptCollectionDrop
        clearCollectionDrag()
        guard canAccept, let root, let expected = drag.rootIdentity,
              currentFileIdentity(root.url) == expected else {
            statusMessage = "The map changed. Drag the item again from the current map."
            return false
        }
        let nodes = drag.nodes + drag.omitted
        guard nodes.count > 64 else { return collect(nodes) }
        let prepared = prepareCollection(nodes)
        let validationID = UUID()
        let scanID = generation
        let token = ScanCancellation()
        let progress = "Checking \(prepared.nodes.count) items for collection…"
        collectionValidationID = validationID
        collectionValidationCancellation = token
        collectionValidationStatus = progress
        isValidatingCollection = true
        statusMessage = progress
        let worker = Task.detached(priority: .userInitiated) {
            validateCollectionNodes(prepared.nodes, root: root.url, cancellation: token)
        }
        collectionValidationTask = Task { [weak self] in
            let validated = await worker.value
            guard let self, self.collectionValidationID == validationID else { return }
            self.collectionValidationID = nil
            self.collectionValidationTask = nil
            self.collectionValidationCancellation = nil
            self.collectionValidationStatus = nil
            self.isValidatingCollection = false
            guard let validated, !Task.isCancelled, !token.isCancelled,
                  self.generation == scanID, self.root?.id == root.id, self.root?.fileIdentity == expected,
                  currentFileIdentity(root.url) == expected, (!self.isScanning || self.isPartialMap), !self.isRemoving else {
                if self.statusMessage == progress { self.statusMessage = "The map changed. Drag the items again from the current map." }
                return
            }
            _ = self.applyCollection(validated, includedCount: prepared.includedCount,
                                     publishStatus: self.statusMessage == progress)
        }
        return true
    }

    private func cancelCollectionValidation() {
        collectionValidationCancellation?.cancel()
        collectionValidationTask?.cancel()
        collectionValidationID = nil
        collectionValidationTask = nil
        collectionValidationCancellation = nil
        isValidatingCollection = false
        if let progress = collectionValidationStatus, statusMessage == progress { statusMessage = nil }
        collectionValidationStatus = nil
    }

    func moveCollectionToTrash() { removeCollection(permanently: false) }

    func deleteCollectionPermanently() { removeCollection(permanently: true) }

    private func removeCollection(permanently: Bool) {
        guard let root, snapshot != nil, !isPartialMap, !collection.isEmpty, !isRemoving, !isScanning, !isValidatingCollection else { return }
        clearCollectionDrag()
        scanCancellation?.cancel(); refreshTask?.cancel(); refreshTask = nil
        monitorTask?.cancel(); monitorTask = nil
        monitor?.stop(); monitor = nil
        isRefreshing = false; isRemoving = true
        errorMessage = nil
        statusMessage = permanently ? "Deleting collected items permanently…" : "Moving collected items to Trash…"
        let items = collection
        let identities = collectionIdentities
        let rootURL = root.url
        let rootIdentity = root.fileIdentity
        let completion = workCompletion
        Task {
            await completion?.value
            let outcome = await Task.detached(priority: .userInitiated) {
                var removed = Set<String>()
                var failures: [String] = []
                var changedParents = Set<String>()
                var rootChanged = false
                for item in items {
                    guard let rootIdentity, currentFileIdentity(rootURL) == rootIdentity else {
                        failures.append("The mapped folder changed. Refresh the map before removing any more items.")
                        rootChanged = true
                        break
                    }
                    guard let expected = identities[item.id], currentFileIdentity(item.url) == expected else {
                        failures.append("\(item.name): the item changed. Remove it from the collection and add it again.")
                        if isWithin(item.id, root: rootURL.path) { changedParents.insert(item.url.deletingLastPathComponent().path) }
                        continue
                    }
                    if let reason = TrashPolicy.rejectionReason(for: item.url, scanRoot: rootURL) {
                        failures.append("\(item.name): \(reason)"); continue
                    }
                    changedParents.insert(item.url.deletingLastPathComponent().path)
                    do {
                        if permanently { try FileManager.default.removeItem(at: item.url) }
                        else { try FileManager.default.trashItem(at: item.url, resultingItemURL: nil) }
                        removed.insert(item.id)
                    } catch { failures.append("\(item.name): \(error.localizedDescription)") }
                }
                return (removed, failures, changedParents, rootChanged)
            }.value
            isRemoving = false
            collection.removeAll { outcome.0.contains($0.id) }
            collectionIdentities = collectionIdentities.filter { !outcome.0.contains($0.key) }
            showCollection = !collection.isEmpty
            if !outcome.1.isEmpty { errorMessage = outcome.1.joined(separator: "\n") }
            let count = outcome.0.count
            let noun = count == 1 ? "item" : "items"
            let result = permanently ? "Deleted \(count) \(noun) permanently." : "Moved \(count) \(noun) to Trash."
            let remaining = collection.count
            statusMessage = result + (remaining == 0 ? "" : " \(remaining) \(remaining == 1 ? "item remains" : "items remain") in the collection.")
            if outcome.3 {
                updatesPaused = true
                removalResultStatus = nil
            } else if let saved = store.savedSnapshot(for: rootURL) {
                snapshot = saved
                folderCache.removeAll(); folderOrder.removeAll()
                updatesPaused = false
                pendingDirectories.formUnion(outcome.2)
                removalResultStatus = pendingDirectories.isEmpty ? nil : statusMessage
                beginMonitoring(saved)
                scheduleRefresh()
            }
            refreshVolumes()
        }
    }

    private func search() {
        searchTask?.cancel(); searchCancellation?.cancel()
        let originalQuery = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !isPartialMap, !originalQuery.isEmpty, let saved = snapshot else {
            searchResults = []; searchMatchCount = 0; isSearching = false
            return
        }
        isSearching = true
        let token = ScanCancellation()
        searchCancellation = token
        let store = store
        searchTask = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: 100_000_000) } catch { return }
            do {
                let worker = Task.detached(priority: .userInitiated) {
                    try store.search(query: originalQuery, snapshot: saved, cancellation: token)
                }
                let result = try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { token.cancel() })
                guard let self, !Task.isCancelled, !token.isCancelled,
                      self.query.trimmingCharacters(in: .whitespacesAndNewlines) == originalQuery else { return }
                guard self.snapshot?.id == saved.id else { self.search(); return }
                self.searchResults = result.nodes
                self.searchMatchCount = result.totalCount
                for node in result.nodes { self.nodesByID[node.id] = node }
                self.isSearching = false
            } catch {
                guard let self, !Task.isCancelled, !token.isCancelled else { return }
                self.isSearching = false
                if self.adoptLatestSnapshot() || self.snapshot?.id != saved.id { self.search() }
                else { self.statusMessage = "Search could not read the index. Try Refresh." }
            }
        }
    }

    func refreshVolumes() {
        let keys: Set<URLResourceKey> = [.volumeNameKey, .volumeTotalCapacityKey, .volumeAvailableCapacityKey]
        let mounted = FileManager.default.mountedVolumeURLs(includingResourceValuesForKeys: Array(keys), options: .skipHiddenVolumes) ?? []
        let urls = [URL(fileURLWithPath: "/")] + mounted.filter { $0.path.hasPrefix("/Volumes/") }
        volumes = urls.compactMap { url in
            guard let info = try? url.resourceValues(forKeys: keys) else { return nil }
            return DiskVolume(url: url, name: info.volumeName ?? url.lastPathComponent,
                              capacity: Int64(info.volumeTotalCapacity ?? 0), available: Int64(info.volumeAvailableCapacity ?? 0))
        }
    }
}

func formatBytes(_ size: Int64) -> String {
    ByteCountFormatter.string(fromByteCount: size, countStyle: .file)
}
