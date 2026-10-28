import SwiftUI
import AppKit
import Darwin
import OrbitCore

struct DiskVolume: Identifiable, Sendable {
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

private struct PreparedFolder: Sendable {
    let view: IndexedFolder
    let items: [DiskListItem]
    let childIDs: [String]
    let displayedIDs: Set<String>
    let rowIndices: [String: Int]?
    let nodesByID: [String: DiskNode]
    let initialColors: [String: Int]
    let preservedColors: [String: Int]?
    let colorRevision: UUID?
    let colorMappingExtended: Bool

    var root: DiskNode { view.root }
    var folder: DiskNode { view.folder }
    var ancestors: [DiskNode] { view.ancestors }
}

private struct ColorPreparationState: Sendable {
    let path: String
    let revision: UUID
    let colors: [String: Int]
}

private struct RememberedFolder {
    let folder: PreparedFolder
    let removalEpoch: UInt64
    var selection: String?
}

private func collectionTotal(_ items: [DiskNode]) -> Int64 {
    items.reduce(0) { total, item in
        let sum = total.addingReportingOverflow(item.size)
        return sum.overflow ? (item.size < 0 ? Int64.min : Int64.max) : sum.partialValue
    }
}

private struct CollectionSnapshot: Sendable {
    let items: [DiskNode]
    let identities: [String: String]
    let coverage: CollectionCoverage
    let revision: UUID
    let size: Int64
    let pathIDs: Set<String>
    init(items: [DiskNode], identities: [String: String], coverage: CollectionCoverage, revision: UUID,
         size: Int64? = nil, pathIDs: Set<String>? = nil) {
        self.items = items
        self.identities = identities
        self.coverage = coverage
        self.revision = revision
        self.size = size ?? collectionTotal(items)
        self.pathIDs = pathIDs ?? Set(items.map(\.id))
    }
    static var empty: CollectionSnapshot { CollectionSnapshot(items: [], identities: [:], coverage: CollectionCoverage(), revision: UUID()) }
    func renewed() -> CollectionSnapshot { CollectionSnapshot(items: items, identities: identities, coverage: coverage, revision: UUID(), size: size, pathIDs: pathIDs) }
}

private final class CollectionEditQueue {
    let rootIdentity: String
    let rootBytes: Data
    var pending = Set<String>()
    var task: Task<Void, Never>?
    init(rootIdentity: String, rootBytes: Data) { self.rootIdentity = rootIdentity; self.rootBytes = rootBytes }
}

private final class SessionProgress: @unchecked Sendable {
    private let lock = NSLock()
    private var value: ScanProgress?
    func set(_ next: ScanProgress) { lock.lock(); value = next; lock.unlock() }
    func read() -> ScanProgress? { lock.lock(); defer { lock.unlock() }; return value }
}
private struct RemovalKey: Hashable {
    let path: Data
    let identity: String
    init(_ removal: ScanRemoval) { path = Data(removal.path.utf8); identity = removal.identity }
}
private func mergingRemovals(_ prior: [ScanRemoval], _ incoming: [ScanRemoval]) -> (items: [ScanRemoval], overflow: Bool) {
    var keys = Set(prior.map(RemovalKey.init))
    var merged = prior
    var overflow = prior.count > 10_000
    for removal in incoming where keys.insert(RemovalKey(removal)).inserted {
        if merged.count < 10_000 { merged.append(removal) } else { overflow = true }
    }
    return (merged, overflow)
}
private func sameLiveRecovery(_ a: RemovalRecovery, _ b: RemovalRecovery) -> Bool {
    guard a.hasLiveOperation, b.hasLiveOperation, a.cursor == b.cursor,
          a.overlayOverflow == b.overlayOverflow, a.requiresFullRefresh == b.requiresFullRefresh,
          a.removals.count == b.removals.count,
          Set(a.affectedParents.map { Data($0.utf8) }) == Set(b.affectedParents.map { Data($0.utf8) }) else { return false }
    return zip(a.removals, b.removals).allSatisfy {
        Data($0.path.utf8) == Data($1.path.utf8) && $0.identity == $1.identity
    }
}
private final class CollectionChunkBuffer {
    var chunks: [[DiskNode]] = []
    var processed = 0
    var target = 128
    var revalidationIndex = 0
    var revalidationRevision: UUID?
    var revalidationRemovalEpoch: UInt64?
    var readyToCommit = false
}

private final class CollectionValidationJob {
    let id = UUID()
    let rootPath: String
    let rootBytes: Data
    let rootIdentity: String
    let initialStatusFeedbackRevision: UInt64
    let token = ScanCancellation()
    var requests: [(nodes: [DiskNode], groupContext: GroupContext?, offset: Int, buffer: CollectionChunkBuffer?)?] = []
    var nextRequest = 0
    var didChunk = false
    var addedCount = 0
    var addedName: String?
    var includedCount = 0
    var rejectionCount = 0
    var rejectionExamples: [String] = []
    var task: Task<Void, Never>?
    init(root: URL, identity: String, initialStatusFeedbackRevision: UInt64) {
        rootPath = root.path; rootBytes = Data(root.path.utf8); rootIdentity = identity
        self.initialStatusFeedbackRevision = initialStatusFeedbackRevision
    }
}

private final class MappingSession {
    let id = UUID()
    let url: URL
    let identity: String?
    let token: ScanCancellation
    var preview: DiskScanPreview?
    let progress = SessionProgress()
    var lastRoute: String?
    var rememberedFolder: RememberedFolder?
    var savedSnapshot: IndexSnapshot?
    var collectionState = CollectionSnapshot.empty
    var collectionValidation: CollectionValidationJob?
    var collectionStatus: String?
    var collectionError: String?
    var presentedCollectionError: String?
    var collectionStatusRevision: UInt64 = 0
    var collectionErrorRevision: UInt64 = 0
    func invalidateCollectionStatus() { collectionStatusRevision &+= 1; collectionStatus = nil }
    func invalidateCollectionError() { collectionErrorRevision &+= 1 }
    func enqueueCollectionError(_ message: String) {
        if let pending = collectionError { collectionError = pending + "\n\n" + message }
        else { collectionError = message }
    }
    var isRemoving = false
    var pendingDirectories = Set<String>()
    var pendingCheckpoint: DiskChangeCheckpoint?
    var pendingFullRefresh = false
    var removalParents = Set<String>()
    var removalVersions: [String: UInt64] = [:]
    var removals: [ScanRemoval] = []
    var removalEpoch: UInt64 = 0
    var durableRecoveryCursor: RemovalCursor?
    var durableRecoveryTarget: RemovalCursor?
    func includeRemovalTarget(_ cursor: RemovalCursor) {
        if let current = durableRecoveryTarget, current.identity == cursor.identity, current.sequence > cursor.sequence { return }
        durableRecoveryTarget = cursor
    }
    var durableRecoveryPending = false
    var durableRecoveryConflict = false
    var durablePreviewBlocked = false
    var removalStatus: String? { didSet { if removalStatus != oldValue { invalidateCollectionStatus() } } }
    var removalError: String? { didSet { if removalError != oldValue { invalidateCollectionError() } } }
    var finished = false
    var scanFailed = false
    var refreshWorker: Task<IndexSnapshot, Error>?
    var refreshWorkerID: UUID?
    var worker: Task<(IndexSnapshot, PreparedFolder, Bool), Error>?
    init(url: URL, identity: String?, token: ScanCancellation, preview: DiskScanPreview?) {
        self.url = url; self.identity = identity; self.token = token; self.preview = preview
    }
}

private func containsBytes(_ path: String, root: String) -> Bool {
    let pathBytes = Array(path.utf8), rootBytes = Array(root.utf8)
    if pathBytes == rootBytes { return true }
    if rootBytes == [47] { return pathBytes.first == 47 }
    return pathBytes.count > rootBytes.count && pathBytes.starts(with: rootBytes) && pathBytes[rootBytes.count] == 47
}

// These syscalls can wait on a mounted or privacy-protected directory; never run on the main actor.
private func isReachableWithinSession(root: URL, target: URL, expectedIdentity: String) -> Bool {
    (try? MappingCoverage.canReach(root: root, target: target, expectedRootIdentity: expectedIdentity)) == true
}

private struct PreparedPreview: Sendable {
    let preview: ScanPreviewView
    let folder: PreparedFolder
    let replayGeneration: UInt64?
}

struct MapHoverPresentation {
    let relatedRowIDs: Set<String>
    let description: String?
}

struct DiskListItem: Identifiable, Sendable {
    let id: String
    let node: DiskNode
}

private struct FolderSizeOrder {
    let nodes: [DiskNode]
    let items: [DiskListItem]
    let indices: [String: Int]?
    let ids: Set<String>
}

private func prepareFolder(_ view: IndexedFolder, colors previous: ColorPreparationState? = nil,
                           cancellation: ScanCancellation? = nil, cancellable: Bool = true) throws -> PreparedFolder {
    func checkCancellation() throws {
        if cancellable && (Task<Never, Never>.isCancelled || cancellation?.isCancelled == true) {
            throw CancellationError()
        }
    }
    try checkCancellation()
    let children = view.folder.children
    var items: [DiskListItem] = []
    items.reserveCapacity(children.count)
    var childIDs: [String] = []
    childIDs.reserveCapacity(children.count)
    var displayedIDs = Set<String>()
    displayedIDs.reserveCapacity(children.count)
    var colors: [String: Int] = [:]
    colors.reserveCapacity(children.count)
    var rowIndices: [String: Int]? = children.count >= 1_000 ? [:] : nil
    rowIndices?.reserveCapacity(children.count)
    for (index, child) in children.enumerated() {
        if index & 255 == 0 { try checkCancellation() }
        let id = child.id
        items.append(DiskListItem(id: id, node: child))
        childIDs.append(id)
        displayedIDs.insert(id)
        if rowIndices != nil && rowIndices?[id] == nil { rowIndices?[id] = index }
        if colors[id] == nil { colors[id] = colors.count }
    }
    var preserved: [String: Int]?
    if let previous, previous.path == view.folder.id {
        var updated = previous.colors
        var nextColor = (updated.values.max() ?? -1) + 1
        for (index, item) in items.enumerated() {
            if index & 255 == 0 { try checkCancellation() }
            if updated[item.id] == nil {
                updated[item.id] = nextColor
                nextColor += 1
            }
        }
        preserved = updated
    }
    var nodes: [String: DiskNode] = [:]
    nodes.reserveCapacity(children.count + view.ancestors.count)
    var pending = Array(view.ancestors.dropLast())
    var visited = 0
    while let node = pending.popLast() {
        visited &+= 1
        if visited & 255 == 0 { try checkCancellation() }
        nodes[node.id] = node
        pending.append(contentsOf: node.children)
    }
    nodes[view.folder.id] = view.folder
    for (index, item) in items.reversed().enumerated() {
        if index & 255 == 0 { try checkCancellation() }
        nodes[item.id] = item.node
        pending.append(contentsOf: item.node.children)
        while let node = pending.popLast() {
            visited &+= 1
            if visited & 255 == 0 { try checkCancellation() }
            nodes[node.id] = node
            pending.append(contentsOf: node.children)
        }
    }
    return PreparedFolder(view: view, items: items, childIDs: childIDs, displayedIDs: displayedIDs, rowIndices: rowIndices,
                          nodesByID: nodes, initialColors: colors, preservedColors: preserved,
                          colorRevision: preserved == nil ? nil : previous?.revision,
                          colorMappingExtended: preserved.map { $0.count > (previous?.colors.count ?? 0) } ?? false)
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

private struct CollectionDrag {
    let token: Data
    let rootPath: String
    let rootIdentity: String?
    let nodes: [DiskNode]
    let omitted: [DiskNode]
    let groupContext: GroupContext?
}

private struct ReservedCollectionDrop {
    let drag: CollectionDrag
    let root: DiskNode
}

private struct GroupContext {
    let rootID: String
    let collected: Set<String>
    let home: String
    let protected: [String]
}

private func canBeginGroupedCollection(_ node: DiskNode, context: GroupContext) -> Bool {
    let id = node.id
    guard node.isAccessible, !node.isSymbolicLink, node.fileIdentity != nil,
          id != context.rootID, isWithin(id, root: context.rootID),
          !hasCollectedAncestor(id, in: context.collected) else { return false }
    guard !isWithin(context.home, root: id) else { return false }
    guard !context.protected.contains(where: { isWithin(id, root: $0) }) else { return false }
    let components = node.url.pathComponents
    guard !components.contains(".Trash"), !components.contains(".Trashes") else { return false }
    let directories = node.isDirectory ? components[...] : components.dropLast()
    return !directories.contains { $0.hasPrefix(".") }
}

private struct CollectionValidation: Sendable {
    var nodes: [DiskNode] = []
    var rejections: [String] = []
}

private struct CollectionChunkResult: Sendable {
    let application: CollectionApplication?
    let buffered: [DiskNode]
    let includedCount: Int
    let rejections: [String]
}

private struct CollectionApplication: Sendable {
    let collection: [DiskNode]
    let identities: [String: String]
    let coverage: CollectionCoverage?
    let size: Int64?
    let pathIDs: Set<String>?
    let addedCount: Int
    let addedName: String?
    let includedCount: Int
    let rejections: [String]
}

private func prepareCollectionApplication(_ validated: CollectionValidation, existing: [DiskNode],
                                          identities: [String: String], collected: Set<String>, includedCount: Int,
                                          cancellation: ScanCancellation) -> CollectionApplication? {
    let candidates = Set(validated.nodes.map(\.id))
    var included = includedCount
    var added: [DiskNode] = []
    for node in validated.nodes {
        if cancellation.isCancelled { return nil }
        if hasCollectedAncestor(node.id, in: collected) || hasCollectedAncestor(node.id, in: candidates, includingSelf: false) {
            included += 1
        } else { added.append(node) }
    }
    guard !added.isEmpty else {
        return CollectionApplication(collection: existing, identities: identities, coverage: nil, size: nil, pathIDs: nil,
                                     addedCount: 0, addedName: nil, includedCount: included, rejections: validated.rejections)
    }
    let addedIDs = Set(added.map(\.id))
    var retained: [DiskNode] = []
    for node in existing {
        if cancellation.isCancelled { return nil }
        if !hasCollectedAncestor(node.id, in: addedIDs) { retained.append(node) }
    }
    let retainedIDs = Set(retained.map(\.id))
    var resultIdentities = identities.filter { retainedIDs.contains($0.key) }
    for node in added {
        if cancellation.isCancelled { return nil }
        resultIdentities[node.id] = node.fileIdentity
    }
    let result = retained + added
    let coverage = CollectionCoverage(paths: result.map(\.id))
    guard !cancellation.isCancelled else { return nil }
    return CollectionApplication(collection: result, identities: resultIdentities, coverage: coverage, size: collectionTotal(result), pathIDs: Set(result.map(\.id)),
                                 addedCount: added.count, addedName: added.count == 1 ? added[0].name : nil,
                                 includedCount: included, rejections: validated.rejections)
}

func hasCollectedAncestor(_ path: String, in paths: Set<String>, includingSelf: Bool = true) -> Bool {
    guard !paths.isEmpty else { return false }
    func parent(_ path: String) -> String {
        let bytes = path.utf8
        if !bytes.contains(0), bytes.first == 47, (bytes.count == 1 || bytes.last != 47),
           !path.hasSuffix("/."), !path.hasSuffix("/.."),
           !zip(bytes, bytes.dropFirst()).contains(where: { $0 == 47 && $1 == 47 }),
           let slash = bytes.lastIndex(of: 47) {
            return slash == path.startIndex ? "/" : String(path[..<slash])
        }
        return URL(fileURLWithPath: path).deletingLastPathComponent().path
    }
    var candidate = includingSelf ? path : parent(path)
    while true {
        if paths.contains(candidate) { return true }
        if candidate == "/" { return false }
        let next = parent(candidate)
        if next == candidate { return false }
        candidate = next
    }
}

private func validateCollectionNodes(_ nodes: [DiskNode], root: URL,
                                     cancellation: ScanCancellation? = nil) -> CollectionValidation? {
    var result = CollectionValidation()
    let cloudValidator = CloudStoragePolicy.Validator()
    for node in nodes {
        if cancellation?.isCancelled == true { return nil }
        guard node.isAccessible, !node.isSymbolicLink else {
            result.rejections.append("\(node.name): this item cannot be collected.")
            continue
        }
        if let reason = TrashPolicy.rejectionReason(for: node.url, scanRoot: root, cloudValidator: cloudValidator) {
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

private func indexedFolder(_ path: String, store: DiskIndexStore, snapshot: IndexSnapshot,
                           colors: ColorPreparationState? = nil, cancellation: ScanCancellation? = nil,
                           removals: [ScanRemoval] = []) throws -> PreparedFolder {
    let nodes = try store.removalNodes(removals, snapshot: snapshot, cancellation: cancellation)
    let projection = DiskRemovalProjection(removals: removals, source: nodes)
    var target = path
    while true {
        let candidate = try store.node(path: target, snapshot: snapshot, cancellation: cancellation)
        if let candidate, candidate.isDirectory, projection.apply(candidate) != nil { break }
        guard target != snapshot.rootURL.path else { throw DiskIndexError.invalid("The indexed folder is unavailable.") }
        target = URL(fileURLWithPath: target).deletingLastPathComponent().path
        if !isWithin(target, root: snapshot.rootURL.path) { target = snapshot.rootURL.path }
    }
    let folder = try store.loadFolder(path: target, snapshot: snapshot, cancellation: cancellation)
    let rawAncestors = try store.ancestors(path: target, snapshot: snapshot, cancellation: cancellation)
    guard let projectedFolder = projection.apply(folder) else { throw DiskIndexError.invalid("The indexed folder is unavailable.") }
    var ancestors = rawAncestors.compactMap { projection.apply($0) }
    if !ancestors.isEmpty { ancestors[ancestors.count - 1] = projectedFolder }
    return try prepareFolder(IndexedFolder(root: ancestors.first ?? projectedFolder, folder: projectedFolder, ancestors: ancestors),
                             colors: colors, cancellation: cancellation)
}

struct CollectionReviewToken: Hashable {
    fileprivate let sessionID: UUID
    fileprivate let rootBytes: Data
    fileprivate let rootIdentity: String
    fileprivate let reviewID: UUID
    fileprivate let collectionRevision: UUID
    fileprivate let errorRevision: UInt64
}

struct AutomationRemovalReceipt {
    let id: UUID
    let sessionID: UUID
    var running: Bool
    var removedCount: Int?
    var status: String
    var error: String?
}

@MainActor
final class DiskModel: ObservableObject {
    private var atomicFolderInstall = false
    private var folderStateVersion: UInt64 = 0
    private var statusMessageRevision: UInt64 = 0
    private var errorMessageRevision: UInt64 = 0
    #if ORBIT_TESTING
    var testBeforeSessionReopen: @Sendable () async -> Void = {}
    var testForceCollectionFailure = false
    func testSetTransferFeedback(_ url: URL) {
        sessions[url.path]?.removalStatus = "removal status"
        sessions[url.path]?.collectionStatus = "queued collection status"
    }
    func testTransferFeedback(_ url: URL) -> String? { sessions[url.path]?.collectionStatus }
    func testActiveSessionRoot() -> String? { sessions.values.first { $0.id == activeSessionID }?.url.path }
    func testDetachValidation() { detachCollectionValidation() }
    func testHasPendingCollection(_ url: URL) -> Bool { sessions[url.path]?.collectionValidation != nil }
    func testSetCollectionEdit(_ url: URL, pending: Bool) {
        if pending, let identity = sessions[url.path]?.identity {
            let edit = CollectionEditQueue(rootIdentity: identity, rootBytes: Data(url.path.utf8)); edit.pending.insert(url.path); collectionEdits[url.path] = edit
        } else { collectionEdits[url.path] = nil }
    }
    func testActivateResolved(_ url: URL, force: Bool = false) {
        let identity = currentFileIdentity(url)
        scanResolved(url, root: url, identity: identity, targetIdentity: identity, queryAtRequest: query, force: force)
    }
    var testBeforeRootInstall: (() -> Void)?
    var testBeforeLifecycleTransition: ((String) -> Void)?
    var testBeforeFinishRouteLoad: @Sendable () -> Void = {}
    var testBeforeCollectionEdit: @Sendable () -> Void = {}
    #endif
    static let collectionDragType = NSPasteboard.PasteboardType("local.orbitdisk.collection-items")

    var root: DiskNode? {
        willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 }
        didSet {
            if root.map({ Data($0.id.utf8) }) != oldValue.map({ Data($0.id.utf8) }) || root?.fileIdentity != oldValue?.fileIdentity {
                clearCollectionDrag()
                detachCollectionValidation()
                attachCollectionValidation()
                clearHover()
            }
        }
    }
    var navigation: [DiskNode] = [] {
        willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 }
        didSet { mapChildIDs = nil; folderSizeOrder = nil; preparedMapHoverGroup = nil; invalidateDisplayedNodes() }
    }
    private(set) var mapChildIDs: [String]?
    var mapChildIDSet: Set<String>? { mapChildIDs == nil ? nil : folderSizeOrder?.ids }
    private(set) var mapRevision = UUID() {
        willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 }
        didSet {
            mapChildIDs = nil
            mapHoverPresentationCache = nil
            groupedMapHoverCache.removeAll()
            preparedMapHoverGroup = nil
        }
    }
    var selectedID: String? { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    var isScanning = false { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    private(set) var isPartialMap = false { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    var isRefreshing = false { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    var isOpeningFolder = false { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    @Published var scanTitle = "Mapping your files…"
    @Published var scanPath = ""
    @Published var progressCount = 0
    @Published var progressBytes: Int64 = 0
    var scanResult: ScanResult? { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    var indexedAt: Date? { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    var isSavedMap = false { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    private var updatesPausedRevision: UInt64 = 0
    var updatesPaused = false { willSet { if !atomicFolderInstall { objectWillChange.send() }; updatesPausedRevision &+= 1 } }
    private var suppressCollectionFeedbackInvalidation = false
    private func activeFeedbackSession() -> MappingSession? {
        guard !suppressCollectionFeedbackInvalidation, let root, let session = sessions[root.id], session.id == activeSessionID,
              Data(session.url.path.utf8) == Data(root.id.utf8), session.identity == root.fileIdentity else { return nil }
        return session
    }
    var errorMessage: String? {
        willSet {
            if newValue != errorMessage, let session = activeFeedbackSession() {
                session.invalidateCollectionError()
                if let shown = session.presentedCollectionError, errorMessage == shown {
                    session.presentedCollectionError = nil
                    if newValue != nil { session.enqueueCollectionError(shown) }
                }
            }
            if !atomicFolderInstall { objectWillChange.send() }; errorMessageRevision &+= 1
        }
        didSet { if errorMessage == nil && oldValue != nil { schedulePendingCollectionError() } }
    }
    var statusMessage: String? { willSet { if newValue != statusMessage || newValue != nil { activeFeedbackSession()?.invalidateCollectionStatus() }; if !atomicFolderInstall { objectWillChange.send() }; statusMessageRevision &+= 1; folderStateVersion &+= 1 } }
    @Published var query = "" {
        didSet { if query != oldValue { searchStateRevision &+= 1; selectedID = nil; invalidateDisplayedNodes(); clearHover(); search() } }
    }
    private var searchStateRevision: UInt64 = 0
    private var searchStateNodes: [DiskNode] = []
    private var searchStateCount = 0
    private var searchStateSearching = false
    private var searchStateIncomplete = false
    private var searchStateQuery = ""
    private var searchStateRootPath: String?
    private var searchStateRootIdentity: String?
    private var searchStateRemovalEpoch: UInt64 = 0
    private var searchStateUsesPreviewOrder = false
    var searchResults: [DiskNode] {
        get { searchStateNodes }
        set { _ = updateSearchState(nodes: newValue) }
    }
    var searchFailed: Bool { searchStateIncomplete }
    var isSearchResultOutdated: Bool {
        guard isFiltering else { return false }
        let session = sessions.values.first { $0.id == activeSessionID }
        return searchStateIncomplete || query.trimmingCharacters(in: .whitespacesAndNewlines) != searchStateQuery
            || searchStateRootPath != session?.url.path || searchStateRootIdentity != session?.identity
            || searchStateRemovalEpoch != (session?.removalEpoch ?? 0)
    }
    var searchMatchCount: Int {
        get { searchStateCount }
        set { _ = updateSearchState(count: newValue) }
    }
    var isSearching: Bool {
        get { searchStateSearching }
        set { _ = updateSearchState(searching: newValue) }
    }
    @discardableResult
    private func updateSearchState(nodes: [DiskNode]? = nil, count: Int? = nil, searching: Bool? = nil, incomplete: Bool? = nil, lookup: [String: DiskNode]? = nil) -> Bool {
        let revision = searchStateRevision, previousQuery = query
        let owner = activeSessionID, scanID = generation
        let session = sessions.values.first { $0.id == owner }
        let removalEpoch = session?.removalEpoch ?? 0
        objectWillChange.send()
        guard revision == searchStateRevision, previousQuery == query, owner == activeSessionID,
              scanID == generation, session?.removalEpoch ?? 0 == removalEpoch else { return false }
        if let nodes { searchStateNodes = nodes; invalidateDisplayedNodes() }
        if let count { searchStateCount = count }
        if let searching { searchStateSearching = searching }
        if let incomplete { searchStateIncomplete = incomplete }
        if let lookup { searchNodesByID = lookup }
        searchStateRevision &+= 1
        return true
    }
    private var collectionState = CollectionSnapshot(items: [], identities: [:], coverage: CollectionCoverage(), revision: UUID())
    private var collectionRevision: UUID { collectionState.revision }
    private var collectionEdits: [String: CollectionEditQueue] = [:]
    private var announcingCollectionEdit = false
    var isUpdatingCollection: Bool {
        guard let root, let session = sessions[root.id], session.id == activeSessionID,
              let edit = collectionEdits[root.id], edit.rootIdentity == session.identity,
              edit.rootBytes == Data(root.id.utf8) else { return false }
        return !edit.pending.isEmpty
    }
    func isRemovingFromCollection(_ id: String) -> Bool {
        guard let root, let session = sessions[root.id], session.id == activeSessionID,
              let edit = collectionEdits[root.id], edit.rootIdentity == session.identity,
              edit.rootBytes == Data(root.id.utf8) else { return false }
        return edit.pending.contains(id)
    }

    var collection: [DiskNode] {
        get { collectionState.items }
        set {
            let next = CollectionSnapshot(items: newValue, identities: collectionState.identities,
                                          coverage: CollectionCoverage(paths: newValue.map(\.id)), revision: UUID())
            let previousRevision = collectionRevision
            objectWillChange.send()
            guard collectionRevision == previousRevision else { return }
            collectionState = next
        }
    }
    var collectionCoverage: CollectionCoverage { displayedRootMatchesActiveSession ? collectionState.coverage : CollectionCoverage() }
    var displayedCollection: [DiskNode] { displayedRootMatchesActiveSession ? collectionState.items : [] }
    var displayedCollectionSize: Int64 { displayedRootMatchesActiveSession ? collectionState.size : 0 }
    var canReviewDisplayedCollection: Bool { displayedRootMatchesActiveSession && !collectionState.items.isEmpty }
    var collectionReviewToken: CollectionReviewToken? {
        guard showCollection, displayedRootMatchesActiveSession, let root, let identity = root.fileIdentity,
              let session = sessions[root.id], session.id == activeSessionID else { return nil }
        return CollectionReviewToken(sessionID: session.id, rootBytes: Data(root.id.utf8), rootIdentity: identity,
                                     reviewID: collectionReviewID, collectionRevision: collectionRevision,
                                     errorRevision: errorMessageRevision)
    }
    func collectionReviewHasOwner(_ token: CollectionReviewToken?) -> Bool {
        guard let token, let current = collectionReviewToken else { return false }
        return token.sessionID == current.sessionID && token.rootBytes == current.rootBytes
            && token.rootIdentity == current.rootIdentity && token.reviewID == current.reviewID
    }
    func collectionReviewMatches(_ token: CollectionReviewToken?) -> Bool {
        guard let token, collectionReviewHasOwner(token) else { return false }
        return token.collectionRevision == collectionRevision
    }
    func beginCollectionConfirmation(review token: CollectionReviewToken?) -> UUID? {
        guard !atomicFolderInstall, let token, collectionReviewMatches(token), !isRemoving,
              !isUpdatingCollection, collectionConfirmation == nil else { return nil }
        let id = UUID()
        collectionConfirmation = (id, token)
        return id
    }
    func endCollectionConfirmation(_ id: UUID?) {
        guard let id, collectionConfirmation?.id == id else { return }
        collectionConfirmation = nil
    }
    private func confirmationHoldsPublication(for session: MappingSession) -> Bool {
        guard let confirmation = collectionConfirmation, confirmation.owner.sessionID == session.id else { return false }
        return collectionReviewHasOwner(confirmation.owner)
    }
    @discardableResult
    func beginCollectionReview() -> CollectionReviewToken? {
        guard !atomicFolderInstall, !showCollection, canReviewDisplayedCollection, !isRemoving,
              !isUpdatingCollection else { return nil }
        let sessionID = activeSessionID, revision = collectionRevision, request = resolutionID
        objectWillChange.send()
        guard !atomicFolderInstall, !showCollection, activeSessionID == sessionID, collectionRevision == revision,
              resolutionID == request, canReviewDisplayedCollection, !isRemoving,
              !isUpdatingCollection else { return nil }
        atomicFolderInstall = true
        collectionReviewID = UUID()
        showCollection = true
        atomicFolderInstall = false
        return collectionReviewToken
    }
    func clearCollection(review token: CollectionReviewToken?) {
        guard !atomicFolderInstall, collectionReviewMatches(token), !isRemoving,
              !isUpdatingCollection else { return }
        objectWillChange.send()
        guard !atomicFolderInstall, let token, collectionReviewMatches(token), !isRemoving,
              !isUpdatingCollection else { return }
        atomicFolderInstall = true
        if let root, let session = sessions[root.id], session.id == activeSessionID {
            if let job = session.collectionValidation {
                job.token.cancel(); job.task?.cancel(); session.collectionValidation = nil
            }
            reservedCollectionDrops = reservedCollectionDrops.filter { _, reserved in
                !reserved.drag.rootPath.utf8.elementsEqual(root.id.utf8) || reserved.drag.rootIdentity != session.identity
            }
        }
        detachCollectionValidation()
        collectionState = CollectionSnapshot(items: [], identities: collectionState.identities,
                                             coverage: CollectionCoverage(), revision: UUID())
        if errorMessageRevision == token.errorRevision { errorMessage = nil }
        showCollection = false
        atomicFolderInstall = false
    }
    func dismissCollection(review token: CollectionReviewToken?) {
        guard !atomicFolderInstall, let token, showCollection, token.reviewID == collectionReviewID else { return }
        objectWillChange.send()
        guard !atomicFolderInstall, showCollection, token.reviewID == collectionReviewID else { return }
        atomicFolderInstall = true
        if collectionReviewHasOwner(token) && errorMessageRevision == token.errorRevision { errorMessage = nil }
        showCollection = false
        atomicFolderInstall = false
    }
    func dismissCollectionError(review token: CollectionReviewToken?) {
        guard !atomicFolderInstall, let token, collectionReviewHasOwner(token),
              errorMessageRevision == token.errorRevision else { return }
        objectWillChange.send()
        guard !atomicFolderInstall, collectionReviewHasOwner(token), errorMessageRevision == token.errorRevision else { return }
        atomicFolderInstall = true
        errorMessage = nil
        atomicFolderInstall = false
    }
    func removeFromCollection(_ id: String, review token: CollectionReviewToken?) {
        guard let token, collectionReviewMatches(token) else { return }
        removeFromCollection(id, expectedReview: token)
    }

    private(set) var draggingLabel: String?
    private var collectionDragEpoch: UInt64 = 0
    private(set) var isValidatingCollection = false { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    var hoveredMapNodes: [DiskNode] = [] {
        willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 }
        didSet { mapHoverPresentationCache = nil; preparedMapHoverGroup = nil }
    }
    var hoveredMapGroupID: String? {
        willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 }
        didSet {
            if hoveredMapGroupID != oldValue { mapHoverPresentationCache = nil; preparedMapHoverGroup = nil }
        }
    }
    var hoveredListID: String? { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    private var collectionReviewID = UUID()
    private var collectionConfirmation: (id: UUID, owner: CollectionReviewToken)?
    var showCollection = false {
        willSet { if !atomicFolderInstall { objectWillChange.send() } }
        didSet { if !showCollection { collectionConfirmation = nil } }
    }
    @Published var isRemoving = false
    @Published private(set) var isRemovingAcrossSessions = false
    private var activeRemovalIDs = Set<UUID>()
    private var removalReceipts: [UUID: AutomationRemovalReceipt] = [:]
    private var removalReceiptOrder: [UUID] = []
    private var startingRemovals: [UUID: (path: Data, identity: String?)] = [:]
    private var finishingRemovals: [UUID: (path: Data, identity: String?)] = [:]
    var hasActiveRemovals: Bool { !activeRemovalIDs.isEmpty }

    func beginRemovalActivity() -> UUID {
        let id = UUID()
        activeRemovalIDs.insert(id)
        reconcileRemovalActivity()
        return id
    }

    func endRemovalActivity(_ id: UUID) {
        activeRemovalIDs.remove(id)
        reconcileRemovalActivity()
    }
    private func reconcileRemovalActivity() {
        while isRemovingAcrossSessions != hasActiveRemovals {
            isRemovingAcrossSessions = hasActiveRemovals
        }
    }
    @Published var previewURL: URL?
    @Published var showIssues = false
    var volumes: [DiskVolume] = [DiskVolume(url: URL(fileURLWithPath: "/"), name: "Mac", capacity: 0, available: 0)] {
        willSet { if !atomicFolderInstall { objectWillChange.send() } }
    }
    private let volumeDiscovery: @Sendable () -> [DiskVolume]
    private var volumeRefreshID = UUID()
    private var volumeRefreshInFlight = false
    private var announcingVolumeRefresh = false
    var isDiscoveringVolumes = true { willSet { if !atomicFolderInstall { objectWillChange.send() } } }
    @Published var sortByName = false {
        didSet {
            guard sortByName != oldValue else { return }
            let oldNodes = displayedNodesCache
            let oldItems = displayedItemsCache
            let oldIndices = displayedRowIndices
            let oldIDs = displayedIDsCache
            let wasSorting = isSortingRows
            if !sortByName && !wasSorting {
                let nodes = isFiltering ? searchResults : current?.children ?? []
                resetLargeListIfReordered(oldItems, count: nodes.count) { index in
                    if !isFiltering, let order = folderSizeOrder { return order.items[index].id }
                    return nodes[index].id
                }
            }
            invalidateDisplayedNodes()
            if (isSortingRows || (wasSorting && !sortByName)), let oldNodes {
                displayedNodesCache = oldNodes
                displayedItemsCache = oldItems
                displayedRowIndices = oldIndices
                displayedIDsCache = oldIDs
            }
        }
    }
    private(set) var isSortingRows = false { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    private(set) var displayedOrderRevision: UInt64 = 0 { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    private(set) var largeListIdentity: UInt64 = 0 { willSet { if !atomicFolderInstall { objectWillChange.send() }; folderStateVersion &+= 1 } }
    var largeListWillReset: (() -> Void)?
    var didLaunch = false
    private let store: DiskIndexStore
    private let preferences: UserDefaults
    private let stagingDirectory: URL
    private let scanProgressObserver: @Sendable (ScanProgress) -> Void
    private let sortStartObserver: @Sendable () -> Void
    private var snapshot: IndexSnapshot? {
        didSet {
            guard oldValue?.id != snapshot?.id || oldValue?.databaseURL != snapshot?.databaseURL else { return }
            prepareSearchIndex()
        }
    }
    private var scanCancellation: ScanCancellation?
    private var searchCancellation: ScanCancellation?
    private var sessions: [String: MappingSession] = [:]
    private var activeSessionID: UUID?
    private var resolutionID = UUID()
    private var resolutionWorker: Task<(URL, String?, String?, Bool, String?), Never>?
    private var generation = UUID()
    private var browseGeneration = UUID()
    private var requestedFolderPath: String?
    private var requestedTarget: (path: String, identity: String)?
    private var scanPreview: DiskScanPreview?
    private var previewTask: Task<Void, Never>?
    private var lastPreviewIsListing = false
    #if ORBIT_TESTING
    var testScanner = DiskScanner()
    func testPreviewRetained(_ path: String) -> Bool { sessions[path]?.preview != nil }
    func testScanFailed(_ path: String) -> Bool { sessions[path]?.scanFailed == true }
    var testBeforeResolution: @Sendable () -> Void = {}
    var testBeforeRemoval: @Sendable () -> Void = {}
    var testBeforeRemovalMerge: @Sendable () -> Void = {}
    var testBeforeRefreshSave: @Sendable () -> Void = {}
    var testAfterPublishedRemovalCursorRead: @Sendable () -> Void = {}
    var testMonitorUnavailable = false
    func testStopMonitor() { monitorTask?.cancel(); monitorTask = nil; monitor?.stop(); monitor = nil }
    func testStopJournalMonitor() { removalJournalTask?.cancel(); removalJournalTask = nil }
    private(set) var testJournalPollCount = 0
    var testRemovalEpoch: UInt64? { sessions.values.first(where: { $0.id == activeSessionID })?.removalEpoch }
    var testBeforeScannerStart: @Sendable () -> Void = {}
    var testAfterStageAttach: @Sendable () -> Void = {}
    var testAfterStageStep: @Sendable (ResumableStage) -> Void = { _ in }
    var testConfigureStage: @Sendable (ResumableStage) -> Void = { _ in }
    var testBeforePreviewSearch: @Sendable () -> Void = {}
    var testAfterPreviewSearch: @Sendable (String) -> Void = { _ in }
    var testBeforePreviewRead: @Sendable () -> Void = {}
    var testRecoveryParentLimit = 10_000
    var testHasDeferredPreview: Bool { deferredPreview != nil }
    var testMapInteractionActive: Bool { mapInteractionActive }
    var testHasDeferredNameSort: Bool { deferredNameSort != nil }
    func testSetDeferredPublication(first: @escaping () -> Void, second: @escaping () -> Void) {
        deferredScanCompletion = first
        deferredIndexedFolder = second
        deferredNameSort = ([], [], [:], nameSortRevision)
    }
    func testRefreshPreview() {
        guard let preview = scanPreview, let token = scanCancellation else { return }
        lastPreviewRevision = nil
        Task { await readPreview(preview, scanID: generation, token: token, force: true) }
    }
    var testSavedSearchOrder: @Sendable (Bool) -> Void = { _ in }
    func testRetrySearch() { search() }
    func testReachable(root: URL, target: URL) -> Bool {
        guard let identity = currentFileIdentity(root) else { return false }
        return isReachableWithinSession(root: root, target: target, expectedIdentity: identity)
    }
    func testContains(_ path: String, root: String) -> Bool { containsBytes(path, root: root) }
    var testCollectionPrepare: @Sendable () async -> Void = {}
    private(set) var testConfirmationPublicationHoldCount = 0
    #endif
    private var previewPreparationWorker: Task<PreparedPreview?, Error>?
    private var recoveryParentLimit: Int {
        #if ORBIT_TESTING
        return testRecoveryParentLimit
        #else
        return 10_000
        #endif
    }
    private var previewPreparationID = UUID()
    private var folderPreparationWorker: Task<PreparedFolder, Error>?
    private var folderPreparationCancellation: ScanCancellation?
    private var lastPreviewRevision: UInt64?
    private var lastPreviewPath: String?
    private var mapInteractionActive = false
    private var mapInteractionOwner: UUID?
    private var deferredPreview: (view: PreparedPreview, scanID: UUID, browseID: UUID)?
    private var deferredScanCompletion: (() -> Void)?
    private var deferredIndexedFolder: (() -> Void)?
    var hasDeferredIndexedFolder: Bool { deferredIndexedFolder != nil }
    private var searchTask: Task<Void, Never>?
    private var searchPreparationTask: Task<Void, Never>?
    private var searchPreparationCancellation: ScanCancellation?
    private var searchPreparationSnapshotID: String?
    private var searchPreparationDatabaseURL: URL?
    private var refreshTask: Task<Void, Never>?
    private var workCompletion: Task<Void, Never>?
    private var monitor: DiskChangeMonitor?
    private var monitorTask: Task<Void, Never>?
    private var removalJournalTask: Task<Void, Never>?
    private var pendingDirectories = Set<String>()
    private var pendingFullRefresh = false
    private var pendingCheckpoint: DiskChangeCheckpoint?
    private var nodesByID: [String: DiskNode] = [:]
    private var searchNodesByID: [String: DiskNode] = [:]
    private var folderCache: [String: IndexedFolder] = [:]
    private var folderOrder: [String] = []
    private var collectionIdentities: [String: String] {
        get { collectionState.identities }
        set {
            let next = CollectionSnapshot(items: collectionState.items, identities: newValue,
                                          coverage: collectionState.coverage, revision: UUID(), size: collectionState.size, pathIDs: collectionState.pathIDs)
            let previousRevision = collectionRevision
            objectWillChange.send()
            guard collectionRevision == previousRevision else { return }
            collectionState = next
        }
    }
    private var collectionDrag: CollectionDrag?
    private var reservedCollectionDrops: [UUID: ReservedCollectionDrop] = [:]
    private var cancellingCollectionValidation = false
    private var collectionValidationID: UUID?
    private var collectionValidationStatusRevision: UInt64?
    private var collectionValidationRequestID: UUID?
    private var collectionValidationGeneration: UUID?
    private var removalResultStatus: String?
    private var displayedNodesCache: [DiskNode]? {
        didSet { displayedItemsCache = nil; displayedRowIndices = nil }
    }
    private var displayedItemsCache: [DiskListItem]?
    private var displayedRowIndices: [String: Int]?
    private var folderSizeOrder: FolderSizeOrder?
    private var nameSortWorker: Task<(nodes: [DiskNode], items: [DiskListItem], indices: [String: Int]), Error>?
    private var nameSortTask: Task<Void, Never>?
    private var nameSortRevision = UUID()
    private var deferredNameSort: (nodes: [DiskNode], items: [DiskListItem], indices: [String: Int], revision: UUID)?
    private var displayedIDsCache: Set<String>?
    private var mapHoverPresentationCache: MapHoverPresentation?
    private var preparedMapHoverGroup: PreparedMapHoverGroup?
    private let groupedMapHoverCache = GroupedMapHoverCache()
    private(set) var childColorIndices: [String: Int] = [:]
    private var colorMappingRevision = UUID()

    @discardableResult
    private func clearPresentation(clearSelection: Bool = false) -> Bool {
        let owner = activeSessionID, scanID = generation, browseID = browseGeneration, requestID = resolutionID
        let session = sessions.values.first { $0.id == owner }
        let epoch = session?.removalEpoch ?? 0, version = folderStateVersion
        let removing = isRemoving, collectionVersion = collectionRevision
        let snapshotID = snapshot?.id, partial = isPartialMap, preview = scanPreview
        let readiness = preview?.removalReplayReadiness()
        objectWillChange.send()
        guard activeSessionID == owner, generation == scanID, browseGeneration == browseID,
              resolutionID == requestID, session?.removalEpoch ?? 0 == epoch, folderStateVersion == version,
              isRemoving == removing, collectionRevision == collectionVersion,
              snapshot?.id == snapshotID, isPartialMap == partial, scanPreview === preview,
              preview?.removalReplayReadiness()?.generation == readiness?.generation,
              preview?.removalReplayReadiness()?.blocked == readiness?.blocked else { return false }
        atomicFolderInstall = true
        defer { atomicFolderInstall = false }
        root = nil; navigation = []; nodesByID.removeAll()
        if clearSelection { selectedID = nil; searchNodesByID.removeAll(); childColorIndices.removeAll(); colorMappingRevision = UUID() }
        return true
    }

    private func presentationTransition(_ name: String, _ action: () -> Void) -> Bool {
        guard !atomicFolderInstall else { return false }
        let owner = activeSessionID, scanID = generation, browseID = browseGeneration, requestID = resolutionID
        let session = sessions.values.first { $0.id == owner }
        let epoch = session?.removalEpoch ?? 0, version = folderStateVersion
        let snapshotID = snapshot?.id, partial = isPartialMap, preview = scanPreview
        let readiness = preview?.removalReplayReadiness()
        #if ORBIT_TESTING
        testBeforeLifecycleTransition?(name)
        #endif
        objectWillChange.send()
        guard activeSessionID == owner, generation == scanID, browseGeneration == browseID,
              resolutionID == requestID, session?.removalEpoch ?? 0 == epoch, folderStateVersion == version,
              snapshot?.id == snapshotID, isPartialMap == partial, scanPreview === preview,
              preview?.removalReplayReadiness()?.generation == readiness?.generation,
              preview?.removalReplayReadiness()?.blocked == readiness?.blocked else { return false }
        atomicFolderInstall = true
        defer { atomicFolderInstall = false }
        action()
        return true
    }

    #if ORBIT_TESTING
    func testCollectionPathIDs() -> Set<String> { collectionState.pathIDs }
    func testCancelCollectionValidation() { cancelCollectionValidation() }
    func testCollectInBackground(_ nodes: [DiskNode]) -> Bool {
        guard let root, let identity = root.fileIdentity else { return false }
        return collectInBackground(nodes, root: root, rootIdentity: identity, groupContext: nil)
    }
    func testBumpResolution() { resolutionID = UUID() }

    func testClearSynthetic() -> Bool { clearPresentation(clearSelection: true) }
    func testClearPresentationKeepingColors() -> Bool { clearPresentation() }
    var testColorMappingRevision: UUID { colorMappingRevision }
    private var testDeferredColorFolder: PreparedFolder?
    func testPrepareDeferredColorFolder(_ node: DiskNode) async throws {
        let previous = colorPreparationState
        let view = IndexedFolder(root: node, folder: node, ancestors: [node])
        testDeferredColorFolder = try await Task.detached { try prepareFolder(view, colors: previous) }.value
    }
    func testInstallDeferredColorFolder() -> Bool {
        guard let prepared = testDeferredColorFolder else { return false }
        testDeferredColorFolder = nil
        return installFolder(prepared)
    }
    func testInstallColorFolder(_ node: DiskNode) async throws {
        let previous = colorPreparationState
        let view = IndexedFolder(root: node, folder: node, ancestors: [node])
        let prepared = try await Task.detached { try prepareFolder(view, colors: previous) }.value
        guard installFolder(prepared) else { throw NSError(domain: "Test folder installation rejected", code: 1) }
    }

    func testInstallSynthetic(_ node: DiskNode, selection: String? = nil) -> Bool {
        let view = IndexedFolder(root: node, folder: node, ancestors: [node])
        return installFolder(try! prepareFolder(view), selection: selection)
    }
    #endif

    init(cacheDirectory: URL? = nil, stagingDirectory: URL? = nil, preferences: UserDefaults = .standard,
         scanProgressObserver: @escaping @Sendable (ScanProgress) -> Void = { _ in },
         sortStartObserver: @escaping @Sendable () -> Void = {},
         volumeDiscovery: (@Sendable () -> [DiskVolume])? = nil) {
        self.preferences = preferences
        self.volumeDiscovery = volumeDiscovery ?? { discoverVolumes() }
        self.scanProgressObserver = scanProgressObserver
        self.sortStartObserver = sortStartObserver
        let directory = cacheDirectory ?? FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(Bundle.main.bundleIdentifier ?? "local.orbitdisk.app")
            .appendingPathComponent("Indexes", isDirectory: true)
        store = DiskIndexStore(directory: directory)
        self.stagingDirectory = (stagingDirectory ?? store.lockDirectory).standardizedFileURL
        refreshVolumes()
    }

    deinit {
        searchCancellation?.cancel(); searchTask?.cancel()
        searchPreparationCancellation?.cancel(); searchPreparationTask?.cancel()
    }

    private func prepareSearchIndex() {
        if let snapshot, searchPreparationSnapshotID == snapshot.id,
           searchPreparationDatabaseURL == snapshot.databaseURL, searchPreparationTask != nil { return }
        searchPreparationCancellation?.cancel(); searchPreparationTask?.cancel()
        searchPreparationCancellation = nil; searchPreparationTask = nil
        searchPreparationSnapshotID = nil; searchPreparationDatabaseURL = nil
        guard let snapshot else { return }
        let store = store, cancellation = ScanCancellation()
        searchPreparationCancellation = cancellation; searchPreparationSnapshotID = snapshot.id
        searchPreparationDatabaseURL = snapshot.databaseURL
        searchPreparationTask = Task.detached(priority: .utility) { [weak self] in
            do {
                // Give the initial folder read a head start over the optional search cache.
                try await Task.sleep(nanoseconds: 250_000_000)
                _ = try store.prepareSearch(snapshot: snapshot, cancellation: cancellation)
            } catch {}
            await self?.finishSearchPreparation(cancellation)
        }
    }

    private func finishSearchPreparation(_ cancellation: ScanCancellation) {
        guard searchPreparationCancellation === cancellation else { return }
        searchPreparationCancellation = nil; searchPreparationTask = nil
        searchPreparationSnapshotID = nil; searchPreparationDatabaseURL = nil
    }

    var automationSessionID: UUID? { displayedRootMatchesActiveSession && !isOpeningFolder ? activeSessionID : nil }
    func automationRemovalReceipt(_ id: UUID) -> AutomationRemovalReceipt? { removalReceipts[id] }
    func latestAutomationRemovalReceipt(sessionID: UUID) -> AutomationRemovalReceipt? {
        removalReceiptOrder.reversed().lazy.compactMap { self.removalReceipts[$0] }.first { $0.sessionID == sessionID }
    }
    private func finishRemovalReceipt(_ id: UUID, count: Int, permanently: Bool, failures: [String]) {
        guard var receipt = removalReceipts[id] else { return }
        receipt.running = false
        receipt.removedCount = count
        let noun = count == 1 ? "item" : "items"
        receipt.status = permanently ? "Deleted \(count) \(noun) permanently." : "Moved \(count) \(noun) to Trash."
        receipt.error = failures.isEmpty ? nil : failures.joined(separator: "\n")
        removalReceipts[id] = receipt
        var finished = 0
        for key in removalReceiptOrder.reversed() where removalReceipts[key]?.running == false {
            finished += 1
            if finished > 32 { removalReceipts.removeValue(forKey: key) }
        }
        removalReceiptOrder.removeAll { removalReceipts[$0] == nil }
    }

    var current: DiskNode? { navigation.last }
    var isUpdatingRemovalSizes: Bool {
        guard let session = sessions.values.first(where: { $0.id == activeSessionID }) else { return false }
        return isRemoving || session.isRemoving || session.durableRecoveryPending || !session.removals.isEmpty
    }
    func isUpdatingRemovalSize(of node: DiskNode) -> Bool {
        node.isDirectory && isUpdatingRemovalSizes
    }
    private var colorPreparationState: ColorPreparationState? {
        guard let current else { return nil }
        return ColorPreparationState(path: current.id, revision: colorMappingRevision, colors: childColorIndices)
    }
    var selected: DiskNode? { selectedID.flatMap { node(withID: $0) } }
    func node(withID id: String) -> DiskNode? { searchNodesByID[id] ?? nodesByID[id] }
    var isFiltering: Bool { !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    var displayedNodes: [DiskNode] {
        if let cached = displayedNodesCache { return cached }
        let nodes = isFiltering ? searchResults : current?.children ?? []
        let displayed = sortByName && !isSortingRows
            ? nodes.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending } : nodes
        displayedNodesCache = displayed
        return displayed
    }
    var displayedItems: [DiskListItem] {
        if let cached = displayedItemsCache { return cached }
        let items = displayedNodes.map { DiskListItem(id: $0.id, node: $0) }
        displayedItemsCache = items
        return items
    }
    func firstDisplayedRowIndex(matching ids: Set<String>) -> Int? {
        let items = displayedItems
        guard !ids.isEmpty else { return nil }
        if items.count < 1_000 || ids.count > items.count / 8 {
            return items.firstIndex { ids.contains($0.id) }
        }
        if displayedRowIndices == nil {
            var indices: [String: Int] = [:]
            indices.reserveCapacity(items.count)
            for (index, item) in items.enumerated() where indices[item.id] == nil {
                indices[item.id] = index
            }
            displayedRowIndices = indices
        }
        guard let indices = displayedRowIndices else { return nil }
        return ids.compactMap { indices[$0] }.min()
    }
    var collectionSize: Int64 { collectionState.size }
    var canCollectSelection: Bool {
        selected.map(canCollect) ?? false
    }
    private var displayedRootMatchesActiveSession: Bool {
        guard let root, let session = sessions[root.id] else { return false }
        return session.id == activeSessionID && session.identity != nil && session.identity == root.fileIdentity
            && Data(session.url.path.utf8) == Data(root.id.utf8)
    }

    var canAcceptCollectionDrop: Bool {
        guard displayedRootMatchesActiveSession, let drag = collectionDrag, let root else { return false }
        return drag.rootPath.utf8.elementsEqual(root.id.utf8) && drag.rootIdentity == root.fileIdentity
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
        scheduleNameSort()
        if !isFiltering, (!sortByName || isSortingRows), let order = folderSizeOrder {
            displayedNodesCache = order.nodes
            displayedItemsCache = order.items
            displayedRowIndices = order.indices
            displayedIDsCache = order.ids
        }
        displayedOrderRevision &+= 1
    }

    private func resetLargeListIfReordered(_ previous: [DiskListItem]?, count: Int,
                                           idAt: (Int) -> String) {
        guard let previous, previous.count >= 5_000, count >= 5_000 else { return }
        if previous.count == count && !previous.indices.contains(where: { previous[$0].id != idAt($0) }) { return }
        largeListWillReset?()
        largeListIdentity &+= 1
    }

    private func scheduleNameSort() {
        nameSortWorker?.cancel()
        nameSortTask?.cancel()
        deferredNameSort = nil
        nameSortRevision = UUID()
        let nodes = isFiltering ? searchResults : current?.children ?? []
        guard sortByName, nodes.count >= 2_000 else {
            nameSortWorker = nil; nameSortTask = nil; isSortingRows = false
            return
        }
        isSortingRows = true
        let revision = nameSortRevision
        let observer = sortStartObserver
        let worker = Task.detached(priority: .userInitiated) { () throws -> (nodes: [DiskNode], items: [DiskListItem], indices: [String: Int]) in
            observer()
            try Task.checkCancellation()
            var comparisons = 0
            let sorted = try nodes.sorted { left, right in
                comparisons &+= 1
                if comparisons & 255 == 0 { try Task.checkCancellation() }
                return left.name.localizedStandardCompare(right.name) == .orderedAscending
            }
            var items: [DiskListItem] = []
            items.reserveCapacity(sorted.count)
            var indices: [String: Int] = [:]
            indices.reserveCapacity(sorted.count)
            for (index, node) in sorted.enumerated() {
                if index & 255 == 0 { try Task.checkCancellation() }
                let id = node.id
                items.append(DiskListItem(id: id, node: node))
                if indices[id] == nil { indices[id] = index }
            }
            return (sorted, items, indices)
        }
        nameSortWorker = worker
        nameSortTask = Task { [weak self] in
            guard let result = try? await worker.value, let self, !Task.isCancelled,
                  self.nameSortRevision == revision else { return }
            self.nameSortWorker = nil
            self.nameSortTask = nil
            if self.mapInteractionActive {
                self.deferredNameSort = (result.nodes, result.items, result.indices, revision)
            } else {
                self.finishNameSort(nodes: result.nodes, items: result.items, indices: result.indices, revision: revision)
            }
        }
    }

    private func finishNameSort(nodes: [DiskNode], items: [DiskListItem], indices: [String: Int], revision: UUID) {
        guard nameSortRevision == revision, sortByName else { return }
        resetLargeListIfReordered(displayedItemsCache, count: items.count) { items[$0].id }
        displayedNodesCache = nodes
        displayedItemsCache = items
        displayedRowIndices = indices
        isSortingRows = false
        displayedOrderRevision &+= 1
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
        guard displayedRootMatchesActiveSession, let root else { return false }
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let protected = ["/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc", home + "/Library"]
        return canBeginGroupedCollection(node, context: GroupContext(rootID: root.id, collected: collectionState.pathIDs,
                                                                    home: home, protected: protected))
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
        let requestedURL = url.standardizedFileURL
        if root?.id != requestedURL.path { clearCollectionDrag() }
        let queryAtRequest = query
        if force, snapshot != nil, root?.id == requestedURL.path { isRefreshing = true }
        resolutionWorker?.cancel(); resolutionWorker = nil
        let request = UUID(); resolutionID = request
        let session = force ? nil : sessions.values.filter { session in
            containsBytes(requestedURL.path, root: session.url.path) && session.identity != nil
        }.max { $0.url.path.utf8.count < $1.url.path.utf8.count }
        let candidateURL = session?.url, candidateIdentity = session?.identity
        let candidateEpoch = session?.removalEpoch
        let coveringRemovals = session?.removals.filter { containsBytes(requestedURL.path, root: $0.path) } ?? []
        #if ORBIT_TESTING
        let observer = testBeforeResolution
        #else
        let observer: @Sendable () -> Void = {}
        #endif
        isOpeningFolder = true
        let cache = store.directory, staging = stagingDirectory
        let check = Task.detached(priority: .userInitiated) { () -> (URL, String?, String?, Bool, String?) in
            observer()
            if let reason = CloudStoragePolicy.exclusionReason(for: requestedURL) {
                return (requestedURL, nil, nil, false, reason)
            }
            do { return try CloudStoragePolicy.withoutMaterialization {
            let identity = currentFileIdentity(requestedURL)
            let replaced = coveringRemovals.contains { removal in
                guard let live = currentFileIdentity(URL(fileURLWithPath: removal.path)) else { return false }
                return live != removal.identity
            }
            if let candidateURL, let candidateIdentity, !replaced,
               isReachableWithinSession(root: candidateURL, target: requestedURL, expectedIdentity: candidateIdentity) {
                return (candidateURL, candidateIdentity, identity, true, nil)
            }
            if !force, !Task.isCancelled,
               let coverage = try? MappingCoverage.find(for: requestedURL, cache: cache, stagingDirectory: staging) {
                return (coverage.rootURL, coverage.rootIdentity, coverage.requestedIdentity, false, nil)
            }
            return (requestedURL, identity, identity, false, nil)
            } } catch { return (requestedURL, nil, nil, false, error.localizedDescription) }
        }
        resolutionWorker = check
        Task { [weak self, weak session] in
            let (coveredRoot, coveredIdentity, requestedIdentity, usedSession, exclusion) = await check.value
            guard let self, self.resolutionID == request, !Task.isCancelled else { return }
            self.resolutionWorker = nil
            if let exclusion {
                self.isOpeningFolder = false
                self.errorMessage = exclusion
                return
            }
            if let session, session.removalEpoch != candidateEpoch {
                self.scan(requestedURL, force: force); return
            }
            let knownTarget = self.node(withID: requestedURL.path)
            let targetMatches = knownTarget == nil || knownTarget?.fileIdentity == requestedIdentity
            let canReuse = targetMatches && (!usedSession || session.map { self.sessions[$0.url.path] === $0 } == true)
            let resolvedRoot = canReuse ? coveredRoot : requestedURL
            let resolvedIdentity = canReuse ? coveredIdentity : requestedIdentity
            self.scanResolved(requestedURL, root: resolvedRoot, identity: resolvedIdentity,
                              targetIdentity: requestedIdentity, queryAtRequest: queryAtRequest, force: force)
        }
    }

    private func scanResolved(_ requestedURL: URL, root url: URL, identity: String?, targetIdentity: String?, queryAtRequest: String, force: Bool) {
        if !force, let saved = snapshot, !isScanning, url.path == saved.rootURL.path,
           sessions[url.path]?.scanFailed != true,
           sessions[url.path]?.token.isCancelled == false,
           identity != nil, root?.fileIdentity == identity, containsBytes(requestedURL.path, root: saved.rootURL.path) {
            if let node = node(withID: requestedURL.path), node.isDirectory {
                if node.fileIdentity != targetIdentity { scan(requestedURL, force: true); return }
                if node.id == current?.id { isOpeningFolder = false; return }
                if query == queryAtRequest { query = "" }
                loadFolder(node.id)
                return
            }
        }
        if !force, isPartialMap, let root, url.path == root.id, identity != nil, root.fileIdentity == identity,
           sessions[url.path]?.scanFailed != true,
           sessions[url.path]?.token.isCancelled == false,
           containsBytes(requestedURL.path, root: root.id), let node = node(withID: requestedURL.path), node.isDirectory {
            if node.fileIdentity != targetIdentity { scan(requestedURL, force: true); return }
            if node.id == current?.id { isOpeningFolder = false; return }
            if query == queryAtRequest { query = "" }
            loadFolder(node.id)
            return
        }
        let sameRoot = root?.id == url.path && root?.fileIdentity == identity
        let desiredPath = requestedURL.path != url.path ? requestedURL.path
            : sameRoot ? requestedFolderPath ?? current?.id ?? url.path : url.path
        if !sameRoot { clearCollectionDrag() }
        if let id = activeSessionID, let active = sessions.values.first(where: { $0.id == id }) {
            active.lastRoute = requestedFolderPath ?? current?.id
            active.rememberedFolder?.selection = selectedID
            active.collectionState = collectionState
            let starting = startingRemovals.values.contains {
                $0.path == Data(active.url.path.utf8) && $0.identity == active.identity
            }
            let finishing = finishingRemovals.values.contains {
                $0.path == Data(active.url.path.utf8) && $0.identity == active.identity
            }
            active.isRemoving = starting || (finishing ? active.isRemoving : isRemoving)
            active.pendingDirectories.formUnion(pendingDirectories)
            active.pendingCheckpoint = pendingCheckpoint ?? active.pendingCheckpoint
            active.pendingFullRefresh = active.pendingFullRefresh || pendingFullRefresh
        }
        let prior = sessions[url.path]
        let reusable = !force && identity != nil && prior?.identity == identity && prior?.token.isCancelled == false && prior?.scanFailed == false
        let preserve = activeSessionID != nil && (!sameRoot || reusable)
        stopWork(preserveScan: preserve, preserveCollection: true)
        let scanID = UUID()
        generation = scanID
        if requestedURL.path != url.path, let targetIdentity { requestedTarget = (requestedURL.path, targetIdentity) }
        if reusable, let session = prior {
            collectionState = session.collectionState.renewed()
            isRemoving = session.isRemoving
            statusMessage = session.removalStatus
            errorMessage = session.removalError
            removalResultStatus = session.isRemoving ? nil : session.removalStatus
            pendingDirectories.formUnion(session.removalParents)
            pendingDirectories.formUnion(session.pendingDirectories); session.pendingDirectories.removeAll()
            pendingCheckpoint = session.pendingCheckpoint; session.pendingCheckpoint = nil
            pendingFullRefresh = session.pendingFullRefresh; session.pendingFullRefresh = false
            showCollection = false
            activeSessionID = session.id
            attachCollectionValidation()
            beginRemovalJournalMonitoring(session)
            scanCancellation = session.token
            let route = requestedURL.path != url.path ? requestedURL.path : (session.lastRoute ?? desiredPath)
            requestedFolderPath = route
            if session.finished {
                isScanning = true; isRefreshing = false; updatesPaused = false
                scanTitle = "Opening saved map…"
                let store = store, colors = colorPreparationState, identity = session.identity
                let previousRefresh = session.refreshWorker
                if previousRefresh != nil {
                    let removals = session.removals, epoch = session.removalEpoch
                    isScanning = false; isRefreshing = true
                    Task { [weak self] in
                        let baseline = try? await Task.detached { () throws -> (IndexSnapshot, PreparedFolder) in
                            guard let saved = store.savedSnapshot(for: url) else { throw DiskIndexError.invalid("The saved map is unavailable.") }
                            let view = try indexedFolder(route, store: store, snapshot: saved, colors: colors,
                                                         cancellation: session.token, removals: removals)
                            return (saved, view)
                        }.value
                        guard let self, self.generation == scanID, self.isRefreshing,
                              session.removalEpoch == epoch,
                              self.requestedFolderPath?.utf8.elementsEqual(route.utf8) == true,
                              let (saved, view) = baseline else { return }
                        self.snapshot = saved; self.isPartialMap = false
                        guard self.publish(view, snapshot: saved) else { return }
                    }
                }
                let retainedRemovals = session.removals
                #if ORBIT_TESTING
                let beforeSessionReopen = testBeforeSessionReopen
                #endif
                let reopen = Task.detached { () throws -> (IndexSnapshot, PreparedFolder, Bool) in
                    #if ORBIT_TESTING
                    await beforeSessionReopen()
                    #endif
                    if let previousRefresh { _ = try? await previousRefresh.value }
                    guard !session.token.isCancelled else { throw CancellationError() }
                    guard let saved = store.savedSnapshot(for: url) else { throw DiskIndexError.invalid("The saved map is unavailable.") }
                    let view = try indexedFolder(route, store: store, snapshot: saved, colors: colors, cancellation: session.token, removals: retainedRemovals)
                    guard view.root.fileIdentity == identity else { throw DiskIndexError.invalid("The mapped folder changed.") }
                    return (saved, view, true)
                }
                observeSession(reopen, scanID: scanID, token: session.token)
                if let preview = session.preview {
                    guard beginPreview(preview, rootURL: url, path: route, scanID: scanID, token: session.token) else { return }
                } else if let saved = session.savedSnapshot {
                    snapshot = saved; scanPreview = nil; isPartialMap = false
                    restoreRememberedFolder(session, path: route, snapshot: saved)
                    guard generation == scanID, activeSessionID == session.id else { return }
                }
                return
            }
            guard let worker = session.worker else { return }
            isScanning = session.preview != nil; isRefreshing = session.preview == nil; updatesPaused = false
            scanPath = session.url.path
            if let progress = session.progress.read() { progressCount = progress.itemCount; progressBytes = progress.bytesScanned; scanPath = progress.currentPath }
            observeSession(worker, scanID: scanID, token: session.token)
            if let preview = session.preview {
                guard beginPreview(preview, rootURL: url, path: route, scanID: scanID, token: session.token) else { return }
            } else if let saved = session.savedSnapshot {
                snapshot = saved; isPartialMap = false; requestedFolderPath = route; isOpeningFolder = true
                restoreRememberedFolder(session, path: route, snapshot: saved)
                guard generation == scanID, activeSessionID == session.id else { return }
                let store = store, colors = colorPreparationState, removals = session.removals
                Task { [weak self] in
                    let view = try? await Task.detached { try indexedFolder(route, store: store, snapshot: saved, colors: colors, cancellation: session.token, removals: removals) }.value
                    guard let self, self.generation == scanID, self.isRefreshing, !session.token.isCancelled,
                          self.requestedFolderPath?.utf8.elementsEqual(route.utf8) == true, let view else { return }
                    guard self.publish(view, snapshot: saved) else { return }
                }
            }
            return
        }
        let previousWorker = prior?.worker
        let previousPreview = prior?.preview
        if let prior { prior.token.cancel(); prior.worker?.cancel(); sessions.removeValue(forKey: url.path) }
        let token = ScanCancellation()
        scanCancellation = token
        let keepMap = sameRoot && snapshot != nil && !isPartialMap
        isScanning = !keepMap
        isRefreshing = keepMap
        updatesPaused = false
        scanTitle = force ? "Mapping your files…" : "Opening saved map…"
        scanPath = url.path
        progressCount = 0; progressBytes = 0
        errorMessage = nil
        statusMessage = keepMap ? "Refreshing the full map…" : nil
        if !keepMap && query == queryAtRequest { query = "" }
        requestedFolderPath = desiredPath
        let store = store
        let stagingDirectory = stagingDirectory
        let observer = scanProgressObserver
        #if ORBIT_TESTING
        let scanner = testScanner
        let scannerObserver = testBeforeScannerStart
        let stageAttachObserver = testAfterStageAttach
        let stageStepObserver = testAfterStageStep
        let stageConfigurer = testConfigureStage
        #else
        let scanner = DiskScanner()
        let scannerObserver: @Sendable () -> Void = {}
        let stageAttachObserver: @Sendable () -> Void = {}
        let stageStepObserver: @Sendable (ResumableStage) -> Void = { _ in }
        let stageConfigurer: @Sendable (ResumableStage) -> Void = { _ in }
        #endif
        let preview = keepMap ? nil : DiskScanPreview()
        let session = MappingSession(url: url, identity: identity, token: token, preview: preview)
        if let prior, prior.identity == identity, Data(prior.url.path.utf8) == Data(url.path.utf8) {
            session.collectionValidation = prior.collectionValidation
            prior.collectionValidation = nil
            session.collectionState = prior.collectionState
            session.isRemoving = prior.isRemoving; session.removalParents = prior.removalParents
            session.pendingDirectories = prior.pendingDirectories; session.pendingCheckpoint = prior.pendingCheckpoint
            session.pendingFullRefresh = prior.pendingFullRefresh
            session.removalVersions = prior.removalVersions; session.removals = prior.removals
            session.removalEpoch = prior.removalEpoch; session.removalStatus = prior.removalStatus
            session.rememberedFolder = prior.rememberedFolder
            if !force, let progress = prior.progress.read() {
                session.progress.set(progress)
                progressCount = progress.itemCount; progressBytes = progress.bytesScanned; scanPath = progress.currentPath
            }
            session.removalError = prior.removalError
            session.collectionStatus = prior.collectionStatus
            session.collectionError = prior.collectionError
            session.presentedCollectionError = prior.presentedCollectionError
            session.collectionStatusRevision = prior.collectionStatusRevision
            session.collectionErrorRevision = prior.collectionErrorRevision
        }
        collectionState = session.collectionState.renewed()
        isRemoving = session.isRemoving
        pendingDirectories.formUnion(session.removalParents)
        pendingDirectories.formUnion(session.pendingDirectories); session.pendingDirectories.removeAll()
        pendingCheckpoint = session.pendingCheckpoint; session.pendingCheckpoint = nil
        pendingFullRefresh = session.pendingFullRefresh; session.pendingFullRefresh = false
        showCollection = false
        if keepMap { session.savedSnapshot = snapshot; session.lastRoute = desiredPath }
        if let prior, prior.collectionValidation != nil {
            prior.collectionValidation?.token.cancel()
            prior.collectionValidation?.task?.cancel()
            prior.collectionValidation = nil
        }
        sessions[url.path] = session
        activeSessionID = session.id
        attachCollectionValidation()
        let sessionID = session.id, progressCell = session.progress
        let colors = colorPreparationState
        let worker = Task.detached(priority: .userInitiated) { [weak self] in
            guard let model = self else { throw CancellationError() }
            do {
            if let previousWorker { _ = try? await previousWorker.value }
            previousPreview?.detachStage()
            if token.isCancelled { throw CancellationError() }
            try FileManager.default.createDirectory(at: store.directory, withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            try FileManager.default.createDirectory(at: store.lockDirectory, withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            try FileManager.default.createDirectory(at: stagingDirectory, withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            var savedCursor: RemovalCursor?
            if let saved = store.savedSnapshot(for: url) { savedCursor = try store.removalCursor(snapshot: saved, cancellation: token) }
            else { savedCursor = nil }
            let journal = try RemovalJournal(cache: store.directory)
            func recoverJournal() throws -> RemovalRecovery {
                do { return try journal.recover(root: url, cursor: savedCursor, cancellation: token) }
                catch RemovalRecoveryError.journalChanged {
                    let fresh = try journal.recover(root: url, cursor: nil, cancellation: token)
                    return RemovalRecovery(cursor: fresh.cursor, removals: fresh.removals,
                                           affectedParents: fresh.affectedParents, overlayOverflow: fresh.overlayOverflow,
                                           hasLiveOperation: fresh.hasLiveOperation, requiresFullRefresh: true)
                }
            }
            var recoveryEpoch = await MainActor.run { session.removalEpoch }
            var recovery = try recoverJournal()
            if recovery.overlayOverflow, !recovery.requiresFullRefresh, let saved = store.savedSnapshot(for: url) {
                var workingSaved = try store.recoveryWorkingCopy(of: saved, cancellation: token)
                let workingFile = workingSaved
                defer { store.discardRecoveryCopy(workingFile) }
                await MainActor.run {
                    guard model.activeSessionID == session.id, model.sessions[url.path] === session else { return }
                    session.durablePreviewBlocked = true
                    guard model.clearPresentation() else { return }
                    model.statusMessage = "Updating affected folders…"
                }
                while true {
                    if token.isCancelled { throw CancellationError() }
                    if recovery.hasLiveOperation {
                        try await Task.sleep(nanoseconds: 100_000_000)
                        recovery = try journal.recover(root: url, cursor: savedCursor, cancellation: token)
                        continue
                    }
                    if recovery.requiresFullRefresh { break }
                    do {
                        let paths = try refreshDirectories(recovery.affectedParents, store: store, snapshot: workingSaved, cancellation: token)
                        for path in paths {
                            if token.isCancelled { throw CancellationError() }
                            let result: ScanResult?
                            do {
                                result = try DiskScanner().scan(URL(fileURLWithPath: path), cancellation: token,
                                                                excluding: [store.directory, store.lockDirectory, stagingDirectory])
                            } catch ScanError.excluded { result = nil }
                            catch ScanError.filesystem(_, let code) where code == ENOENT || code == ENOTDIR { result = nil }
                            workingSaved = try store.update(snapshot: workingSaved, replacements: [IndexReplacement(path: path, result: result)],
                                                            eventID: saved.eventID, eventIdentity: saved.eventIdentity, cancellation: token,
                                                            journalFenceCursor: recovery.cursor)
                        }
                        workingSaved = try store.update(snapshot: workingSaved, replacements: [], eventID: saved.eventID,
                                                        eventIdentity: saved.eventIdentity, cancellation: token, removalCursor: recovery.cursor)
                        _ = try store.publishRecoveryCopy(workingSaved, replacing: saved, cursor: recovery.cursor, cancellation: token)
                        savedCursor = recovery.cursor
                        recovery = RemovalRecovery(cursor: recovery.cursor, removals: [], affectedParents: [], overlayOverflow: false,
                                                   hasLiveOperation: false, requiresFullRefresh: false)
                        await MainActor.run { if model.sessions[url.path] === session { session.durablePreviewBlocked = false } }
                        break
                    } catch RemovalRecoveryError.changedDuringReconciliation {
                        recovery = try journal.recover(root: url, cursor: savedCursor, cancellation: token)
                    }
                }
            }
            while true {
                let prior = await MainActor.run { (session.removals, session.removalEpoch, session.durableRecoveryTarget) }
                let conflictingJournal = prior.2.map { $0.identity != recovery.cursor.identity } ?? false
                if prior.1 != recoveryEpoch || (conflictingJournal && !recovery.requiresFullRefresh) {
                    recovery = try recoverJournal()
                    recoveryEpoch = prior.1
                    if let target = prior.2, target.identity != recovery.cursor.identity {
                        recovery = RemovalRecovery(cursor: recovery.cursor, removals: recovery.removals,
                            affectedParents: recovery.affectedParents, overlayOverflow: recovery.overlayOverflow,
                            hasLiveOperation: recovery.hasLiveOperation, requiresFullRefresh: true)
                    }
                    continue
                }
                let merged = mergingRemovals(prior.0, recovery.removals)
                let installed = await MainActor.run { [recovery, savedCursor] () -> Bool in
                    guard model.sessions[url.path] === session, !token.isCancelled else { return true }
                    guard session.removalEpoch == prior.1 else { return false }
                    model.objectWillChange.send()
                    guard model.sessions[url.path] === session, !token.isCancelled else { return true }
                    guard session.removalEpoch == prior.1 else { return false }
                    if recovery.hasLiveOperation {
                        let sequence = savedCursor?.identity == recovery.cursor.identity ? (savedCursor?.sequence ?? 0) : 0
                        session.durableRecoveryCursor = RemovalCursor(identity: recovery.cursor.identity, sequence: sequence)
                    } else { session.durableRecoveryCursor = recovery.cursor }
                    session.includeRemovalTarget(recovery.cursor)
                    if recovery.requiresFullRefresh { session.durableRecoveryConflict = false }
                    if model.activeSessionID == session.id { model.beginRemovalJournalMonitoring(session) }
                    guard !recovery.removals.isEmpty || recovery.requiresFullRefresh else { return true }
                    session.removalEpoch &+= 1
                    session.removals = merged.items
                    session.durablePreviewBlocked = session.durablePreviewBlocked || merged.overflow || recovery.overlayOverflow
                    let combinedParents = session.removalParents.union(recovery.affectedParents)
                    if combinedParents.count > model.recoveryParentLimit || recovery.requiresFullRefresh {
                        session.pendingFullRefresh = true
                        session.durablePreviewBlocked = true
                    } else {
                        session.removalParents = combinedParents
                        for parent in recovery.affectedParents { session.removalVersions[parent] = session.removalEpoch }
                    }
                    session.durableRecoveryPending = true
                    return true
                }
                if installed { break }
                if token.isCancelled { throw CancellationError() }
            }
            let hasStage = ResumableStage.hasCandidate(cache: store.directory, stagingDirectory: stagingDirectory, root: url)
            var existing: (IndexSnapshot, PreparedFolder)?
            if !recovery.requiresFullRefresh, let saved = store.savedSnapshot(for: url) {
                do {
                    let view = try indexedFolder(desiredPath, store: store, snapshot: saved, colors: colors, cancellation: token, removals: recovery.removals)
                    if let identity = view.root.fileIdentity, currentFileIdentity(url) == identity { existing = (saved, view) }
                } catch {
                    if token.isCancelled { throw CancellationError() }
                }
            }
            if !force, !hasStage, let existing { return (existing.0, existing.1, true) }
            if let existing, hasStage, !keepMap, !force {
                await MainActor.run {
                    guard model.sessions[url.path] === session else { return }
                    session.savedSnapshot = existing.0
                    session.preview = nil
                    guard model.activeSessionID == sessionID, !token.isCancelled,
                          model.requestedFolderPath?.utf8.elementsEqual(desiredPath.utf8) == true else { return }
                    model.snapshot = existing.0; model.isPartialMap = false
                    model.isScanning = false; model.isRefreshing = true
                    model.publish(existing.1, snapshot: existing.0)
                }
            }
            if token.isCancelled { throw CancellationError() }
            scannerObserver()
            if token.isCancelled { throw CancellationError() }
            let current = try DiskChangeMonitor.capture(root: url)
            let checkpoint = StageCheckpoint(rootPath: current.rootPath, identity: current.identity, eventID: current.eventID)
            let started = ProcessInfo.processInfo.systemUptime
            var stage: ResumableStage?
            do {
                stage = try ResumableStage(cache: store.directory, stagingDirectory: stagingDirectory, root: url, current: checkpoint, cancellation: token)
            } catch ResumableStage.Failure.alreadyPublished(let id) {
                if !force, let existing { return (existing.0, existing.1, true) }
                try ResumableStage.retirePublished(cache: store.directory, stagingDirectory: stagingDirectory, root: url, id: id, cancellation: token)
                stage = try ResumableStage(cache: store.directory, stagingDirectory: stagingDirectory, root: url, current: checkpoint, cancellation: token)
            } catch ResumableStage.Failure.invalidated {
                try ResumableStage.retireInvalidated(cache: store.directory, stagingDirectory: stagingDirectory, root: url, cancellation: token)
                stage = try ResumableStage(cache: store.directory, stagingDirectory: stagingDirectory, root: url, current: checkpoint, cancellation: token)
            }
            if let stage {
                if !force {
                    stage.configureReuse { directory, checkpoint in
                        guard let saved = store.savedSnapshot(for: directory, waitForLocks: false) else { return nil }
                        return try DiskChangeMonitor.reuseCandidate(saved, for: checkpoint)
                    }
                }
                #if ORBIT_TESTING
                stage.configureTestScanner(scanner)
                #endif
                stageConfigurer(stage)
                let reportProgress: @Sendable (ScanProgress) -> Void = { [weak model] progress in
                    progressCell.set(progress)
                    Task { @MainActor [weak model] in
                        guard let model, model.activeSessionID == sessionID, !token.isCancelled else { return }
                        model.progressCount = progress.itemCount
                        model.progressBytes = progress.bytesScanned
                        model.scanPath = progress.currentPath
                    }
                }
                try stage.startDiscovery(progress: reportProgress)
                preview?.attach(stage: stage)
                await MainActor.run {
                    guard model.activeSessionID == sessionID, !token.isCancelled else { return }
                    model.scanTitle = "Mapping your files…"
                    if let preview, session.preview != nil, model.scanPreview !== preview {
                        model.beginPreview(preview, rootURL: url, path: desiredPath, scanID: model.generation, token: token)
                    }
                }
                stageAttachObserver()
                try await withTaskCancellationHandler(operation: {
                    try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                        DispatchQueue.global(qos: .userInitiated).async {
                            do {
                                while try stage.advance(progress: { progress in
                                    observer(progress)
                                    reportProgress(progress)
                                }) { stageStepObserver(stage) }
                                continuation.resume()
                            } catch { continuation.resume(throwing: error) }
                        }
                    }
                }, onCancel: { token.cancel() })
                if let preview { await model.readPreview(preview, scanID: await model.generation, token: token, force: true) }
                let saved = try stage.publish(elapsed: ProcessInfo.processInfo.systemUptime - started)
                return (saved, try indexedFolder(desiredPath, store: store, snapshot: saved, colors: colors, cancellation: token), false)
            }
            throw DiskIndexError.invalid("The resumable mapping stage is unavailable.")
            } catch {
                await MainActor.run {
                    if model.sessions[url.path] === session { session.scanFailed = true }
                }
                throw error
            }
        }
        session.worker = worker
        Task { [weak self, weak session] in
            guard let result = try? await worker.value, let self, let session,
                  self.sessions[url.path] === session, !token.isCancelled else { return }
            session.savedSnapshot = result.0
            session.finished = true
            if session.removals.isEmpty && !session.isRemoving { session.preview = nil }
            session.worker = nil
        }
        observeSession(worker, scanID: scanID, token: token)
        if let preview, activeSessionID == session.id, generation == scanID, scanPreview !== preview {
            guard beginPreview(preview, rootURL: url, path: desiredPath, scanID: scanID, token: token) else { return }
        }
    }

    private func observeSession(_ worker: Task<(IndexSnapshot, PreparedFolder, Bool), Error>, scanID: UUID, token: ScanCancellation) {
        workCompletion = Task { [weak self] in
            do {
                let (saved, view, reused) = try await worker.value
                guard let self, self.generation == scanID, !token.isCancelled else { return }
                let owner = self.activeSessionID
                let session = self.sessions.values.first { $0.id == owner }
                let cursorReadEpoch = session?.removalEpoch ?? 0
                let store = self.store
                #if ORBIT_TESTING
                let cursorReadObserver = self.testAfterPublishedRemovalCursorRead
                #endif
                let publishedCursor = try? await Task.detached(priority: .utility) {
                    let cursor = try store.removalCursor(snapshot: saved, cancellation: token)
                    #if ORBIT_TESTING
                    cursorReadObserver()
                    #endif
                    return cursor
                }.value
                guard self.generation == scanID, self.activeSessionID == owner, !token.isCancelled else { return }
                if let session, let publishedCursor {
                    self.retireRemovalState(through: publishedCursor, session: session, epoch: cursorReadEpoch, scanID: scanID)
                }
                guard self.generation == scanID, self.activeSessionID == owner, !token.isCancelled else { return }
                let observedEpoch = session?.removalEpoch ?? 0
                let target = session?.durableRecoveryTarget
                let covered = session?.durableRecoveryConflict != true && (target == nil ||
                    (publishedCursor?.identity == target?.identity && (publishedCursor?.sequence ?? -1) >= (target?.sequence ?? 0)))
                if session?.durablePreviewBlocked == true && !covered {
                    self.snapshot = saved; self.isScanning = false; self.isRefreshing = false
                    self.pendingDirectories.formUnion(session?.removalParents ?? [])
                    self.scheduleRefresh()
                    return
                }
                if let target = self.requestedTarget, self.requestedFolderPath == target.path,
                   view.folder.id == target.path && view.folder.fileIdentity != target.identity {
                    self.requestedTarget = nil; self.scan(URL(fileURLWithPath: target.path), force: true); return
                }
                if self.mapInteractionActive {
                    self.deferredScanCompletion = { [weak self] in
                        guard let self, self.generation == scanID, !token.isCancelled, self.activeSessionID == owner else { return }
                        if (session?.removalEpoch ?? 0) != observedEpoch {
                            self.snapshot = saved; self.isScanning = false; self.isRefreshing = false
                            self.pendingDirectories.formUnion(session?.removalParents ?? [])
                            self.scheduleRefresh()
                            return
                        }
                        self.finishScan(saved, view: view, reused: reused)
                    }
                } else { self.finishScan(saved, view: view, reused: reused) }
            } catch {
                guard let self, self.generation == scanID, !token.isCancelled else { return }
                let owner = self.activeSessionID, resolution = self.resolutionID
                let rootPath = self.root.map { Data($0.id.utf8) }, rootIdentity = self.root?.fileIdentity
                let errorRevision = self.errorMessageRevision, statusRevision = self.statusMessageRevision
                let pausedRevision = self.updatesPausedRevision
                guard !self.atomicFolderInstall else { return }
                self.objectWillChange.send()
                guard !self.atomicFolderInstall, self.generation == scanID, !token.isCancelled,
                      self.activeSessionID == owner, self.resolutionID == resolution,
                      self.root.map({ Data($0.id.utf8) }) == rootPath, self.root?.fileIdentity == rootIdentity else { return }
                self.previewTask?.cancel(); self.previewTask = nil
                self.atomicFolderInstall = true
                defer { self.atomicFolderInstall = false }
                self.isScanning = false; self.isRefreshing = false
                if self.resolutionWorker == nil { self.isOpeningFolder = false }
                if self.errorMessageRevision == errorRevision { self.errorMessage = error.localizedDescription }
                if self.updatesPausedRevision == pausedRevision { self.updatesPaused = self.snapshot != nil || self.isPartialMap }
                if self.isPartialMap && self.statusMessageRevision == statusRevision { self.statusMessage = "Partial map kept. Restart mapping to finish." }
            }
        }
    }

    private func retireRemovalState(through cursor: RemovalCursor, session: MappingSession, epoch: UInt64, scanID: UUID) {
        func canRetire() -> Bool {
            guard generation == scanID, activeSessionID == session.id, sessions[session.url.path] === session,
                  session.removalEpoch == epoch, !session.isRemoving, !session.durableRecoveryConflict,
                  let target = session.durableRecoveryTarget else { return false }
            return cursor.identity == target.identity && cursor.sequence >= target.sequence
        }
        guard canRetire() else { return }
        objectWillChange.send()
        guard canRetire() else { return }
        session.removals.removeAll(); session.removalParents.removeAll(); session.removalVersions.removeAll()
        session.durableRecoveryPending = false; session.durablePreviewBlocked = false
        if session.durableRecoveryCursor?.identity != cursor.identity || (session.durableRecoveryCursor?.sequence ?? -1) < cursor.sequence {
            session.durableRecoveryCursor = cursor
            session.includeRemovalTarget(cursor)
            beginRemovalJournalMonitoring(session)
        }
    }

    private func finishScan(_ saved: IndexSnapshot, view: PreparedFolder, reused: Bool) {
        let owner = activeSessionID, scanID = generation, requestID = resolutionID
        let session = sessions.values.first { $0.id == owner }
        let epoch = session?.removalEpoch ?? 0
        func currentOperation() -> Bool {
            activeSessionID == owner && generation == scanID && resolutionID == requestID && (session?.removalEpoch ?? 0) == epoch
        }
        func retryHandoff() {
            Task { [weak self] in
                do { try await Task.sleep(nanoseconds: 30_000_000) } catch { return }
                guard let self, self.activeSessionID == owner, self.generation == scanID, self.resolutionID == requestID else { return }
                self.finishScan(saved, view: view, reused: reused)
            }
        }
        if let session, !session.removals.isEmpty {
            guard presentationTransition("finishPending", {
                snapshot = saved; isScanning = false; isRefreshing = false
                pendingDirectories.formUnion(session.removalParents)
                statusMessage = session.isRemoving ? session.removalStatus : (session.removalStatus.map { $0 + " Updating the map…" } ?? "Updating the map after removal…")
                if scanPreview == nil { isPartialMap = false }
            }), currentOperation() else { retryHandoff(); return }
            if scanPreview == nil { loadFolder(requestedFolderPath ?? saved.rootURL.path, keepingSelection: selectedID) }
            guard currentOperation() else { retryHandoff(); return }
            beginMonitoring(saved)
            guard currentOperation() else { retryHandoff(); return }
            scheduleRefresh()
            return
        }
        let preservesRoute = root?.id == saved.rootURL.path
        let requested = requestedFolderPath.flatMap { isWithin($0, root: saved.rootURL.path) ? $0 : nil }
        let destination = requested ?? (preservesRoute ? current?.id : nil)
        let selection = preservesRoute ? selectedID : nil
        let opensDifferentRoute = destination.map { Data($0.utf8) != Data(view.folder.id.utf8) } ?? false
        if let destination, opensDifferentRoute {
            let browseID = browseGeneration, colors = colorPreparationState, removals = session?.removals ?? []
            let store = store, token = session?.token ?? ScanCancellation()
            #if ORBIT_TESTING
            let testRouteLoad = testBeforeFinishRouteLoad
            #endif
            guard presentationTransition("finishRoute", { isOpeningFolder = true }), currentOperation() else { retryHandoff(); return }
            Task { [weak self, weak session] in
                do {
                    let prepared = try await Task.detached(priority: .userInitiated) {
                        #if ORBIT_TESTING
                        testRouteLoad()
                        #endif
                        return try indexedFolder(destination, store: store, snapshot: saved, colors: colors, cancellation: token, removals: removals)
                    }.value
                    guard let self, !token.isCancelled, self.activeSessionID == owner, self.generation == scanID,
                          self.resolutionID == requestID else { return }
                    if self.browseGeneration != browseID || session?.removalEpoch ?? 0 != epoch {
                        self.finishScan(saved, view: view, reused: reused)
                        return
                    }
                    if let target = self.requestedTarget, Data(target.path.utf8) == Data(destination.utf8) {
                        let removed = session?.removals.contains { containsBytes(target.path, root: $0.path) } == true
                        if removed { self.requestedTarget = nil }
                        else if Data(prepared.folder.id.utf8) != Data(target.path.utf8) || prepared.folder.fileIdentity != target.identity {
                            self.requestedTarget = nil
                            self.scan(URL(fileURLWithPath: target.path), force: true)
                            return
                        } else { self.requestedTarget = nil }
                    }
                    self.requestedFolderPath = prepared.folder.id
                    self.finishScan(saved, view: prepared, reused: reused)
                } catch {
                    guard let self, self.activeSessionID == owner, self.generation == scanID,
                          self.resolutionID == requestID else { return }
                    if self.browseGeneration != browseID || session?.removalEpoch ?? 0 != epoch {
                        self.finishScan(saved, view: view, reused: reused)
                        return
                    }
                    self.isOpeningFolder = false
                    self.errorMessage = error.localizedDescription
                }
            }
            return
        }
        guard publish(view, snapshot: saved, selection: selection, completedScan: reused) else {
            retryHandoff()
            return
        }
        guard currentOperation() else { retryHandoff(); return }
        previewTask?.cancel(); previewTask = nil
        previewPreparationWorker?.cancel(); previewPreparationWorker = nil
        guard currentOperation() else { retryHandoff(); return }
        previewPreparationID = UUID()
        deferredPreview = nil; deferredScanCompletion = nil
        preferences.set(saved.rootURL.path, forKey: "lastIndexedRoot")
        beginMonitoring(saved)
        guard currentOperation() else { retryHandoff(); return }
        scheduleRefresh(); refreshVolumes()
    }

    @discardableResult
    private func restoreRememberedFolder(_ session: MappingSession, path: String, snapshot: IndexSnapshot? = nil) -> Bool {
        guard activeSessionID == session.id, !session.durablePreviewBlocked,
              let remembered = session.rememberedFolder, remembered.removalEpoch == session.removalEpoch,
              remembered.folder.folder.id.utf8.elementsEqual(path.utf8),
              remembered.folder.root.id.utf8.elementsEqual(session.url.path.utf8),
              remembered.folder.root.fileIdentity == session.identity,
              requestedTarget.map({ !$0.path.utf8.elementsEqual(path.utf8) || $0.identity == remembered.folder.folder.fileIdentity }) ?? true else {
            if root != nil, !displayedRootMatchesActiveSession { _ = clearPresentation(clearSelection: true) }
            return false
        }
        return installFolder(remembered.folder, selection: remembered.selection, publishedSnapshot: snapshot)
    }

    @discardableResult
    private func beginPreview(_ preview: DiskScanPreview, rootURL: URL, path: String,
                              scanID: UUID, token: ScanCancellation) -> Bool {
        guard generation == scanID, !token.isCancelled else { return false }
        preview.prioritize(path: path)
        guard presentationTransition("beginPreview", {
            if root?.id != rootURL.path || root?.fileIdentity != sessions[rootURL.path]?.identity {
                root = nil; navigation = []; selectedID = nil
                nodesByID.removeAll(); searchNodesByID.removeAll(); childColorIndices.removeAll(); colorMappingRevision = UUID()
            }
            snapshot = nil; scanResult = nil; indexedAt = nil; isSavedMap = false
            folderCache.removeAll(); folderOrder.removeAll()
            isPartialMap = true
            scanPreview = preview
            requestedFolderPath = path
            lastPreviewRevision = nil; lastPreviewPath = nil
        }) else { return false }
        if let session = sessions[rootURL.path], session.id == activeSessionID {
            restoreRememberedFolder(session, path: path)
            guard generation == scanID, activeSessionID == session.id, !token.isCancelled else { return false }
        }
        previewTask = Task { [weak self] in
            for await _ in preview.updates {
                guard !Task.isCancelled, !token.isCancelled else { return }
                guard let self, self.generation == scanID, self.scanPreview === preview else { return }
                await self.readPreview(preview, scanID: scanID, token: token)
            }
        }
        return true
    }

    private func readPreview(_ preview: DiskScanPreview, scanID: UUID, token: ScanCancellation, force: Bool = false) async {
        guard generation == scanID, !token.isCancelled, scanPreview === preview else { return }
        if !force && isOpeningFolder && previewPreparationWorker != nil { return }
        let path = requestedFolderPath ?? current?.id
        let browseID = browseGeneration
        let lastRevision = lastPreviewRevision
        let lastPath = lastPreviewPath
        let session = sessions.values.first { $0.id == activeSessionID }
        if let session, let readiness = preview.removalReplayReadiness() {
            let target = session.durableRecoveryTarget
            let covered = target == nil || (readiness.cursor?.identity == target?.identity &&
                                            (readiness.cursor?.sequence ?? -1) >= (target?.sequence ?? 0))
            if readiness.blocked || (session.durablePreviewBlocked && !covered) {
                if !session.durablePreviewBlocked {
                    session.durablePreviewBlocked = true
                    guard clearPresentation(), generation == scanID, activeSessionID == session.id else { return }
                    statusMessage = "Updating affected folders…"
                }
                return
            }
            if session.durablePreviewBlocked && covered { session.durablePreviewBlocked = false }
        }
        guard session?.durablePreviewBlocked != true else { return }
        let removals = session?.removals ?? [], removalEpoch = session?.removalEpoch ?? 0
        let replayGeneration = preview.removalReplayReadiness()?.generation
        let colors = colorPreparationState
        #if ORBIT_TESTING
        let readObserver = testBeforePreviewRead
        #else
        let readObserver: @Sendable () -> Void = {}
        #endif
        previewPreparationWorker?.cancel()
        let preparationID = UUID()
        previewPreparationID = preparationID
        let worker = Task.detached(priority: .userInitiated) { () throws -> PreparedPreview? in
            readObserver()
            if !force, lastRevision == preview.revision, lastPath == path { return nil }
            let view: ScanPreviewView
            if let requested = try preview.view(path: path, cancellation: token, removals: removals) {
                view = requested
            } else if let path, let removed = removals.first(where: { containsBytes(path, root: $0.path) }) {
                var candidate = URL(fileURLWithPath: removed.path).deletingLastPathComponent().path
                var parentView: ScanPreviewView?
                while parentView == nil {
                    parentView = try preview.view(path: candidate, cancellation: token, removals: removals)
                    if candidate == "/" { break }
                    candidate = URL(fileURLWithPath: candidate).deletingLastPathComponent().path
                }
                guard let parentView else { return nil }
                view = parentView
            } else if force || (path.map { preview.confirmsMissingDirectory($0) } ?? false),
                      let rootView = try preview.view(cancellation: token, removals: removals) {
                view = rootView
            } else {
                return nil
            }
            let folder = IndexedFolder(root: view.root, folder: view.folder, ancestors: view.ancestors)
            return PreparedPreview(preview: view, folder: try prepareFolder(folder, colors: colors, cancellation: token), replayGeneration: replayGeneration)
        }
        previewPreparationWorker = worker
        let view = try? await worker.value
        guard previewPreparationID == preparationID else { return }
        previewPreparationWorker = nil
        guard !Task.isCancelled, !token.isCancelled, generation == scanID, browseGeneration == browseID,
              scanPreview === preview, session?.removalEpoch ?? 0 == removalEpoch,
              preview.removalReplayReadiness()?.generation == replayGeneration,
              preview.removalReplayReadiness()?.blocked != true, let view else { return }
        if let target = requestedTarget, requestedFolderPath == target.path {
            if view.folder.folder.id == target.path {
                guard view.folder.folder.fileIdentity == target.identity else {
                    requestedTarget = nil; scan(URL(fileURLWithPath: target.path), force: true); return
                }
                requestedTarget = nil
            } else if removals.contains(where: { containsBytes(target.path, root: $0.path) }) { requestedTarget = nil }
        }
        receivePreview(view, scanID: scanID, browseID: browseID)
    }

    private func receivePreview(_ view: PreparedPreview, scanID: UUID, browseID: UUID) {
        guard generation == scanID, browseGeneration == browseID, isPartialMap else { return }
        guard scanPreview?.removalReplayReadiness()?.generation == view.replayGeneration,
              scanPreview?.removalReplayReadiness()?.blocked != true,
              sessions.values.first(where: { $0.id == activeSessionID })?.durablePreviewBlocked != true else { return }
        if lastPreviewPath == view.preview.folder.id, let revision = lastPreviewRevision {
            if revision > view.preview.revision || (revision == view.preview.revision && !isOpeningFolder) { return }
        }
        lastPreviewRevision = view.preview.revision; lastPreviewPath = view.preview.folder.id
        lastPreviewIsListing = view.preview.isListing
        if mapInteractionActive {
            deferredPreview = (view, scanID, browseID)
            return
        }
        publishPreview(view)
    }

    private func publishPreview(_ view: PreparedPreview) {
        guard installFolder(view.folder, selection: selectedID, retainingPendingSelection: true) else { return }
        if let preview = scanPreview {
            let state = preview.removalReplayReadiness()
            if state?.generation != view.replayGeneration || state?.blocked == true {
                let scanID = generation
                let session = sessions.values.first { $0.id == activeSessionID }
                let token = session?.token.isCancelled == false ? session!.token : ScanCancellation()
                Task { [weak self, weak preview] in
                    guard let self, let preview, self.generation == scanID, self.scanPreview === preview else { return }
                    await self.readPreview(preview, scanID: scanID, token: token, force: true)
                }
            }
        }
        if isFiltering && searchTask == nil { search() }
    }

    func beginMapInteraction() -> () -> Void {
        let owner = UUID()
        mapInteractionOwner = owner
        setMapInteractionActive(true)
        return { [weak self] in
            guard let self, self.mapInteractionOwner == owner else { return }
            self.mapInteractionOwner = nil
            self.setMapInteractionActive(false)
        }
    }

    func setMapInteractionActive(_ active: Bool) {
        mapInteractionActive = active
        guard !active else { return }
        if let completion = deferredScanCompletion, !isRemoving {
            deferredScanCompletion = nil
            completion()
        } else if let pending = deferredPreview {
            deferredPreview = nil
            if generation == pending.scanID && browseGeneration == pending.browseID && isPartialMap,
               scanPreview?.removalReplayReadiness()?.generation == pending.view.replayGeneration,
               scanPreview?.removalReplayReadiness()?.blocked != true,
               sessions.values.first(where: { $0.id == activeSessionID })?.durablePreviewBlocked != true {
                publishPreview(pending.view)
            }
        }
        guard !mapInteractionActive else { return }
        if let publishFolder = deferredIndexedFolder {
            deferredIndexedFolder = nil
            publishFolder()
        }
        guard !mapInteractionActive else { return }
        if let pending = deferredNameSort {
            deferredNameSort = nil
            finishNameSort(nodes: pending.nodes, items: pending.items, indices: pending.indices, revision: pending.revision)
        }
    }

    private func stopWork(preserveScan: Bool = false, preserveCollection: Bool = false) {
        previewTask?.cancel(); previewTask = nil
        previewPreparationWorker?.cancel(); previewPreparationWorker = nil
        previewPreparationID = UUID()
        folderPreparationWorker?.cancel(); folderPreparationWorker = nil
        folderPreparationCancellation?.cancel(); folderPreparationCancellation = nil
        deferredPreview = nil; deferredScanCompletion = nil; deferredIndexedFolder = nil
        mapInteractionActive = false; mapInteractionOwner = nil
        if let pending = deferredNameSort {
            deferredNameSort = nil
            finishNameSort(nodes: pending.nodes, items: pending.items, indices: pending.indices, revision: pending.revision)
        }
        if preserveCollection { detachCollectionValidation() } else { cancelCollectionValidation() }
        removalResultStatus = nil
        if !preserveScan { scanCancellation?.cancel() }
        activeSessionID = nil
        searchCancellation?.cancel(); searchTask?.cancel(); searchTask = nil
        isSearching = false
        refreshTask?.cancel(); refreshTask = nil
        monitorTask?.cancel(); monitorTask = nil
        removalJournalTask?.cancel(); removalJournalTask = nil
        monitor?.stop(); monitor = nil
        pendingDirectories.removeAll(); pendingFullRefresh = false; pendingCheckpoint = nil
        browseGeneration = UUID()
        requestedFolderPath = nil
        requestedTarget = nil
        isOpeningFolder = false
    }

    func cancelScan() {
        resolutionID = UUID(); resolutionWorker?.cancel(); resolutionWorker = nil
        let attachedSession = activeSessionID
        stopWork(preserveCollection: true)
        activeSessionID = attachedSession
        attachCollectionValidation()
        generation = UUID()
        if let attachedSession, let session = sessions.values.first(where: { $0.id == attachedSession }) {
            beginRemovalJournalMonitoring(session)
        }
        isScanning = false; isRefreshing = false
        updatesPaused = snapshot != nil || isPartialMap
        statusMessage = isRemoving ? sessions.values.first { $0.id == activeSessionID }?.removalStatus
            : isPartialMap ? "Partial map kept. Restart mapping to finish."
            : snapshot == nil ? "Scan cancelled." : "Updates paused. Refresh to resume."
        if isFiltering { search() }
    }

    func rescan() {
        let fallback = sessions.values.first(where: { $0.id == activeSessionID })?.url
        if let url = root?.url ?? fallback { scan(url, force: true) }
    }

    private func beginRemovalJournalMonitoring(_ session: MappingSession) {
        removalJournalTask?.cancel()
        guard session.durableRecoveryCursor != nil else { return }
        let cache = store.directory, rootURL = session.url
        removalJournalTask = Task { [weak self, weak session] in
            var lastLive: (recovery: RemovalRecovery, epoch: UInt64)?
            while !Task.isCancelled {
                guard let self, let session, self.activeSessionID == session.id,
                      let cursor = session.durableRecoveryCursor else { return }
                let existing = session.removals, epoch = session.removalEpoch
                let previous = lastLive
                let cancellation = ScanCancellation()
                let worker = Task.detached(priority: .utility) {
                    let recovery = try RemovalJournal(cache: cache).recover(root: rootURL, cursor: cursor, cancellation: cancellation)
                    let unchanged = previous.map { $0.epoch == epoch && sameLiveRecovery($0.recovery, recovery) } ?? false
                    return (recovery, unchanged ? nil : mergingRemovals(existing, recovery.removals))
                }
                do {
                    let (recovery, merged) = try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: {
                        cancellation.cancel(); worker.cancel()
                    })
                    guard !Task.isCancelled, self.activeSessionID == session.id,
                          session.durableRecoveryCursor == cursor else { return }
                    if session.removalEpoch != epoch { continue }
                    #if ORBIT_TESTING
                    self.testJournalPollCount += 1
                    #endif
                    guard let merged else {
                        try await Task.sleep(nanoseconds: 1_000_000_000)
                        continue
                    }
                    if !recovery.removals.isEmpty || recovery.requiresFullRefresh {
                        self.objectWillChange.send()
                        guard !Task.isCancelled, self.activeSessionID == session.id,
                              session.durableRecoveryCursor == cursor else { return }
                        if session.removalEpoch != epoch { continue }
                        if !recovery.hasLiveOperation { session.durableRecoveryCursor = recovery.cursor }
                        session.includeRemovalTarget(recovery.cursor)
                        session.removalEpoch &+= 1
                        lastLive = recovery.hasLiveOperation ? (recovery, session.removalEpoch) : nil
                        self.deferredPreview = nil
                        session.removals = merged.items
                        let combinedParents = session.removalParents.union(recovery.affectedParents)
                        let parentsExceeded = combinedParents.count > self.recoveryParentLimit
                        if parentsExceeded || recovery.requiresFullRefresh {
                            session.pendingFullRefresh = true
                            self.pendingFullRefresh = true
                        } else {
                            session.removalParents = combinedParents
                            for parent in recovery.affectedParents { session.removalVersions[parent] = session.removalEpoch }
                            self.pendingDirectories.formUnion(recovery.affectedParents)
                        }
                        session.durableRecoveryPending = true
                        if recovery.overlayOverflow || recovery.requiresFullRefresh || merged.overflow || parentsExceeded {
                            session.durablePreviewBlocked = true
                            let scanID = self.generation
                            guard self.clearPresentation(), self.generation == scanID, self.activeSessionID == session.id else { return }
                            self.statusMessage = "Updating affected folders…"
                        }
                        self.folderCache.removeAll(); self.folderOrder.removeAll()
                        if self.isFiltering { self.search() }
                        guard self.activeSessionID == session.id else { return }
                        if let preview = self.scanPreview, self.isPartialMap {
                            self.lastPreviewRevision = nil
                            let token = session.token.isCancelled ? ScanCancellation() : session.token
                            await self.readPreview(preview, scanID: self.generation, token: token, force: true)
                        } else if let saved = self.snapshot {
                            self.loadFolder(self.requestedFolderPath ?? saved.rootURL.path, keepingSelection: self.selectedID)
                        }
                        self.scheduleRefresh()
                    } else {
                        if !recovery.hasLiveOperation { session.durableRecoveryCursor = recovery.cursor }
                        session.includeRemovalTarget(recovery.cursor)
                    }
                    try await Task.sleep(nanoseconds: 1_000_000_000)
                } catch {
                    guard !Task.isCancelled, self.activeSessionID == session.id else { return }
                    session.durableRecoveryPending = true
                    session.pendingFullRefresh = true
                    self.pendingFullRefresh = true
                    self.scheduleRefresh()
                    return
                }
            }
        }
    }

    private func beginMonitoring(_ saved: IndexSnapshot) {
        let monitorOwner = activeSessionID, monitorGeneration = generation
        let monitorRoot = root.map { Data($0.id.utf8) }, monitorRootIdentity = root?.fileIdentity
        monitorTask?.cancel(); monitorTask = nil
        monitor?.stop(); monitor = nil
        do {
            #if ORBIT_TESTING
            if testMonitorUnavailable { throw DiskIndexError.invalid("Test monitor unavailable") }
            #endif
            let preferences = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Preferences/\(Bundle.main.bundleIdentifier ?? "local.orbitdisk.app").plist")
            let watcher = try DiskChangeMonitor(root: saved.rootURL, excluding: [store.directory, store.lockDirectory, stagingDirectory, preferences])
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
                    self.recordMonitorUnavailable(owner: monitorOwner, generation: monitorGeneration,
                                                  root: monitorRoot, rootIdentity: monitorRootIdentity, watcher: watcher)
                }
            }
        } catch {
            recordMonitorUnavailable(owner: monitorOwner, generation: monitorGeneration,
                                     root: monitorRoot, rootIdentity: monitorRootIdentity, watcher: nil)
        }
    }

    private func recordMonitorUnavailable(owner: UUID?, generation scanID: UUID, root expectedRoot: Data?,
                                          rootIdentity: String?, watcher: DiskChangeMonitor?) {
        func matchesOwner() -> Bool {
            activeSessionID == owner && generation == scanID && (watcher == nil || monitor === watcher)
                && root.map { Data($0.id.utf8) } == expectedRoot && root?.fileIdentity == rootIdentity
        }
        guard matchesOwner() else { return }
        let statusRevision = statusMessageRevision, pausedRevision = updatesPausedRevision
        objectWillChange.send()
        guard matchesOwner() else { return }
        let priorAtomic = atomicFolderInstall
        atomicFolderInstall = true
        if updatesPausedRevision == pausedRevision { updatesPaused = true }
        if statusMessageRevision == statusRevision {
            statusMessage = "Saved map available. Automatic updates unavailable; use Refresh."
        }
        atomicFolderInstall = priorAtomic
    }

    private func scheduleRefresh() {
        let durableRecovery = sessions.values.first(where: { $0.id == activeSessionID })?.durableRecoveryPending == true
        guard !isRefreshing, !isScanning, !isRemoving, (!updatesPaused || durableRecovery),
              refreshTask == nil, pendingFullRefresh || !pendingDirectories.isEmpty else { return }
        refreshTask = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: 1_500_000_000) } catch { return }
            self?.refreshTask = nil
            self?.refreshChangedFolders()
        }
    }

    private func refreshChangedFolders() {
        guard let saved = snapshot, !isRefreshing, !isScanning, !isRemoving else { return }
        if pendingFullRefresh {
            pendingFullRefresh = false; pendingDirectories.removeAll(); pendingCheckpoint = nil
            sessions.values.first(where: { $0.id == activeSessionID })?.pendingFullRefresh = false
            scan(saved.rootURL, force: true); return
        }
        let paths = pendingDirectories
        guard !paths.isEmpty else { return }
        let session = sessions.values.first { $0.id == activeSessionID }
        let refreshVersions = session?.removalVersions ?? [:]
        let refreshRemovalEpoch = session?.removalEpoch ?? 0
        let checkpoint = pendingCheckpoint ?? DiskChangeCheckpoint(rootPath: saved.rootURL.path,
            identity: saved.eventIdentity, eventID: saved.eventID)
        pendingDirectories.removeAll(); pendingCheckpoint = nil
        let token = ScanCancellation()
        scanCancellation = token
        let scanID = generation
        let store = store
        let stagingDirectory = stagingDirectory
        #if ORBIT_TESTING
        let refreshObserver = testBeforeRefreshSave
        #else
        let refreshObserver: @Sendable () -> Void = {}
        #endif
        isRefreshing = true
        statusMessage = removalResultStatus.map { $0 + " Updating the map…" } ?? "Updating changed folders…"
        let worker = Task.detached(priority: .utility) { () throws -> IndexSnapshot in
            let startingCursor = try store.removalCursor(snapshot: saved, cancellation: token)
            let journal = try RemovalJournal(cache: store.directory)
            var workingSaved = try store.recoveryWorkingCopy(of: saved, cancellation: token)
            let workingFile = workingSaved
            defer { store.discardRecoveryCopy(workingFile) }
            while true {
                let recovery = try journal.recover(root: saved.rootURL, cursor: startingCursor, cancellation: token)
                if recovery.hasLiveOperation {
                    if token.isCancelled { throw CancellationError() }
                    try await Task.sleep(nanoseconds: 100_000_000)
                    continue
                }
                var recoveryPaths = paths
                recoveryPaths.formUnion(recovery.affectedParents)
                if recovery.requiresFullRefresh { recoveryPaths = [saved.rootURL.path] }
                do {
                    let minimal = try refreshDirectories(recoveryPaths, store: store, snapshot: workingSaved, cancellation: token)
                    for path in minimal {
                        if token.isCancelled { throw CancellationError() }
                        let result: ScanResult?
                        do {
                            result = try DiskScanner().scan(URL(fileURLWithPath: path), cancellation: token,
                                                            excluding: [store.directory, store.lockDirectory, stagingDirectory])
                        } catch ScanError.excluded { result = nil }
                        catch ScanError.filesystem(_, let code) where code == ENOENT || code == ENOTDIR { result = nil }
                        refreshObserver()
                        workingSaved = try store.update(snapshot: workingSaved, replacements: [IndexReplacement(path: path, result: result)],
                                                        eventID: saved.eventID, eventIdentity: saved.eventIdentity, cancellation: token,
                                                        journalFenceCursor: recovery.cursor)
                    }
                    workingSaved = try store.update(snapshot: workingSaved, replacements: [], eventID: checkpoint.eventID,
                                                    eventIdentity: checkpoint.identity, cancellation: token, removalCursor: recovery.cursor)
                    return try store.publishRecoveryCopy(workingSaved, replacing: saved, cursor: recovery.cursor, cancellation: token)
                } catch RemovalRecoveryError.changedDuringReconciliation {
                    continue
                }
            }
        }
        let refreshID = UUID()
        session?.refreshWorker = worker; session?.refreshWorkerID = refreshID
        Task { [weak session] in
            _ = try? await worker.value
            guard let session, session.refreshWorkerID == refreshID else { return }
            session.refreshWorker = nil; session.refreshWorkerID = nil
        }
        workCompletion = Task { [weak self] in
            do {
                let updated = try await worker.value
                guard let self, self.generation == scanID, !token.isCancelled else { return }
                self.snapshot = updated
                if let session {
                    self.objectWillChange.send()
                    guard self.generation == scanID, !token.isCancelled, self.activeSessionID == session.id else { return }
                    for (path, version) in refreshVersions where session.removalVersions[path] == version {
                        session.removalVersions.removeValue(forKey: path); session.removalParents.remove(path)
                        session.removals.removeAll { URL(fileURLWithPath: $0.path).deletingLastPathComponent().path == path }
                    }
                    session.durableRecoveryPending = session.removalEpoch != refreshRemovalEpoch || !session.removalParents.isEmpty
                    if !session.durableRecoveryPending { session.durablePreviewBlocked = false }
                }
                self.folderCache.removeAll(); self.folderOrder.removeAll()
                self.isRefreshing = false; self.isSavedMap = false
                if self.isPartialMap, session?.removals.isEmpty != false {
                    self.previewTask?.cancel(); self.previewTask = nil; self.scanPreview = nil; self.isPartialMap = false
                    session?.preview = nil
                }
                self.statusMessage = self.removalResultStatus ?? "Updated changed folders."
                self.removalResultStatus = nil
                if !self.isPartialMap {
                    self.loadFolder(self.requestedFolderPath ?? self.current?.id ?? updated.rootURL.path,
                                    keepingSelection: self.selectedID)
                }
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

    @discardableResult
    private func installFolder(_ view: PreparedFolder, selection: String? = nil, retainingPendingSelection: Bool = false, publishedSnapshot: IndexSnapshot? = nil, completedScan: Bool? = nil) -> Bool {
        let owner = activeSessionID, scanID = generation, browseID = browseGeneration, requestID = resolutionID
        let session = sessions.values.first { $0.id == owner }
        let removalEpoch = session?.removalEpoch ?? 0
        let removing = isRemoving, collectionVersion = collectionRevision
        let snapshotID = snapshot?.id, partial = isPartialMap
        let preview = scanPreview, replayGeneration = preview?.removalReplayReadiness()?.generation
        let stateVersion = folderStateVersion
        let sameFolder = current?.id == view.folder.id
        let canUsePreservedColors = sameFolder && view.colorRevision == colorMappingRevision
        let priorItems = displayedItemsCache
        let needsReset = sameFolder && !isFiltering && (priorItems?.count ?? 0) >= 5_000 && view.items.count >= 5_000 &&
            (priorItems!.count != view.items.count || priorItems!.indices.contains(where: { priorItems![$0].id != view.items[$0].id }))
        if needsReset { largeListWillReset?() }
        guard activeSessionID == owner, generation == scanID, browseGeneration == browseID, resolutionID == requestID,
              session?.removalEpoch ?? 0 == removalEpoch, snapshot?.id == snapshotID, isPartialMap == partial,
              isRemoving == removing, collectionRevision == collectionVersion,
              scanPreview === preview, preview?.removalReplayReadiness()?.generation == replayGeneration,
              preview?.removalReplayReadiness()?.blocked != true, folderStateVersion == stateVersion else { return false }
        #if ORBIT_TESTING
        testBeforeRootInstall?()
        if completedScan != nil { testBeforeLifecycleTransition?("finishInstall") }
        #endif
        objectWillChange.send()
        guard activeSessionID == owner, generation == scanID, browseGeneration == browseID, resolutionID == requestID,
              session?.removalEpoch ?? 0 == removalEpoch, snapshot?.id == snapshotID, isPartialMap == partial,
              isRemoving == removing, collectionRevision == collectionVersion,
              scanPreview === preview, preview?.removalReplayReadiness()?.generation == replayGeneration,
              preview?.removalReplayReadiness()?.blocked != true, folderStateVersion == stateVersion else { return false }
        atomicFolderInstall = true
        defer { atomicFolderInstall = false }
        if needsReset { largeListIdentity &+= 1 }
        let preservesHover = current?.id == view.folder.id && root?.id == view.root.id
            && root?.fileIdentity == view.root.fileIdentity
        let oldGroup = preservesHover && !isFiltering && hoveredMapNodes.count >= 1_024 ? preparedMapHoverGroup : nil
        let oldIDs = oldGroup == nil ? nil : mapChildIDs
        let oldSet = oldGroup == nil ? nil : mapChildIDSet
        let hoveredWasCurrentChildren = oldGroup != nil && (current.map { current in
            hoveredMapNodes.count == current.children.count && hoveredMapNodes.withUnsafeBufferPointer { left in
                current.children.withUnsafeBufferPointer { right in left.baseAddress == right.baseAddress }
            }
        } ?? false)
        let hoverRevisionWasCurrent = oldGroup?.mapRevision == mapRevision
        if !preservesHover { clearHover() }
        mapRevision = UUID()
        root = view.root
        navigation = view.ancestors
        mapChildIDs = view.childIDs
        requestedFolderPath = view.folder.id
        if !sameFolder || childColorIndices.isEmpty {
            childColorIndices = view.initialColors
            colorMappingRevision = UUID()
        } else if canUsePreservedColors, let colors = view.preservedColors {
            childColorIndices = colors
            if view.colorMappingExtended { colorMappingRevision = UUID() }
        } else {
            let previousCount = childColorIndices.count
            var nextColor = (childColorIndices.values.max() ?? -1) + 1
            for item in view.items where childColorIndices[item.id] == nil {
                childColorIndices[item.id] = nextColor
                nextColor += 1
            }
            if childColorIndices.count != previousCount { colorMappingRevision = UUID() }
        }
        folderSizeOrder = FolderSizeOrder(nodes: view.folder.children, items: view.items,
                                         indices: view.rowIndices, ids: view.displayedIDs)
        if !isFiltering {
            if !sortByName || isSortingRows { displayedNodesCache = view.folder.children }
            displayedIDsCache = view.displayedIDs
            if !sortByName || isSortingRows { displayedItemsCache = view.items; displayedRowIndices = view.rowIndices }
        }
        nodesByID = view.nodesByID
        searchNodesByID.removeAll(keepingCapacity: true)
        if !searchResults.isEmpty {
            var searchNodes: [String: DiskNode] = [:]
            var pending = searchResults
            while let node = pending.popLast() {
                searchNodes[node.id] = node
                pending.append(contentsOf: node.children)
            }
            for (id, node) in searchNodes where nodesByID[id] == nil { searchNodesByID[id] = node }
        }
        if preservesHover {
            let rootPrefix = view.folder.id == "/" ? "/" : view.folder.id + "/"
            let rootPrefixBytes = Array(rootPrefix.utf8)
            let fastRoot = hoverRevisionWasCurrent && hoveredWasCurrentChildren && hoveredMapNodes.count == oldIDs?.count
                && oldGroup?.parentID == view.folder.id && oldGroup?.rootID == view.folder.id
                && oldGroup?.id == hoveredMapGroupID && oldIDs?.count == view.childIDs.count
                && view.displayedIDs.count == view.childIDs.count && oldSet?.count == view.childIDs.count
                && oldSet == oldGroup?.memberIDs
                && zip(oldIDs ?? [], view.childIDs).allSatisfy { $0.utf8.elementsEqual($1.utf8) }
                && zip(zip(hoveredMapNodes, view.folder.children), view.childIDs).allSatisfy { pair, id in
                    let (old, new) = pair
                    let bytes = id.utf8
                    return old.fileIdentity != nil && old.fileIdentity == new.fileIdentity && new.children.isEmpty
                        && bytes.starts(with: rootPrefixBytes) && bytes.count > rootPrefixBytes.count
                        && !bytes.dropFirst(rootPrefixBytes.count).contains(47)
                }
            let refreshedHover = fastRoot ? view.folder.children : hoveredMapNodes.compactMap { previous -> DiskNode? in
                guard let node = node(withID: previous.id), node.fileIdentity == previous.fileIdentity else { return nil }
                return node
            }
            if refreshedHover.count == hoveredMapNodes.count {
                hoveredMapNodes = refreshedHover
                if fastRoot, let oldGroup {
                    preparedMapHoverGroup = PreparedMapHoverGroup(id: oldGroup.id, parentID: oldGroup.parentID,
                                                                  rootID: oldGroup.rootID, mapRevision: mapRevision,
                                                                  memberIDs: view.displayedIDs)
                }
            } else {
                hoveredMapNodes = []
                hoveredMapGroupID = nil
            }
            if let id = hoveredListID, node(withID: id) == nil { hoveredListID = nil }
        }
        selectedID = selection.flatMap { retainingPendingSelection || node(withID: $0) != nil ? $0 : nil }
        if let saved = publishedSnapshot {
            if let reused = completedScan {
                session?.durablePreviewBlocked = false
                snapshot = saved; isPartialMap = false; scanPreview = nil
                lastPreviewRevision = nil; lastPreviewPath = nil; lastPreviewIsListing = false
                folderCache.removeAll(); folderOrder.removeAll()
                isSavedMap = reused; isScanning = false; isRefreshing = false
                statusMessage = session?.removalStatus
            }
            indexedAt = saved.scannedAt
            scanResult = ScanResult(root: view.root, elapsed: saved.elapsed, skippedCount: saved.skippedCount, issues: saved.issues)
            folderCache[view.folder.id] = view.view
            folderOrder.removeAll { $0 == view.folder.id }; folderOrder.append(view.folder.id)
            while folderOrder.count > 24 { folderCache.removeValue(forKey: folderOrder.removeFirst()) }
        }
        if let session, session.id == activeSessionID,
           root.map({ Data($0.id.utf8) }) == Data(session.url.path.utf8), root?.fileIdentity == session.identity {
            session.rememberedFolder = RememberedFolder(folder: view, removalEpoch: session.removalEpoch, selection: selectedID)
            if let publishedSnapshot { session.savedSnapshot = publishedSnapshot }
            if statusMessage == nil, let status = session.collectionStatus {
                session.collectionStatus = nil
                suppressCollectionFeedbackInvalidation = true
                statusMessage = status
                suppressCollectionFeedbackInvalidation = false
            }
            if errorMessage == nil, let error = session.presentedCollectionError ?? session.collectionError {
                if session.presentedCollectionError == nil {
                    session.presentedCollectionError = error
                    session.collectionError = nil
                }
                suppressCollectionFeedbackInvalidation = true
                errorMessage = error
                suppressCollectionFeedbackInvalidation = false
            }
        }
        isOpeningFolder = false
        return true
    }

    @discardableResult
    private func publish(_ view: PreparedFolder, snapshot saved: IndexSnapshot, selection: String? = nil, completedScan: Bool? = nil) -> Bool {
        guard installFolder(view, selection: selection, publishedSnapshot: saved, completedScan: completedScan) else { return false }
        let owner = activeSessionID, scanID = generation, browseID = browseGeneration, requestID = resolutionID
        let session = sessions.values.first { $0.id == owner }, epoch = session?.removalEpoch ?? 0
        if isFiltering { search() }
        return activeSessionID == owner && generation == scanID && browseGeneration == browseID && resolutionID == requestID &&
            (session?.removalEpoch ?? 0) == epoch
    }

    private func loadFolder(_ path: String, keepingSelection selection: String? = nil) {
        deferredIndexedFolder = nil
        folderPreparationWorker?.cancel(); folderPreparationWorker = nil
        folderPreparationCancellation?.cancel(); folderPreparationCancellation = nil
        previewPreparationWorker?.cancel(); previewPreparationWorker = nil
        previewPreparationID = UUID()
        let colors = colorPreparationState
        let session = sessions.values.first { $0.id == activeSessionID }
        guard session?.durablePreviewBlocked != true else { return }
        let removals = session?.removals ?? [], removalEpoch = session?.removalEpoch ?? 0
        if isPartialMap, let preview = scanPreview {
            preview.prioritize(path: path)
            if path != current?.id { clearHover() }
            requestedFolderPath = path
            browseGeneration = UUID()
            let browseID = browseGeneration
            let scanID = generation
            selectedID = selection
            isOpeningFolder = true
            let preparationID = UUID()
            previewPreparationID = preparationID
            let replayGeneration = preview.removalReplayReadiness()?.generation
            let worker = Task.detached(priority: .userInitiated) { () throws -> PreparedPreview? in
                guard let view = try preview.view(path: path, cancellation: ScanCancellation(), removals: removals) else { return nil }
                let folder = IndexedFolder(root: view.root, folder: view.folder, ancestors: view.ancestors)
                return PreparedPreview(preview: view, folder: try prepareFolder(folder, colors: colors), replayGeneration: replayGeneration)
            }
            previewPreparationWorker = worker
            Task { [weak self] in
                let view = try? await worker.value
                guard let self, self.generation == scanID, self.browseGeneration == browseID,
                      self.previewPreparationID == preparationID, self.scanPreview === preview, self.isPartialMap,
                      preview.removalReplayReadiness()?.generation == replayGeneration,
                      preview.removalReplayReadiness()?.blocked != true, session?.durablePreviewBlocked != true,
                      session?.removalEpoch ?? 0 == removalEpoch else { return }
                self.previewPreparationWorker = nil
                if let view { self.receivePreview(view, scanID: scanID, browseID: browseID) }
                else {
                    self.isOpeningFolder = false
                    if removals.contains(where: { containsBytes(path, root: $0.path) }), let token = self.scanCancellation {
                        Task { await self.readPreview(preview, scanID: scanID, token: token, force: true) }
                    }
                }
            }
            return
        }
        guard let saved = snapshot else { return }
        if path != current?.id { clearHover() }
        requestedFolderPath = path
        browseGeneration = UUID()
        let browseID = browseGeneration
        let cached = removals.isEmpty ? folderCache[path] : nil
        if let cached, cached.folder.children.count < 256,
           let prepared = try? prepareFolder(cached, colors: colors, cancellable: false) {
            receiveIndexedFolder(prepared, snapshot: saved, selection: selection,
                                 scanID: generation, browseID: browseID)
            return
        }
        isOpeningFolder = true
        let store = store
        let scanID = generation
        let cancellation = ScanCancellation()
        folderPreparationCancellation = cancellation
        let worker = Task.detached(priority: .userInitiated) {
            if let cached { return try prepareFolder(cached, colors: colors, cancellation: cancellation) }
            return try indexedFolder(path, store: store, snapshot: saved, colors: colors, cancellation: cancellation, removals: removals)
        }
        folderPreparationWorker = worker
        Task { [weak self] in
            do {
                let view = try await worker.value
                guard let self, self.generation == scanID, self.browseGeneration == browseID else { return }
                self.folderPreparationWorker = nil
                self.folderPreparationCancellation = nil
                guard self.snapshot?.id == saved.id, session?.removalEpoch ?? 0 == removalEpoch else { self.loadFolder(path, keepingSelection: selection); return }
                self.receiveIndexedFolder(view, snapshot: saved, selection: selection,
                                          scanID: scanID, browseID: browseID)
            } catch {
                guard let self, self.generation == scanID, self.browseGeneration == browseID else { return }
                self.folderPreparationWorker = nil
                self.folderPreparationCancellation = nil
                self.isOpeningFolder = false
                if self.adoptLatestSnapshot() || self.snapshot?.id != saved.id { self.loadFolder(path, keepingSelection: selection) }
                else { self.errorMessage = error.localizedDescription }
            }
        }
    }

    private func receiveIndexedFolder(_ view: PreparedFolder, snapshot saved: IndexSnapshot,
                                      selection: String?, scanID: UUID, browseID: UUID) {
        if let target = requestedTarget, requestedFolderPath == target.path {
            let removed = sessions.values.first { $0.id == activeSessionID }?.removals.contains { containsBytes(target.path, root: $0.path) } == true
            if removed { requestedTarget = nil }
            else if view.folder.id != target.path || view.folder.fileIdentity != target.identity {
                requestedTarget = nil; scan(URL(fileURLWithPath: target.path), force: true); return
            } else { requestedTarget = nil }
        }
        if mapInteractionActive && current?.id == view.folder.id {
            let owner = activeSessionID
            let epoch = sessions.values.first(where: { $0.id == owner })?.removalEpoch ?? 0
            deferredIndexedFolder = { [weak self] in
                guard let self, self.generation == scanID, self.browseGeneration == browseID, self.activeSessionID == owner else { return }
                let session = self.sessions.values.first { $0.id == owner }
                if session?.durablePreviewBlocked == true { return }
                if (session?.removalEpoch ?? 0) != epoch {
                    self.loadFolder(view.folder.id, keepingSelection: self.selectedID)
                    return
                }
                guard self.snapshot?.id == saved.id else {
                    self.loadFolder(view.folder.id, keepingSelection: self.selectedID)
                    return
                }
                guard self.publish(view, snapshot: saved, selection: self.selectedID) else { return }
            }
            return
        }
        guard publish(view, snapshot: saved, selection: selection) else { return }
    }

    private func adoptLatestSnapshot() -> Bool {
        guard let saved = snapshot, let latest = store.savedSnapshot(for: saved.rootURL), latest.id != saved.id else { return false }
        snapshot = latest
        folderCache.removeAll(); folderOrder.removeAll()
        return true
    }

    func open(_ node: DiskNode) {
        guard displayedRootMatchesActiveSession else { return }
        if requestedTarget?.path != node.id { requestedTarget = nil }
        guard node.isDirectory, !node.isSymbolicLink else { selectedID = node.id; return }
        query = ""
        loadFolder(node.id)
    }
    func goBack() {
        guard displayedRootMatchesActiveSession else { return }
        requestedTarget = nil
        guard navigation.count > 1, let departed = current else { return }
        query = ""
        loadFolder(navigation[navigation.count - 2].id, keepingSelection: departed.id)
    }
    func openSelection() { if let selected { open(selected) } }
    func revealSelection() { if let selected { NSWorkspace.shared.activateFileViewerSelecting([selected.url]) } }
    func previewSelection() { if let selected, !selected.isDirectory { previewURL = selected.url } }

    func removeFromCollection(_ id: String) { removeFromCollection(id, expectedReview: nil) }

    private func removeFromCollection(_ id: String, expectedReview: CollectionReviewToken?) {
        guard !atomicFolderInstall, displayedRootMatchesActiveSession, let root,
              let session = sessions[root.id], let identity = session.identity,
              !isRemoving, collectionState.pathIDs.contains(id) else { return }
        let rootID = root.id, rootBytes = Data(root.id.utf8), expectedRevision = collectionRevision
        if !announcingCollectionEdit {
            announcingCollectionEdit = true
            objectWillChange.send()
            announcingCollectionEdit = false
        }
        guard !atomicFolderInstall, let latest = sessions[rootID], latest.identity == identity,
              Data(latest.url.path.utf8) == rootBytes, !latest.isRemoving,
              expectedReview.map({ collectionReviewMatches($0) }) ?? true else { return }
        let active = activeSessionID == latest.id
        let state = active ? collectionState : latest.collectionState
        guard state.revision == expectedRevision, state.pathIDs.contains(id) else { return }
        if state.items.count <= 1_000 && active {
            let items = state.items.filter { $0.id != id }
            collectionState = CollectionSnapshot(items: items, identities: state.identities,
                                                 coverage: CollectionCoverage(paths: items.map(\.id)), revision: UUID())
            return
        }
        let edit: CollectionEditQueue
        if let current = collectionEdits[rootID], current.rootIdentity == identity, current.rootBytes == rootBytes { edit = current }
        else {
            edit = CollectionEditQueue(rootIdentity: identity, rootBytes: rootBytes)
            collectionEdits[rootID] = edit
        }
        edit.pending.insert(id)
        guard edit.task == nil else { return }
        edit.task = Task { [weak self, weak edit] in
            guard let edit else { return }
            while let self, self.collectionEdits[rootID] === edit, !edit.pending.isEmpty {
                guard let session = self.sessions[rootID], session.identity == identity,
                      Data(session.url.path.utf8) == rootBytes else {
                    self.collectionEdits.removeValue(forKey: rootID)
                    return
                }
                let active = self.activeSessionID == session.id
                let old = active ? self.collectionState : session.collectionState
                let pending = edit.pending
                #if ORBIT_TESTING
                let observer = self.testBeforeCollectionEdit
                #else
                let observer: @Sendable () -> Void = {}
                #endif
                let prepared = await Task.detached(priority: .userInitiated) {
                    observer()
                    let items = old.items.filter { !pending.contains($0.id) }
                    return CollectionSnapshot(items: items, identities: old.identities,
                                              coverage: CollectionCoverage(paths: items.map(\.id)), revision: UUID())
                }.value
                guard self.collectionEdits[rootID] === edit else { return }
                guard let latest = self.sessions[rootID], latest.identity == identity,
                      Data(latest.url.path.utf8) == rootBytes else {
                    self.collectionEdits.removeValue(forKey: rootID)
                    return
                }
                let stillActive = self.activeSessionID == latest.id
                let state = stillActive ? self.collectionState : latest.collectionState
                guard active == stillActive, old.revision == state.revision else { continue }
                if stillActive {
                    self.objectWillChange.send()
                    guard self.collectionEdits[rootID] === edit,
                          let current = self.sessions[rootID], current === latest,
                          self.activeSessionID == current.id, self.collectionRevision == state.revision else { continue }
                    self.collectionState = prepared
                }
                latest.collectionState = prepared
                edit.pending.subtract(pending)
                if edit.pending.isEmpty {
                    edit.task = nil
                    self.collectionEdits.removeValue(forKey: rootID)
                    return
                }
            }
        }
    }

    func collectSelection() {
        if let selected { collect([selected]) }
    }

    @discardableResult
    func collect(_ nodes: [DiskNode]) -> Bool {
        guard displayedRootMatchesActiveSession, let root, let identity = root.fileIdentity, !nodes.isEmpty else { return false }
        return collectInBackground(nodes, root: root, rootIdentity: identity, groupContext: nil)
    }

    func beginCollectionDrag(_ nodes: [DiskNode], label: String) -> NSPasteboardItem? {
        let epoch = collectionDragEpoch &+ 1
        clearCollectionDrag()
        guard collectionDragEpoch == epoch else { return nil }
        guard displayedRootMatchesActiveSession, let root else { return nil }
        let collected = collectionState.pathIDs
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let protected = ["/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc", home + "/Library"]
        let rootID = root.id
        let context = GroupContext(rootID: rootID, collected: collected, home: home, protected: protected)
        let isLarge = nodes.count > 64
        let eligible = nodes.count == 1 ? nodes.filter(canCollect)
            : isLarge ? Array(nodes.prefix(1))
            : nodes.filter { canBeginGroupedCollection($0, context: context) }
        guard !eligible.isEmpty else {
            statusMessage = nodes.allSatisfy { hasCollectedAncestor($0.id, in: collected) }
                ? "These items are already included in the collection."
                : "These items cannot be collected. Refresh the map if they have changed."
            return nil
        }
        let token = Data(UUID().uuidString.utf8)
        let eligibleIDs = isLarge ? Set<String>() : Set(eligible.map(\.id))
        collectionDrag = CollectionDrag(token: token, rootPath: root.id, rootIdentity: root.fileIdentity,
                                        nodes: isLarge ? nodes : eligible,
                                        omitted: isLarge ? [] : nodes.filter { !eligibleIDs.contains($0.id) },
                                        groupContext: isLarge ? context : nil)
        setDraggingLabel(label, epoch: epoch)
        guard collectionDragEpoch == epoch, collectionDrag?.token == token else { return nil }
        let item = NSPasteboardItem()
        item.setData(token, forType: Self.collectionDragType)
        return item
    }

    func endCollectionDrag() {
        collectionDragEpoch &+= 1
        setDraggingLabel(nil, epoch: collectionDragEpoch)
    }

    private func setDraggingLabel(_ label: String?, epoch: UInt64) {
        if !atomicFolderInstall { objectWillChange.send() }
        guard collectionDragEpoch == epoch else { return }
        folderStateVersion &+= 1
        draggingLabel = label
    }

    func endCollectionDrag(_ item: NSPasteboardItem) {
        guard let token = item.data(forType: Self.collectionDragType), collectionDrag?.token == token else { return }
        endCollectionDrag()
    }

    private func clearCollectionDrag() {
        collectionDrag = nil
        endCollectionDrag()
    }

    func reserveCollectionDrop() -> UUID? {
        guard canAcceptCollectionDrop, let drag = collectionDrag, let root,
              let identity = drag.rootIdentity, root.fileIdentity == identity,
              let session = sessions[drag.rootPath], session.id == activeSessionID,
              session.identity == identity, session.url.path.utf8.elementsEqual(drag.rootPath.utf8) else { return nil }
        let reservation = UUID()
        reservedCollectionDrops[reservation] = ReservedCollectionDrop(drag: drag, root: root)
        clearCollectionDrag()
        return reservation
    }

    @discardableResult
    func completeCollectionDrop(_ reservation: UUID, data: Data?) -> Bool {
        guard let reserved = reservedCollectionDrops.removeValue(forKey: reservation),
              let data, data == reserved.drag.token, let identity = reserved.drag.rootIdentity,
              let owner = sessions[reserved.drag.rootPath], owner.identity == identity,
              owner.url.path.utf8.elementsEqual(reserved.drag.rootPath.utf8) else { return false }
        let nodes = reserved.drag.omitted.isEmpty ? reserved.drag.nodes : reserved.drag.nodes + reserved.drag.omitted
        return collectInBackground(nodes, root: reserved.root, rootIdentity: identity,
                                   groupContext: reserved.drag.groupContext, reservedOwner: owner)
    }

    @discardableResult
    func collectDrag(data: Data) -> Bool {
        guard let drag = collectionDrag, data == drag.token else { return false }
        let canAccept = canAcceptCollectionDrop
        clearCollectionDrag()
        guard canAccept, let root, let expected = drag.rootIdentity,
              drag.rootPath.utf8.elementsEqual(root.id.utf8), root.fileIdentity == expected,
              displayedRootMatchesActiveSession else {
            statusMessage = "The map changed. Drag the item again from the current map."
            return false
        }
        let nodes = drag.omitted.isEmpty ? drag.nodes : drag.nodes + drag.omitted
        return collectInBackground(nodes, root: root, rootIdentity: expected, groupContext: drag.groupContext)
    }

    private func schedulePendingCollectionError() {
        Task { [weak self] in self?.presentPendingCollectionError() }
    }

    private func presentPendingCollectionError() {
        guard !atomicFolderInstall, errorMessage == nil, let session = activeFeedbackSession(),
              let pending = session.presentedCollectionError ?? session.collectionError else { return }
        let sessionID = session.id, errorRevision = errorMessageRevision, queueRevision = session.collectionErrorRevision
        let wasPresented = session.presentedCollectionError != nil
        objectWillChange.send()
        guard !atomicFolderInstall, errorMessage == nil, errorMessageRevision == errorRevision,
              let latest = activeFeedbackSession(), latest === session, latest.id == sessionID,
              latest.collectionErrorRevision == queueRevision,
              (latest.presentedCollectionError ?? latest.collectionError) == pending,
              (latest.presentedCollectionError != nil) == wasPresented else { return }
        if !wasPresented {
            latest.presentedCollectionError = pending
            latest.collectionError = nil
        }
        atomicFolderInstall = true
        suppressCollectionFeedbackInvalidation = true
        errorMessage = pending
        suppressCollectionFeedbackInvalidation = false
        atomicFolderInstall = false
        objectWillChange.send()
    }

    private func session(for job: CollectionValidationJob) -> MappingSession? {
        guard let session = sessions[job.rootPath], session.identity == job.rootIdentity,
              Data(session.url.path.utf8) == job.rootBytes, session.collectionValidation === job else { return nil }
        return session
    }

    private func attachCollectionValidation(previousStatus: UInt64? = nil) {
        guard let root, let session = sessions[root.id], session.id == activeSessionID,
              Data(root.id.utf8) == Data(session.url.path.utf8), root.fileIdentity == session.identity,
              let job = session.collectionValidation, collectionValidationID != job.id else { return }
        let priorStatus = previousStatus ?? statusMessageRevision
        let priorAtomic = atomicFolderInstall
        if !priorAtomic { objectWillChange.send() }
        guard let latest = self.session(for: job), latest === session, activeSessionID == session.id,
              self.root.map({ Data($0.id.utf8) }) == job.rootBytes, self.root?.fileIdentity == root.fileIdentity,
              collectionValidationID == nil else { return }
        atomicFolderInstall = true
        collectionValidationID = job.id
        collectionValidationRequestID = resolutionID
        collectionValidationGeneration = generation
        isValidatingCollection = true
        if statusMessageRevision == priorStatus && session.collectionStatusRevision == job.initialStatusFeedbackRevision {
            suppressCollectionFeedbackInvalidation = true
            statusMessage = "Checking items for collection…"
            suppressCollectionFeedbackInvalidation = false
            collectionValidationStatusRevision = statusMessageRevision
        }
        atomicFolderInstall = priorAtomic
    }

    private func detachCollectionValidation() {
        guard !cancellingCollectionValidation,
              collectionValidationID != nil || isValidatingCollection || collectionValidationStatusRevision != nil else { return }
        cancellingCollectionValidation = true
        defer { cancellingCollectionValidation = false }
        let validationID = collectionValidationID, progressRevision = collectionValidationStatusRevision
        let ownerID = activeSessionID
        let priorAtomic = atomicFolderInstall
        if !priorAtomic { objectWillChange.send() }
        guard collectionValidationID == validationID else { return }
        atomicFolderInstall = true
        collectionValidationID = nil
        collectionValidationRequestID = nil; collectionValidationGeneration = nil
        isValidatingCollection = false
        if let progressRevision, statusMessageRevision == progressRevision {
            suppressCollectionFeedbackInvalidation = true
            statusMessage = nil
            suppressCollectionFeedbackInvalidation = false
        }
        collectionValidationStatusRevision = nil
        atomicFolderInstall = priorAtomic
        cancellingCollectionValidation = false
        if activeSessionID != ownerID { attachCollectionValidation() }
    }

    private func cancelCollectionValidation() {
        if let id = collectionValidationID,
           let session = sessions.values.first(where: { $0.collectionValidation?.id == id }),
           let job = session.collectionValidation {
            job.token.cancel(); job.task?.cancel(); session.collectionValidation = nil
        }
        detachCollectionValidation()
    }

    private func finishCollectionFailure(_ job: CollectionValidationJob) {
        guard let session = session(for: job) else { return }
        let active = activeSessionID == session.id
        let progressRevision = collectionValidationStatusRevision
        let attachedRequest = collectionValidationRequestID, attachedGeneration = collectionValidationGeneration
        session.collectionValidation = nil
        let message = "The map changed. Drag the items again from the current map."
        if active && collectionValidationID == job.id { detachCollectionValidation() }
        guard let current = sessions[job.rootPath], current.identity == job.rootIdentity,
              Data(current.url.path.utf8) == job.rootBytes else { return }
        let canPresent = active && activeSessionID == current.id && displayedRootMatchesActiveSession
            && attachedRequest == resolutionID && attachedGeneration == generation
        if canPresent, let progressRevision, statusMessageRevision == progressRevision &+ 1, statusMessage == nil {
            statusMessage = message
        } else {
            current.enqueueCollectionError(message)
            if canPresent && errorMessage == nil { schedulePendingCollectionError() }
        }
    }

    private func collectInBackground(_ nodes: [DiskNode], root: DiskNode, rootIdentity expected: String,
                                     groupContext: GroupContext?, reservedOwner: MappingSession? = nil) -> Bool {
        guard !atomicFolderInstall, !nodes.isEmpty,
              let owner = sessions[root.id], reservedOwner.map({ $0 === owner }) ?? (owner.id == activeSessionID),
              Data(owner.url.path.utf8) == Data(root.id.utf8), owner.identity == expected else { return false }
        if let job = owner.collectionValidation {
            guard !job.token.isCancelled else { return false }
            job.requests.append((nodes, groupContext, 0, nodes.count > 128 ? CollectionChunkBuffer() : nil))
            return true
        }
        let previousStatus = statusMessageRevision
        let job = CollectionValidationJob(root: root.url, identity: expected, initialStatusFeedbackRevision: owner.collectionStatusRevision), token = job.token
        job.requests.append((nodes, groupContext, 0, nodes.count > 128 ? CollectionChunkBuffer() : nil))
        owner.collectionValidation = job
        if owner.id == activeSessionID { attachCollectionValidation(previousStatus: previousStatus) }
        job.task = Task { [weak self, weak job] in
            guard let job else { return }
            while let self, let session = self.session(for: job), !token.isCancelled {
                if session.isRemoving || self.collectionEdits[job.rootPath] != nil {
                    do { try await Task.sleep(nanoseconds: 50_000_000) } catch { return }
                    continue
                }
                guard job.nextRequest < job.requests.count, let request = job.requests[job.nextRequest] else { return }
                let end = request.offset + min(128, request.nodes.count - request.offset)
                let nodes = Array(request.nodes[request.offset..<end]), groupContext = request.groupContext
                let buffer = request.buffer
                let shouldCommit = buffer.map { $0.processed + nodes.count >= $0.target || end == request.nodes.count } ?? true
                let active = self.activeSessionID == session.id
                let old = active ? self.collectionState : session.collectionState
                let collectionSnapshot = old.items, identities = old.identities, collectedSnapshot = old.pathIDs
                let revision = old.revision
                let removalEpoch = session.removalEpoch
                #if ORBIT_TESTING
                let testCollectionPrepare = self.testCollectionPrepare
                #endif
                if shouldCommit, let buffer, !buffer.chunks.isEmpty {
                    if buffer.revalidationRevision != revision || buffer.revalidationRemovalEpoch != removalEpoch {
                        buffer.revalidationRevision = revision
                        buffer.revalidationRemovalEpoch = removalEpoch
                        buffer.revalidationIndex = 0
                        buffer.readyToCommit = false
                    }
                    if !buffer.readyToCommit {
                        let index = buffer.revalidationIndex
                        let priorNodes = buffer.chunks[index]
                        let recheck = Task.detached(priority: .userInitiated) { () -> CollectionValidation? in
                            guard !token.isCancelled, currentFileIdentity(root.url) == expected,
                                  let checked = validateCollectionNodes(priorNodes, root: root.url, cancellation: token) else { return nil }
                            #if ORBIT_TESTING
                            await testCollectionPrepare()
                            #endif
                            guard !token.isCancelled, currentFileIdentity(root.url) == expected else { return nil }
                            return checked
                        }
                        let checked = await recheck.value
                        guard let latest = self.session(for: job), !token.isCancelled, !Task.isCancelled else { return }
                        let stillActive = self.activeSessionID == latest.id
                        let state = stillActive ? self.collectionState : latest.collectionState
                        guard active == stillActive, revision == state.revision, latest.removalEpoch == removalEpoch,
                              !latest.isRemoving, self.collectionEdits[job.rootPath] == nil else { continue }
                        guard let checked else { self.finishCollectionFailure(job); return }
                        buffer.chunks[index] = checked.nodes
                        buffer.revalidationIndex += 1
                        if buffer.revalidationIndex == buffer.chunks.count { buffer.readyToCommit = true }
                        job.rejectionCount += checked.rejections.count
                        job.rejectionExamples.append(contentsOf: checked.rejections.prefix(max(0, 5 - job.rejectionExamples.count)))
                        job.requests[job.nextRequest] = nil
                        job.nextRequest += 1
                        job.requests.append(request)
                        continue
                    }
                }
                let bufferedChunks = buffer?.chunks ?? []
                let worker = Task.detached(priority: .userInitiated) { () -> CollectionChunkResult? in
                    if token.isCancelled || currentFileIdentity(root.url) != expected { return nil }
                    var ordered = nodes
                    if let context = groupContext {
                        var eligible: [DiskNode] = []
                        for node in nodes {
                            if token.isCancelled { return nil }
                            if canBeginGroupedCollection(node, context: context) { eligible.append(node) }
                        }
                        let ids = Set(eligible.map(\.id))
                        var omitted: [DiskNode] = []
                        for node in nodes {
                            if token.isCancelled { return nil }
                            if !ids.contains(node.id) { omitted.append(node) }
                        }
                        ordered = eligible + omitted
                    }
                    var candidates: [DiskNode] = [], includedCount = 0
                    var seen = Set<String>()
                    for node in ordered {
                        if token.isCancelled { return nil }
                        guard seen.insert(node.id).inserted, !hasCollectedAncestor(node.id, in: collectedSnapshot) else {
                            includedCount += 1
                            continue
                        }
                        candidates.append(node)
                    }
                    guard let validation = validateCollectionNodes(candidates, root: root.url, cancellation: token) else { return nil }
                    #if ORBIT_TESTING
                    await testCollectionPrepare()
                    #endif
                    guard !token.isCancelled, currentFileIdentity(root.url) == expected else { return nil }
                    guard shouldCommit else {
                        return CollectionChunkResult(application: nil, buffered: validation.nodes,
                                                     includedCount: includedCount, rejections: validation.rejections)
                    }
                    let previous = bufferedChunks.flatMap { $0 }
                    let rejections = validation.rejections
                    var combined: [DiskNode] = []
                    var distinct = Set<String>()
                    var duplicates = 0
                    for node in previous + validation.nodes {
                        if token.isCancelled { return nil }
                        if distinct.insert(node.id).inserted { combined.append(node) } else { duplicates += 1 }
                    }
                    let combinedValidation = CollectionValidation(nodes: combined, rejections: rejections)
                    guard let result = prepareCollectionApplication(combinedValidation, existing: collectionSnapshot, identities: identities, collected: collectedSnapshot,
                                                        includedCount: includedCount + duplicates, cancellation: token),
                          currentFileIdentity(root.url) == expected else { return nil }
                    return CollectionChunkResult(application: result, buffered: [], includedCount: 0, rejections: [])
                }
                let prepared = await worker.value
                guard let latest = self.session(for: job), !token.isCancelled, !Task.isCancelled else { return }
                let stillActive = self.activeSessionID == latest.id
                let state = stillActive ? self.collectionState : latest.collectionState
                guard active == stillActive, revision == state.revision else { continue }
                #if ORBIT_TESTING
                if self.testForceCollectionFailure {
                    self.finishCollectionFailure(job)
                    return
                }
                #endif
                guard !latest.isRemoving, self.collectionEdits[job.rootPath] == nil else { continue }
                guard let step = prepared else {
                    self.finishCollectionFailure(job)
                    return
                }
                if step.application?.coverage != nil && self.confirmationHoldsPublication(for: latest) {
                    #if ORBIT_TESTING
                    self.testConfirmationPublicationHoldCount += 1
                    #endif
                    while let owner = self.session(for: job), self.confirmationHoldsPublication(for: owner),
                          !token.isCancelled, !Task.isCancelled {
                        do { try await Task.sleep(nanoseconds: 50_000_000) } catch { return }
                    }
                    continue
                }
                let progressRevision = self.collectionValidationStatusRevision
                if stillActive && step.application?.coverage != nil { self.objectWillChange.send() }
                let rootStillMatches = await Task.detached(priority: .userInitiated) {
                    !token.isCancelled && currentFileIdentity(root.url) == expected
                }.value
                guard let current = self.session(for: job), !token.isCancelled, !Task.isCancelled else { return }
                if !rootStillMatches {
                    self.finishCollectionFailure(job)
                    return
                }
                let activeNow = self.activeSessionID == current.id
                let currentState = activeNow ? self.collectionState : current.collectionState
                guard activeNow == stillActive, currentState.revision == revision, !current.isRemoving,
                      current.removalEpoch == removalEpoch, self.collectionEdits[job.rootPath] == nil else { continue }
                if step.application?.coverage != nil && self.confirmationHoldsPublication(for: current) { continue }
                if !shouldCommit, let buffer {
                    buffer.chunks.append(step.buffered)
                    buffer.processed += nodes.count
                    buffer.readyToCommit = false
                    job.includedCount += step.includedCount
                    job.rejectionCount += step.rejections.count
                    job.rejectionExamples.append(contentsOf: step.rejections.prefix(max(0, 5 - job.rejectionExamples.count)))
                    job.requests[job.nextRequest] = nil
                    job.nextRequest += 1
                    job.didChunk = true
                    job.requests.append((request.nodes, request.groupContext, end, buffer))
                    continue
                }
                guard let prepared = step.application else { self.finishCollectionFailure(job); return }
                let next: CollectionSnapshot
                if let coverage = prepared.coverage {
                    next = CollectionSnapshot(items: prepared.collection, identities: prepared.identities,
                                              coverage: coverage, revision: UUID(), size: prepared.size, pathIDs: prepared.pathIDs)
                } else { next = currentState }
                if job.addedCount == 0 { job.addedName = prepared.addedName }
                job.addedCount += prepared.addedCount
                job.includedCount += prepared.includedCount
                job.rejectionCount += prepared.rejections.count
                job.rejectionExamples.append(contentsOf: prepared.rejections.prefix(max(0, 5 - job.rejectionExamples.count)))
                current.collectionState = next
                job.requests[job.nextRequest] = nil
                job.nextRequest += 1
                if let buffer {
                    buffer.chunks.removeAll(keepingCapacity: true)
                    buffer.processed = 0
                    buffer.revalidationIndex = 0
                    buffer.revalidationRevision = nil; buffer.revalidationRemovalEpoch = nil
                    buffer.readyToCommit = false
                    buffer.target = min(buffer.target * 2, 8192)
                }
                if end < request.nodes.count {
                    job.didChunk = true
                    job.requests.append((request.nodes, request.groupContext, end, buffer))
                }
                if job.nextRequest < job.requests.count {
                    if activeNow && prepared.coverage != nil {
                        let priorAtomic = self.atomicFolderInstall
                        self.atomicFolderInstall = true
                        self.collectionState = next
                        self.atomicFolderInstall = priorAtomic
                        self.objectWillChange.send()
                    }
                    continue
                }
                var messages: [String] = []
                if job.didChunk && job.addedCount > 0 { messages.append("Collection updated.") }
                else if job.addedCount == 1, let name = job.addedName { messages.append("Added \(name) to collection.") }
                else if job.addedCount > 0 { messages.append("Added \(job.addedCount) items to collection.") }
                if job.includedCount > 0 { messages.append("\(job.includedCount) \(job.includedCount == 1 ? "item already included" : "items already included").") }
                var rejectionMessage: String?
                if job.rejectionCount > 0 {
                    messages.append("Skipped \(job.rejectionCount) \(job.rejectionCount == 1 ? "item" : "items").")
                    rejectionMessage = job.rejectionExamples.joined(separator: "\n")
                        + (job.rejectionCount > 5 ? "\n…and \(job.rejectionCount - 5) more." : "")
                }
                current.collectionValidation = nil
                if !activeNow || self.collectionValidationID != job.id {
                    if !messages.isEmpty && current.collectionStatusRevision == job.initialStatusFeedbackRevision {
                        current.collectionStatus = messages.joined(separator: " ")
                    }
                    if let rejectionMessage { current.enqueueCollectionError(rejectionMessage) }
                }
                if activeNow {
                    self.atomicFolderInstall = true
                    self.collectionState = next
                    if self.collectionValidationID == job.id {
                    self.collectionValidationID = nil; self.collectionValidationStatusRevision = nil
                    self.collectionValidationRequestID = nil; self.collectionValidationGeneration = nil
                    self.isValidatingCollection = false
                    if let rejectionMessage {
                        if self.errorMessage == nil && current.collectionError == nil && current.presentedCollectionError == nil {
                            current.presentedCollectionError = rejectionMessage
                            self.errorMessage = rejectionMessage
                        } else {
                            current.enqueueCollectionError(rejectionMessage)
                        }
                    }
                    if let progressRevision, self.statusMessageRevision == progressRevision,
                       self.statusMessage == "Checking items for collection…", !messages.isEmpty {
                        self.statusMessage = messages.joined(separator: " ")
                    }
                    }
                    self.atomicFolderInstall = false
                    self.objectWillChange.send()
                    if self.errorMessage == nil && (current.collectionError != nil || current.presentedCollectionError != nil) {
                        self.schedulePendingCollectionError()
                    }
                }
                return
            }
        }
        return true
    }

    func moveCollectionToTrash() { removeCollection(permanently: false) }
    func moveCollectionToTrash(review token: CollectionReviewToken?) {
        guard collectionReviewMatches(token) else { return }
        removeCollection(permanently: false)
    }

    func deleteCollectionPermanently() { removeCollection(permanently: true) }
    func deleteCollectionPermanently(review token: CollectionReviewToken?) {
        guard collectionReviewMatches(token) else { return }
        removeCollection(permanently: true)
    }

    private func removeCollection(permanently: Bool) {
        guard displayedRootMatchesActiveSession, let root, !collection.isEmpty, !isRemoving,
              let sessionID = activeSessionID, let session = sessions.values.first(where: { $0.id == sessionID }),
              !session.isRemoving, !isUpdatingCollection else { return }
        let items = collection, identities = collectionIdentities
        let rootURL = root.url, rootIdentity = root.fileIdentity
        let journalCache = store.directory
        let parentLimit = recoveryParentLimit
        let startGeneration = generation
        let startErrorRevision = errorMessageRevision, startStatusRevision = statusMessageRevision
        session.isRemoving = true
        session.collectionState = collectionState
        session.removalStatus = permanently ? "Deleting collected items permanently…" : "Moving collected items to Trash…"
        session.removalError = nil
        startingRemovals[sessionID] = (Data(rootURL.path.utf8), session.identity)
        let removalID = beginRemovalActivity()
        removalReceipts[removalID] = AutomationRemovalReceipt(id: removalID, sessionID: sessionID, running: true,
            removedCount: nil, status: permanently ? "Deleting collected items permanently…" : "Moving collected items to Trash…", error: nil)
        removalReceiptOrder.append(removalID)
        if activeSessionID == sessionID, generation == startGeneration { isRemoving = true }
        // An observer can switch sessions before @Published commits the value.
        // Restore the pending removal to A and reconcile the visible flag with its current owner.
        if let current = sessions[rootURL.path], current.identity == session.identity,
           current.url.path.utf8.elementsEqual(rootURL.path.utf8) { current.isRemoving = true }
        while let activeID = activeSessionID,
              let active = sessions.values.first(where: { $0.id == activeID }),
              isRemoving != active.isRemoving {
            isRemoving = active.isRemoving
            if let current = sessions[rootURL.path], current.identity == session.identity,
               current.url.path.utf8.elementsEqual(rootURL.path.utf8) { current.isRemoving = true }
        }
        func ownsDisplayedRoot() -> Bool {
            guard activeSessionID == sessionID, generation == startGeneration,
                  displayedRootMatchesActiveSession, let displayed = self.root else { return false }
            return displayed.id.utf8.elementsEqual(rootURL.path.utf8) && displayed.fileIdentity == rootIdentity
        }
        if ownsDisplayedRoot() {
            objectWillChange.send()
            if ownsDisplayedRoot() {
                let priorAtomic = atomicFolderInstall
                atomicFolderInstall = true
                clearCollectionDrag()
                if errorMessageRevision == startErrorRevision { errorMessage = nil }
                if statusMessageRevision == startStatusRevision { statusMessage = session.removalStatus }
                atomicFolderInstall = priorAtomic
            }
        }
        startingRemovals.removeValue(forKey: sessionID)
        #if ORBIT_TESTING
        let removalObserver = testBeforeRemoval
        #else
        let removalObserver: @Sendable () -> Void = {}
        #endif
        Task { [weak self, session] in
            let outcome = await withCheckedContinuation { continuation in
                // File removal can block; keep its I/O off the cooperative executor.
                DispatchQueue.global(qos: .userInitiated).async {
                    let remove = {
                        removalObserver()
                        var removed = Set<String>()
                        var failures: [String] = []
                        var changedParents = Set<String>()
                        var rootChanged = false
                        var parentsOverflow = false
                        func noteParent(_ path: String) {
                            if changedParents.contains(path) || changedParents.count < parentLimit { changedParents.insert(path) }
                            else { parentsOverflow = true }
                        }
                        let candidates = items.compactMap { item in
                            identities[item.id].map { ScanRemoval(path: item.id, identity: $0) }
                        }
                        let operation: RemovalJournal.Operation
                        do {
                            guard let rootIdentity, !candidates.isEmpty else {
                                throw DiskIndexError.invalid("The selected items no longer have a valid mapping identity.")
                            }
                            operation = try RemovalJournal(cache: journalCache).begin(root: rootURL, rootIdentity: rootIdentity, items: candidates)
                        } catch {
                            return (removed, ["Removal could not be recorded: \(error.localizedDescription)"], changedParents, rootChanged, [ScanRemoval](), false, parentsOverflow, Optional<RemovalCursor>.none)
                        }
                        for item in items {
                            guard let rootIdentity, currentFileIdentity(rootURL) == rootIdentity else {
                                failures.append("The mapped folder changed. Refresh the map before removing any more items.")
                                rootChanged = true
                                break
                            }
                            guard let expected = identities[item.id], currentFileIdentity(item.url) == expected else {
                                failures.append("\(item.name): the item changed. Remove it from the collection and add it again.")
                                if isWithin(item.id, root: rootURL.path) { noteParent(item.url.deletingLastPathComponent().path) }
                                continue
                            }
                            guard operation.parentIsUnchanged(path: item.id) else {
                                failures.append("\(item.name): the containing folder changed. Refresh the map before removing it.")
                                noteParent(item.url.deletingLastPathComponent().path)
                                continue
                            }
                            if let reason = TrashPolicy.rejectionReason(for: item.url, scanRoot: rootURL) {
                                failures.append("\(item.name): \(reason)"); continue
                            }
                            noteParent(item.url.deletingLastPathComponent().path)
                            do {
                                if permanently { try FileManager.default.removeItem(at: item.url) }
                                else { try FileManager.default.trashItem(at: item.url, resultingItemURL: nil) }
                                removed.insert(item.id)
                            } catch { failures.append("\(item.name): \(error.localizedDescription)") }
                        }
                        do { try operation.complete() }
                        catch { failures.append("Removal tracking could not be completed: \(error.localizedDescription)") }
                        var overlay: [ScanRemoval] = []
                        for item in items where removed.contains(item.id) {
                            guard let identity = identities[item.id] else { continue }
                            if overlay.count < 10_000 { overlay.append(ScanRemoval(path: item.id, identity: identity)) }
                        }
                        return (removed, failures, changedParents, rootChanged, overlay, removed.count > overlay.count, parentsOverflow, Optional(operation.cursor))
                    }
                    continuation.resume(returning: remove())
                }
            }
            guard let self else { return }
            defer {
                self.finishRemovalReceipt(removalID, count: outcome.0.count, permanently: permanently, failures: outcome.1)
                self.endRemovalActivity(removalID)
            }
            let completedSession: MappingSession
            let completionErrorRevision: UInt64, completionStatusRevision: UInt64, completionPausedRevision: UInt64
            while true {
                guard let current = self.sessions[rootURL.path], current.identity == session.identity,
                      current.url.path.utf8.elementsEqual(rootURL.path.utf8) else { return }
                let active = self.activeSessionID == current.id
                let generation = self.generation
                let state = active ? self.collectionState : current.collectionState
                let prior = current.removals, epoch = current.removalEpoch
                #if ORBIT_TESTING
                let mergeObserver = self.testBeforeRemovalMerge
                #else
                let mergeObserver: @Sendable () -> Void = {}
                #endif
                let prepared = await Task.detached(priority: .utility) {
                    mergeObserver()
                    let removedMatching = outcome.0.filter { state.identities[$0] == identities[$0] }
                    let items = state.items.filter { !removedMatching.contains($0.id) }
                    let kept = state.identities.filter { !removedMatching.contains($0.key) }
                    let next = CollectionSnapshot(items: items, identities: kept,
                                                  coverage: CollectionCoverage(paths: items.map(\.id)), revision: UUID())
                    return (next, mergingRemovals(prior, outcome.4))
                }.value
                guard self.sessions[rootURL.path] === current, current.removalEpoch == epoch else { continue }
                let activeNow = self.activeSessionID == current.id
                let currentState = activeNow ? self.collectionState : current.collectionState
                guard activeNow == active, currentState.revision == state.revision,
                      !active || self.generation == generation else { continue }
                let errorRevision = self.errorMessageRevision, statusRevision = self.statusMessageRevision
                let pausedRevision = self.updatesPausedRevision
                if active { self.objectWillChange.send() }
                guard self.sessions[rootURL.path] === current, current.removalEpoch == epoch,
                      (self.activeSessionID == current.id) == active,
                      (!active || (self.generation == generation && self.collectionRevision == state.revision)),
                      (active || current.collectionState.revision == state.revision) else { continue }
                if active { self.collectionState = prepared.0 }
                current.collectionState = prepared.0
                current.removalEpoch &+= 1; current.removals = prepared.1.items
                if let cursor = outcome.7 {
                    if let target = current.durableRecoveryTarget, target.identity != cursor.identity {
                        current.durableRecoveryConflict = true
                        current.durablePreviewBlocked = true
                        current.pendingFullRefresh = true
                        if active { self.pendingFullRefresh = true }
                    } else { current.includeRemovalTarget(cursor) }
                }
                let combinedParents = current.removalParents.union(outcome.2)
                if outcome.6 || combinedParents.count > self.recoveryParentLimit {
                    current.pendingFullRefresh = true
                    if active { self.pendingFullRefresh = true }
                } else {
                    current.removalParents = combinedParents
                    for path in outcome.2 { current.removalVersions[path] = current.removalEpoch }
                }
                if !outcome.2.isEmpty { current.durableRecoveryPending = true }
                if outcome.5 || outcome.6 || prepared.1.overflow || combinedParents.count > self.recoveryParentLimit { current.durablePreviewBlocked = true }
                current.isRemoving = false
                current.removalError = outcome.1.isEmpty ? nil : outcome.1.joined(separator: "\n")
                let count = outcome.0.count, noun = count == 1 ? "item" : "items"
                current.removalStatus = permanently ? "Deleted \(count) \(noun) permanently." : "Moved \(count) \(noun) to Trash."
                completedSession = current
                completionErrorRevision = errorRevision; completionStatusRevision = statusRevision
                completionPausedRevision = pausedRevision
                break
            }
            let completionID = UUID()
            self.finishingRemovals[completionID] = (Data(rootURL.path.utf8), completedSession.identity)
            defer { self.finishingRemovals.removeValue(forKey: completionID) }
            let completionGeneration = self.generation
            if self.activeSessionID == completedSession.id { self.isRemoving = false }
            while let activeID = self.activeSessionID,
                  let active = self.sessions.values.first(where: { $0.id == activeID }),
                  self.isRemoving != active.isRemoving {
                self.isRemoving = active.isRemoving
            }
            var clearedForRepair = false
            @MainActor func ownsCompletionPresentation() -> Bool {
                guard self.activeSessionID == completedSession.id, self.generation == completionGeneration,
                      !completedSession.isRemoving, !self.isRemoving else { return false }
                guard let displayed = self.root else { return clearedForRepair }
                return self.displayedRootMatchesActiveSession &&
                    displayed.id.utf8.elementsEqual(rootURL.path.utf8) && displayed.fileIdentity == rootIdentity
            }
            guard ownsCompletionPresentation() else { return }
            if completedSession.durablePreviewBlocked {
                guard self.clearPresentation() else { return }
                clearedForRepair = true
                guard self.root == nil, ownsCompletionPresentation() else { return }
            }
            self.objectWillChange.send()
            guard ownsCompletionPresentation() else { return }
            let priorAtomic = self.atomicFolderInstall
            self.atomicFolderInstall = true
            self.showCollection = self.showCollection && !self.collection.isEmpty
            if self.errorMessageRevision == completionErrorRevision { self.errorMessage = completedSession.removalError }
            if self.statusMessageRevision == completionStatusRevision { self.statusMessage = completedSession.removalStatus }
            if outcome.3 { self.updatesPaused = true }
            else {
                self.pendingDirectories.formUnion(completedSession.removalParents)
                self.removalResultStatus = self.statusMessage
                if self.snapshot != nil, self.updatesPausedRevision == completionPausedRevision { self.updatesPaused = false }
            }
            self.atomicFolderInstall = priorAtomic
            let removalGeneration = self.generation
            if self.isFiltering { self.search() }
            guard ownsCompletionPresentation(), self.generation == removalGeneration else { return }
            if !outcome.3 {
                if let saved = self.snapshot, self.monitor == nil { self.beginMonitoring(saved) }
                guard ownsCompletionPresentation(), self.generation == removalGeneration else { return }
                self.scheduleRefresh()
            }
            if let preview = self.scanPreview, self.isPartialMap {
                self.previewPreparationWorker?.cancel(); self.previewPreparationID = UUID()
                self.lastPreviewRevision = nil; self.deferredPreview = nil
                let token = completedSession.token.isCancelled ? ScanCancellation() : completedSession.token
                Task { await self.readPreview(preview, scanID: self.generation, token: token, force: true) }
            } else if self.snapshot != nil {
                self.folderCache.removeAll(); self.folderOrder.removeAll()
                self.loadFolder(self.requestedFolderPath ?? rootURL.path, keepingSelection: self.selectedID)
            }
            guard ownsCompletionPresentation(), self.generation == removalGeneration else { return }
            if let completion = self.deferredScanCompletion, !self.mapInteractionActive {
                self.deferredScanCompletion = nil; completion()
            }
            guard ownsCompletionPresentation(), self.generation == removalGeneration else { return }
            self.refreshVolumes()
        }
    }

    func retryFailedSearch() {
        if searchFailed && !isSearching { search() }
    }

    private func search() {
        searchTask?.cancel(); searchCancellation?.cancel()
        let originalQuery = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let session = sessions.values.first { $0.id == activeSessionID }
        if session?.durablePreviewBlocked == true {
            _ = updateSearchState(nodes: [], count: 0, searching: false, incomplete: true, lookup: [:])
            return
        }
        let owner = activeSessionID, removalEpoch = session?.removalEpoch ?? 0, scanID = generation
        let removals = session?.removals ?? []
        let sameRoot = searchStateRootPath == session?.url.path && searchStateRootIdentity == session?.identity
        let sameSearch = !originalQuery.isEmpty && searchStateQuery == originalQuery && sameRoot
        let keepResults = !originalQuery.isEmpty && sameRoot && searchStateRemovalEpoch == removalEpoch
        guard updateSearchState(nodes: keepResults ? nil : [], count: keepResults ? nil : 0, searching: !originalQuery.isEmpty, lookup: keepResults ? nil : [:]) else { return }
        guard query.trimmingCharacters(in: .whitespacesAndNewlines) == originalQuery, generation == scanID,
              activeSessionID == owner, session?.removalEpoch ?? 0 == removalEpoch else { return }
        if !sameSearch { searchStateUsesPreviewOrder = false }
        if isPartialMap, !originalQuery.isEmpty, let preview = scanPreview {
            let token = ScanCancellation()
            let replayGeneration = preview.removalReplayReadiness()?.generation
            let searchDelay: UInt64 = preview.isSearchPrepared ? 16_000_000 : 100_000_000
            searchCancellation = token
            #if ORBIT_TESTING
            let searchObserver = testBeforePreviewSearch
            let finishedObserver = testAfterPreviewSearch
            #else
            let searchObserver: @Sendable () -> Void = {}
            #endif
            searchTask = Task { [weak self, weak session] in
                do {
                    try await Task.sleep(nanoseconds: searchDelay)
                    let result = try await Task.detached(priority: .userInitiated) {
                        searchObserver()
                        #if ORBIT_TESTING
                        do {
                            let result = try preview.search(query: originalQuery, cancellation: token, removals: removals)
                            finishedObserver("success"); return result
                        } catch { finishedObserver(error is CancellationError ? "cancelled" : "error"); throw error }
                        #else
                        return try preview.search(query: originalQuery, cancellation: token, removals: removals)
                        #endif
                    }.value
                    guard let self, !Task.isCancelled, !token.isCancelled, self.generation == scanID,
                          self.isPartialMap, self.scanPreview === preview, self.activeSessionID == owner,
                          self.query.trimmingCharacters(in: .whitespacesAndNewlines) == originalQuery else { return }
                    self.searchTask = nil
                    guard preview.removalReplayReadiness()?.generation == replayGeneration,
                          preview.removalReplayReadiness()?.blocked != true,
                          session?.durablePreviewBlocked != true else { self.search(); return }
                    guard session?.removalEpoch ?? 0 == removalEpoch, !result.removalsChanged else { self.search(); return }
                    let retained = [self.selectedID, self.hoveredListID].compactMap { $0.flatMap { self.searchNodesByID[$0] } }
                    var lookup: [String: DiskNode] = [:]
                    for node in retained { lookup[node.id] = node }
                    for node in result.nodes { lookup[node.id] = node }
                    guard self.updateSearchState(nodes: result.nodes, count: result.count, searching: false, incomplete: false, lookup: lookup) else { return }
                    self.searchStateQuery = originalQuery; self.searchStateRootPath = session?.url.path; self.searchStateRootIdentity = session?.identity; self.searchStateRemovalEpoch = removalEpoch; self.searchStateUsesPreviewOrder = true
                    if preview.removalReplayReadiness()?.generation != replayGeneration || preview.removalReplayReadiness()?.blocked == true {
                        self.search(); return
                    }
                    guard !token.isCancelled, self.generation == scanID, self.query.trimmingCharacters(in: .whitespacesAndNewlines) == originalQuery, session?.removalEpoch ?? 0 == removalEpoch else { return }
                    if preview.revision != result.revision {
                        self.searchTask = Task { [weak self] in
                            do { try await Task.sleep(nanoseconds: 1_500_000_000) } catch { return }
                            guard let self, self.generation == scanID, self.isPartialMap,
                                  self.scanPreview === preview,
                                  self.query.trimmingCharacters(in: .whitespacesAndNewlines) == originalQuery else { return }
                            self.search()
                        }
                    }
                } catch {
                    guard let self, !Task.isCancelled, !token.isCancelled else { return }
                    self.searchTask = nil
                    _ = self.updateSearchState(searching: false, incomplete: true)
                }
            }
            return
        }
        guard !isPartialMap, !originalQuery.isEmpty, let saved = snapshot else {
            _ = updateSearchState(nodes: [], count: 0, searching: false, incomplete: false, lookup: [:])
            return
        }
        let token = ScanCancellation()
        searchCancellation = token
        let store = store, stablePathOrder = sameSearch && searchStateUsesPreviewOrder
        let prepared = store.isSearchPrepared(snapshot: saved)
        if !prepared { prepareSearchIndex() }
        let searchDelay: UInt64 = removals.isEmpty && prepared ? 16_000_000 : 100_000_000
        #if ORBIT_TESTING
        let savedOrderObserver = testSavedSearchOrder
        #else
        let savedOrderObserver: @Sendable (Bool) -> Void = { _ in }
        #endif
        searchTask = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: searchDelay) } catch { return }
            do {
                let worker = Task.detached(priority: .userInitiated) {
                    savedOrderObserver(stablePathOrder)
                    return try store.search(query: originalQuery, snapshot: saved, cancellation: token, stablePathOrder: stablePathOrder, removals: removals)
                }
                let result = try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { token.cancel() })
                guard let self, !Task.isCancelled, !token.isCancelled,
                      self.query.trimmingCharacters(in: .whitespacesAndNewlines) == originalQuery else { return }
                guard self.snapshot?.id == saved.id else { self.search(); return }
                let retained = [self.selectedID, self.hoveredListID].compactMap { id in
                    id.flatMap { self.searchNodesByID[$0] }
                }
                var lookup: [String: DiskNode] = [:]
                for node in retained { lookup[node.id] = node }
                for node in result.nodes { lookup[node.id] = node }
                guard self.activeSessionID == owner, session?.removalEpoch ?? 0 == removalEpoch else { self.search(); return }
                guard self.updateSearchState(nodes: result.nodes, count: result.totalCount, searching: false, incomplete: false, lookup: lookup) else { return }
                self.searchStateQuery = originalQuery; self.searchStateRootPath = session?.url.path; self.searchStateRootIdentity = session?.identity; self.searchStateRemovalEpoch = removalEpoch; self.searchStateUsesPreviewOrder = stablePathOrder
            } catch {
                guard let self, !Task.isCancelled, !token.isCancelled else { return }
                if self.adoptLatestSnapshot() || self.snapshot?.id != saved.id { self.search() }
                else {
                    _ = self.updateSearchState(searching: false, incomplete: true)
                }
            }
        }
    }

    func refreshVolumes() {
        volumeRefreshID = UUID()
        let request = volumeRefreshID
        if !isDiscoveringVolumes {
            if !announcingVolumeRefresh {
                announcingVolumeRefresh = true
                objectWillChange.send()
                announcingVolumeRefresh = false
            }
            guard volumeRefreshID == request else { return }
            let priorAtomic = atomicFolderInstall
            atomicFolderInstall = true
            isDiscoveringVolumes = true
            atomicFolderInstall = priorAtomic
        }
        guard !volumeRefreshInFlight else { return }
        startVolumeRefresh()
    }

    private func startVolumeRefresh() {
        volumeRefreshInFlight = true
        let request = volumeRefreshID, discover = volumeDiscovery
        let worker = Task.detached(priority: .utility) { discover() }
        Task { [weak self] in
            let result = await worker.value
            guard let self else { return }
            if self.volumeRefreshID == request {
                self.objectWillChange.send()
                if self.volumeRefreshID == request {
                    let priorAtomic = self.atomicFolderInstall
                    self.atomicFolderInstall = true
                    let root = DiskVolume(url: URL(fileURLWithPath: "/"), name: "Mac", capacity: 0, available: 0)
                    self.volumes = result.contains(where: { $0.url.path == "/" }) ? result : [root] + result
                    self.isDiscoveringVolumes = false
                    self.atomicFolderInstall = priorAtomic
                }
            }
            self.volumeRefreshInFlight = false
            if self.volumeRefreshID != request { self.startVolumeRefresh() }
        }
    }
}

private func discoverVolumes() -> [DiskVolume] {
        let keys: Set<URLResourceKey> = [.volumeNameKey, .volumeTotalCapacityKey, .volumeAvailableCapacityKey]
        let mounted = FileManager.default.mountedVolumeURLs(includingResourceValuesForKeys: Array(keys), options: .skipHiddenVolumes) ?? []
        let urls = [URL(fileURLWithPath: "/")] + mounted.filter { $0.path.hasPrefix("/Volumes/") }
        return urls.compactMap { url in
            guard let info = try? url.resourceValues(forKeys: keys) else { return nil }
            return DiskVolume(url: url, name: info.volumeName ?? url.lastPathComponent,
                              capacity: Int64(info.volumeTotalCapacity ?? 0), available: Int64(info.volumeAvailableCapacity ?? 0))
        }
}

func formatBytes(_ size: Int64) -> String {
    ByteCountFormatter.string(fromByteCount: size, countStyle: .file)
}
