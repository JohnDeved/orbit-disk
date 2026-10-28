import AppKit
import SwiftUI
import UniformTypeIdentifiers

@MainActor
final class FolderPlaces: ObservableObject {
    @Published private(set) var folders: [URL]
    private let preferences: UserDefaults
    private static let preferenceKey = "folderPlaces"

    init(preferences: UserDefaults = .standard) {
        self.preferences = preferences
        var seen = Set<String>()
        folders = (preferences.stringArray(forKey: Self.preferenceKey) ?? []).compactMap { path in
            guard path.hasPrefix("/") else { return nil }
            let url = URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL
            return seen.insert(url.path).inserted ? url : nil
        }
    }

    func receive(_ providers: [NSItemProvider]) async {
        var urls: [URL] = []
        for provider in providers {
            let url: URL? = await withCheckedContinuation { continuation in
                provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier, options: nil) { item, _ in
                    let url: URL?
                    if let value = item as? URL { url = value }
                    else if let data = item as? Data { url = URL(dataRepresentation: data, relativeTo: nil) }
                    else { url = nil }
                    continuation.resume(returning: url)
                }
            }
            if let url { urls.append(url) }
        }
        await addFolders(urls)
    }

    func addFolders(_ urls: [URL]) async {
        let directories = await Task.detached(priority: .userInitiated) {
            urls.compactMap { url -> URL? in
                guard url.isFileURL else { return nil }
                let folder = url.standardizedFileURL.resolvingSymlinksInPath()
                guard (try? folder.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) == true else { return nil }
                return folder
            }
        }.value
        let home = FileManager.default.homeDirectoryForCurrentUser
        let builtIns = [home, home.appendingPathComponent("Downloads"), home.appendingPathComponent("Documents"),
                        URL(fileURLWithPath: "/Applications")]
        var seen = Set((folders + builtIns).map { $0.path })
        let additions = directories.filter { seen.insert($0.path).inserted }
        guard !additions.isEmpty else { return }
        folders.append(contentsOf: additions)
        save()
    }

    func remove(_ folder: URL) {
        folders.removeAll { $0.path == folder.path }
        save()
    }

    private func save() { preferences.set(folders.map(\.path), forKey: Self.preferenceKey) }
}

struct FolderPlacesDrop: DropDelegate {
    let places: FolderPlaces
    let collectionType: String
    @Binding var isTargeted: Bool

    func validateDrop(info: DropInfo) -> Bool {
        !info.hasItemsConforming(to: [collectionType]) && info.hasItemsConforming(to: [UTType.fileURL.identifier])
    }

    func dropEntered(info: DropInfo) { isTargeted = validateDrop(info: info) }
    func dropExited(info: DropInfo) { isTargeted = false }
    func dropUpdated(info: DropInfo) -> DropProposal? { DropProposal(operation: validateDrop(info: info) ? .copy : .cancel) }

    func performDrop(info: DropInfo) -> Bool {
        isTargeted = false
        guard validateDrop(info: info) else { return false }
        let providers = info.itemProviders(for: [UTType.fileURL.identifier])
        Task { @MainActor in await places.receive(providers) }
        return true
    }
}
