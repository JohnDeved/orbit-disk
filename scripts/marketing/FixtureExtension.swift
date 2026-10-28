
@MainActor extension DiskModel {
    func installMarketingFolder(root: DiskNode, folder: DiskNode) {
        navigation = [root, folder]
        childColorIndices = Dictionary(uniqueKeysWithValues: folder.children.enumerated().map { ($0.element.id, $0.offset) })
    }

    func installMarketingFixture(root: DiskNode, selected: DiskNode, collected: [DiskNode]) {
        let session = MappingSession(url: root.url, identity: root.fileIdentity,
                                     token: ScanCancellation(), preview: nil)
        session.finished = true
        session.discoveredFileCount = root.fileCount
        session.cachedFileCount = root.fileCount
        sessions = [root.id: session]
        activeSessionID = session.id
        showCollection = false
        self.root = root
        navigation = [root]
        nodesByID = [:]
        func register(_ node: DiskNode) {
            nodesByID[node.id] = node
            node.children.forEach(register)
        }
        register(root)
        childColorIndices = Dictionary(uniqueKeysWithValues: root.children.enumerated().map { ($0.element.id, $0.offset) })
        volumes = [DiskVolume(url: URL(fileURLWithPath: "/"), name: "Macintosh HD",
                              capacity: 1_000_000_000_000, available: 342_000_000_000)]
        scanResult = ScanResult(root: root, elapsed: 0, skippedCount: 0, issues: [])
        statusMessage = "Sample data"
        selectedID = selected.id
        collection = collected
        session.collectionState = collectionState
        precondition(displayedCollection.map(\.id) == collected.map(\.id),
                     "Marketing fixture must display the requested collection")
        refreshMappingProgress()
    }
}
