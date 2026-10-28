import AppKit
import UniformTypeIdentifiers

@main
enum FolderPlacesChecks {
    @MainActor
    static func main() async throws {
        let manager = FileManager.default
        let root = manager.temporaryDirectory.appendingPathComponent("orbit-places-\(UUID().uuidString)", isDirectory: true)
        let suite = "OrbitFolderPlacesChecks.\(UUID().uuidString)"
        let preferences = UserDefaults(suiteName: suite)!
        defer { try? manager.removeItem(at: root); preferences.removePersistentDomain(forName: suite) }
        let first = root.appendingPathComponent("First", isDirectory: true)
        let second = root.appendingPathComponent("Second", isDirectory: true)
        let third = root.appendingPathComponent("Folder with ü and spaces", isDirectory: true)
        for folder in [first, second, third] { try manager.createDirectory(at: folder, withIntermediateDirectories: true) }
        let alias = root.appendingPathComponent("alias")
        try manager.createSymbolicLink(at: alias, withDestinationURL: first)
        let file = root.appendingPathComponent("ordinary-file")
        try Data("file".utf8).write(to: file)
        let candidates = [root, first, first.appendingPathComponent("nested")]
        precondition(FolderPlaces.selectedPath(for: nil, among: candidates) == nil)
        precondition(FolderPlaces.selectedPath(for: first.appendingPathComponent("nested/file").path, among: candidates) == candidates[2].path)
        precondition(FolderPlaces.selectedPath(for: first.path + "-other", among: candidates) == root.path,
                     "A similar folder name must not select its sibling")
        let places = FolderPlaces(preferences: preferences)
        await places.addFolders([first, file, first, second, alias, root.appendingPathComponent("missing"),
                                 URL(string: "https://example.com/folder")!, manager.homeDirectoryForCurrentUser])
        let expected = [first, second].map { $0.resolvingSymlinksInPath().path }
        precondition(places.folders.map(\.path) == expected, "Accept only unique folders, excluding built-in places")
        precondition(FolderPlaces(preferences: preferences).folders.map(\.path) == expected, "Places must survive relaunch")

        let provider = NSItemProvider(item: Data(third.absoluteString.utf8) as NSData,
                                      typeIdentifier: UTType.fileURL.identifier)
        let invalid = NSItemProvider(item: Data("invalid".utf8) as NSData,
                                     typeIdentifier: UTType.fileURL.identifier)
        await places.receive([provider, invalid, NSItemProvider(item: file as NSURL, typeIdentifier: UTType.fileURL.identifier)])
        precondition(places.folders.map(\.path) == expected + [third.resolvingSymlinksInPath().path],
                     "Finder URL data must preserve Unicode; invalid URLs and files are ignored")
        places.remove(first.resolvingSymlinksInPath())
        precondition(FolderPlaces(preferences: preferences).folders.map(\.path) ==
                     [second, third].map { $0.resolvingSymlinksInPath().path }, "Removing a place must persist")
        precondition(manager.fileExists(atPath: first.path), "Removing a place must leave the folder intact")
        print("PASS: multiple folder drops, file filtering, deduplication, URL decoding and persistent Places")
    }
}
