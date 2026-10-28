import AppKit
import SwiftUI
import OrbitCore

private let megabyte: Int64 = 1_000_000
private let gigabyte: Int64 = 1_000 * megabyte
private let home = URL(fileURLWithPath: "/Users/you", isDirectory: true)

private func file(_ path: String, _ size: Int64) -> DiskNode {
    let url = home.appendingPathComponent(path)
    return DiskNode(url: url, name: url.lastPathComponent, isDirectory: false,
                    size: size, allocatedSize: size, fileCount: 1, fileIdentity: "sample:\(path)")
}

private func folder(_ path: String, _ children: [DiskNode]) -> DiskNode {
    let url = home.appendingPathComponent(path, isDirectory: true)
    return DiskNode(url: url, name: url.lastPathComponent, isDirectory: true,
                    size: children.reduce(0) { $0 + $1.size },
                    allocatedSize: children.reduce(0) { $0 + $1.allocatedSize },
                    fileCount: children.reduce(0) { $0 + $1.fileCount }, children: children,
                    fileIdentity: "sample:\(path)")
}

private func fixture() -> (DiskNode, DiskNode, DiskNode) {
    let projects = folder("Projects", [
        folder("Projects/Studio", [file("Projects/Studio/Footage.mov", 28 * gigabyte),
                                    file("Projects/Studio/Exports.zip", 12 * gigabyte),
                                    file("Projects/Studio/Assets.psd", 6 * gigabyte)]),
        folder("Projects/Archive", [file("Projects/Archive/2024.zip", 15 * gigabyte),
                                     file("Projects/Archive/2023.zip", 9 * gigabyte)]),
        file("Projects/Project backup.zip", 8 * gigabyte)
    ])
    let photos = folder("Photos", [file("Photos/Photo Library.photoslibrary", 58 * gigabyte),
                                    file("Photos/RAW archive.zip", 12 * gigabyte)])
    let downloads = folder("Downloads", [file("Downloads/Installer.dmg", 12 * gigabyte),
                                          file("Downloads/Camera originals.zip", 10 * gigabyte),
                                          file("Downloads/Archive.zip", 8 * gigabyte)])
    let music = folder("Music", [file("Music/Library.musiclibrary", 24 * gigabyte),
                                  file("Music/Live recordings.wav", 10 * gigabyte)])
    let documents = folder("Documents", [file("Documents/Work archive.zip", 16 * gigabyte),
                                          file("Documents/Presentation.pdf", 4 * gigabyte)])
    let movies = folder("Movies", [file("Movies/Travel.mov", 15 * gigabyte),
                                    file("Movies/Family.mov", 8 * gigabyte)])
    let children = [projects, photos, music, downloads, movies, documents,
                    file("Desktop archive.zip", 9 * gigabyte), file("Notes.pdf", 100 * megabyte)]
        .sorted { $0.size > $1.size }
    let root = DiskNode(url: home, name: "Home", isDirectory: true,
                        size: children.reduce(0) { $0 + $1.size },
                        allocatedSize: children.reduce(0) { $0 + $1.allocatedSize },
                        fileCount: children.reduce(0) { $0 + $1.fileCount }, children: children,
                        fileIdentity: "sample:home")
    return (root, projects, downloads)
}

private final class CaptureWindow: NSWindow {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
}

@main struct MarketingCapture {
    @MainActor static func main() async throws {
        precondition(CommandLine.arguments.count == 4 && CommandLine.arguments[2] == "-AppleLocale" && CommandLine.arguments[3] == "en_US",
                     "Usage: MarketingCapture OUTPUT_DIRECTORY -AppleLocale en_US")
        precondition(Locale.current.identifier.hasPrefix("en_US"), "Marketing capture requires the en_US locale")
        let output = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        let app = NSApplication.shared
        app.setActivationPolicy(.prohibited)
        let suiteName = "local.orbitdisk.marketing.\(UUID().uuidString)"
        let prefs = UserDefaults(suiteName: suiteName)!
        defer { prefs.removePersistentDomain(forName: suiteName) }
        let model = DiskModel(cacheDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(suiteName),
                              preferences: prefs)
        let (root, projects, downloads) = fixture()
        let window = CaptureWindow(contentRect: CGRect(x: -20_000, y: -20_000, width: 1280, height: 810),
                                   styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                                   backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: .darkAqua)
        window.toolbarStyle = .unified
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        let host = NSHostingController(rootView: AnyView(EmptyView()))
        host.sizingOptions = []
        window.contentViewController = host
        window.orderFront(nil)
        host.rootView = AnyView(ContentView(model: model).preferredColorScheme(.dark).environment(\.controlActiveState, .key))
        window.setContentSize(NSSize(width: 1280, height: 810))
        defer { window.close() }

        func capture(_ name: String, selected: DiskNode, collected: [DiskNode], folder: DiskNode? = nil) async throws {
            model.installMarketingFixture(root: root, selected: selected, collected: collected)
            if let folder { model.installMarketingFolder(root: root, folder: folder) }
            for _ in 0..<8 {
                window.contentView?.superview?.layoutSubtreeIfNeeded()
                host.view.layoutSubtreeIfNeeded()
                window.contentView?.superview?.displayIfNeeded()
                try await Task.sleep(nanoseconds: 30_000_000)
            }
            guard let frame = window.contentView?.superview,
                  let bitmap = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) else {
                fatalError("Cannot render the app window")
            }
            guard bitmap.pixelsWide >= 1200, bitmap.pixelsHigh >= 750 else {
                fatalError("Invalid screenshot dimensions: \(bitmap.pixelsWide) × \(bitmap.pixelsHigh); window=\(window.frame), view=\(frame.frame)")
            }
            frame.cacheDisplay(in: frame.bounds, to: bitmap)
            var samples = Set<String>()
            for x in stride(from: 20, to: bitmap.pixelsWide, by: 20) {
                for y in stride(from: 20, to: bitmap.pixelsHigh, by: 20) {
                    if let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) {
                        samples.insert("\(Int(color.redComponent * 255)),\(Int(color.greenComponent * 255)),\(Int(color.blueComponent * 255))")
                    }
                }
            }
            guard samples.count > 12 else { fatalError("Screenshot is blank or incomplete: \(name)") }
            guard let png = bitmap.representation(using: .png, properties: [:]) else {
                fatalError("Cannot encode the app screenshot")
            }
            try png.write(to: output.appendingPathComponent(name), options: .atomic)
            print("Captured \(name): \(bitmap.pixelsWide) × \(bitmap.pixelsHigh)")
        }
        try await capture("orbit-overview.png", selected: projects, collected: [])
        try await capture("orbit-collection.png", selected: downloads, collected: [downloads])
        try await capture("orbit-projects.png", selected: projects.children[0], collected: [], folder: projects)

        let reviewModel = DiskModel(cacheDirectory: output.appendingPathComponent("review-cache"), preferences: prefs)
        reviewModel.installMarketingFixture(root: root, selected: downloads, collected: [downloads])
        guard let reviewOwner = reviewModel.beginCollectionReview(), reviewModel.collectionReviewMatches(reviewOwner) else {
            fatalError("Marketing collection review has no active owner")
        }
        let reviewWindow = CaptureWindow(contentRect: CGRect(x: -20_000, y: -20_000, width: 720, height: 470),
                                         styleMask: [.borderless], backing: .buffered, defer: false)
        reviewWindow.appearance = NSAppearance(named: .darkAqua)
        let reviewHost = NSHostingController(rootView: CollectionView(model: reviewModel, reviewOwner: reviewOwner)
            .preferredColorScheme(.dark).environment(\.controlActiveState, .key))
        reviewHost.sizingOptions = []
        reviewWindow.contentViewController = reviewHost
        reviewWindow.setContentSize(NSSize(width: 720, height: 470))
        reviewWindow.orderFront(nil)
        defer { reviewWindow.close() }
        for _ in 0..<12 {
            reviewHost.view.layoutSubtreeIfNeeded()
            reviewHost.view.displayIfNeeded()
            try await Task.sleep(nanoseconds: 30_000_000)
        }
        let reviewView = reviewHost.view
        guard let bitmap = reviewView.bitmapImageRepForCachingDisplay(in: reviewView.bounds),
              bitmap.pixelsWide >= 720, bitmap.pixelsHigh >= 470 else {
            fatalError("Cannot render the collection review")
        }
        reviewView.cacheDisplay(in: reviewView.bounds, to: bitmap)
        var reviewSamples = Set<String>()
        for x in stride(from: 20, to: bitmap.pixelsWide, by: 20) {
            for y in stride(from: 20, to: bitmap.pixelsHigh, by: 20) {
                if let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.deviceRGB) {
                    reviewSamples.insert("\(Int(color.redComponent * 255)),\(Int(color.greenComponent * 255)),\(Int(color.blueComponent * 255))")
                }
            }
        }
        guard reviewSamples.count > 5 else { fatalError("Collection review screenshot is blank or incomplete") }
        guard let png = bitmap.representation(using: .png, properties: [:]) else {
            fatalError("Cannot encode the collection review")
        }
        try png.write(to: output.appendingPathComponent("orbit-review.png"), options: .atomic)
        print("Captured orbit-review.png: \(bitmap.pixelsWide) × \(bitmap.pixelsHigh)")
    }
}
