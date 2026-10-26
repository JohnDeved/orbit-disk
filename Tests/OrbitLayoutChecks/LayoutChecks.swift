import AppKit
import SwiftUI
import OrbitCore

struct FramePreference: PreferenceKey {
    static var defaultValue: [String: CGRect] = [:]
    static func reduce(value: inout [String: CGRect], nextValue: () -> [String: CGRect]) { value.merge(nextValue(), uniquingKeysWith: { _, new in new }) }
}
extension View {
    func layoutProbe(_ name: String) -> some View {
        background(GeometryReader { geometry in
            Color.clear.preference(key: FramePreference.self, value: [name: geometry.frame(in: .global)])
        })
    }
    func toolbarProbe(_ name: String) -> some View {
        background(ToolbarFrameProbe(name: name))
    }
}
struct ToolbarFrameProbe: NSViewRepresentable {
    let name: String
    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        view.identifier = NSUserInterfaceItemIdentifier("orbit-layout:\(name)")
        view.setAccessibilityElement(false)
        return view
    }
    func updateNSView(_ view: NSView, context: Context) {}
}
final class LayoutWindow: NSWindow {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
}
@MainActor final class FrameRecorder {
    var frames: [String: CGRect] = [:]
    var history: [[String: CGRect]] = []
}
final class LayoutGate: @unchecked Sendable {
    let lock = NSLock()
    let semaphore = DispatchSemaphore(value: 0)
    var count = 0
    func observe(_ progress: ScanProgress) {
        lock.lock(); count += 1; let block = count == 2; lock.unlock()
        if block { semaphore.wait() }
    }
}
@main struct LayoutChecks {
    @MainActor static func wait(_ label: String, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(10)
        while !condition() {
            guard Date() < deadline else { fatalError("Timed out: \(label)") }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }
    @MainActor static func main() async throws {
        let app = NSApplication.shared
        app.setActivationPolicy(.prohibited)
        let fm = FileManager.default
        precondition(CommandLine.arguments.count == 3, "Usage: OrbitLayoutChecks FIXTURE_DIRECTORY REPORT_DIRECTORY")
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let reportDirectory = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
        try fm.createDirectory(at: base, withIntermediateDirectories: true)
        let rootURL = base.appendingPathComponent("fixture", isDirectory: true)
        let folderURL = rootURL.appendingPathComponent("Folder", isDirectory: true)
        let emptyURL = rootURL.appendingPathComponent("Empty", isDirectory: true)
        let longURL = rootURL.appendingPathComponent("A very long folder name that should stay on one line without moving the header controls or changing the layout geometry", isDirectory: true)
        for directory in [folderURL, emptyURL, longURL] { try fm.createDirectory(at: directory, withIntermediateDirectories: true) }
        try Data(repeating: 1, count: 40_000).write(to: rootURL.appendingPathComponent("short.bin"))
        try Data(repeating: 1, count: 30_000).write(to: rootURL.appendingPathComponent("An extremely long filename used to check text truncation and stable action and size columns when collecting search results.bin"))
        try Data(repeating: 1, count: 20_000).write(to: folderURL.appendingPathComponent("child.bin"))
        try Data(repeating: 1, count: 10_000).write(to: longURL.appendingPathComponent("child.bin"))
        for i in 0..<305 { try Data([1]).write(to: rootURL.appendingPathComponent(String(format: "match-%03d.bin", i))) }
        let suiteName = "local.orbitdisk.layoutchecks.\(UUID().uuidString)"
        let prefs = UserDefaults(suiteName: suiteName)!
        defer { prefs.removePersistentDomain(forName: suiteName) }
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("cache"), preferences: prefs)
        model.scan(rootURL)
        try await wait("complete map") { model.root != nil && !model.isScanning && !model.isOpeningFolder }
        model.cancelScan()
        let root = model.current!
        let short = root.children.first { $0.name == "short.bin" }!
        let long = root.children.first { !$0.isDirectory && $0.name.hasPrefix("An extremely") }!
        let folder = root.children.first { $0.id == folderURL.path }!
        let empty = root.children.first { $0.id == emptyURL.path }!
        let longFolder = root.children.first { $0.id == longURL.path }!
        let recorder = FrameRecorder()
        let window = LayoutWindow(contentRect: CGRect(x: -20_000, y: -20_000, width: 1280, height: 810),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.toolbarStyle = .unified
        window.titleVisibility = .hidden
        let host = NSHostingController(rootView: AnyView(EmptyView()))
        host.sizingOptions = []
        window.contentViewController = host
        window.orderFront(nil)
        defer { window.close(); model.cancelScan() }
        var reports: [String: [String: [Double]]] = [:]
        var failures: [String] = []
        var installedSize = CGSize.zero
        let stable = ["window", "header", "toolbar-location", "toolbar-actions", "location", "search", "refresh", "open-folder", "traffic-close", "traffic-minimize", "traffic-zoom", "breadcrumb", "up", "map", "metrics", "panel", "inspector", "inspector-actions", "collect-action", "footer", "tray", "review-action"]
        var maximumDelta = 0.0
        var rowHeightRange: ClosedRange<Double>?
        var wideToolbarFrames: [String: CGRect] = [:]
        func serial(_ frames: [String: CGRect]) -> [String: [Double]] {
            frames.mapValues { [Double($0.minX), Double($0.minY), Double($0.width), Double($0.height)] }
        }
        func delta(_ a: CGRect, _ b: CGRect) -> Double {
            max(abs(a.minX-b.minX), abs(a.minY-b.minY), abs(a.width-b.width), abs(a.height-b.height))
        }
        func settle() async throws {
            for _ in 0..<6 {
                window.contentView?.superview?.layoutSubtreeIfNeeded()
                host.view.layoutSubtreeIfNeeded(); host.view.displayIfNeeded()
                try await Task.sleep(nanoseconds: 35_000_000)
            }
        }
        func install(_ view: AnyView, size: CGSize) {
            installedSize = size
            recorder.frames = [:]
            host.rootView = AnyView(view.preferredColorScheme(.dark)
                .onPreferenceChange(FramePreference.self) { recorder.frames = $0; recorder.history.append($0) })
            window.setContentSize(size)
        }
        func toolbarFrames() -> [String: CGRect] {
            var frames = ["window": CGRect(origin: .zero, size: window.frame.size)]
            func windowFrame(_ view: NSView) -> CGRect {
                let frame = view.convert(view.bounds, to: nil)
                return CGRect(x: frame.minX, y: window.frame.height - frame.maxY, width: frame.width, height: frame.height)
            }
            func visit(_ view: NSView) {
                guard !view.isHiddenOrHasHiddenAncestor else { return }
                if let identifier = view.identifier?.rawValue, identifier.hasPrefix("orbit-layout:") {
                    frames[String(identifier.dropFirst("orbit-layout:".count))] = windowFrame(view)
                }
                for child in view.subviews { visit(child) }
            }
            if let frameView = window.contentView?.superview { visit(frameView) }
            for (name, type) in [("traffic-close", NSWindow.ButtonType.closeButton),
                                 ("traffic-minimize", .miniaturizeButton), ("traffic-zoom", .zoomButton)] {
                if let button = window.standardWindowButton(type) { frames[name] = windowFrame(button) }
            }
            let headerParts = ["location", "search", "refresh", "open-folder"].compactMap { frames[$0] }
            if headerParts.count == 4 { frames["header"] = headerParts.reduce(CGRect.null) { $0.union($1) } }
            return frames
        }
        func checkToolbar(_ label: String, _ frames: [String: CGRect]) {
            let controls = ["header", "toolbar-location", "toolbar-actions", "location", "search", "refresh", "open-folder"]
            guard let traffic = frames["traffic-close"] else { failures.append("Missing traffic lights: \(label)"); return }
            for name in controls {
                guard let frame = frames[name], frame.width > 0, frame.height > 0 else {
                    failures.append("Missing native toolbar control \(name): \(label)"); continue
                }
                if abs(frame.midY - traffic.midY) > 1 {
                    failures.append("\(label) \(name) is not aligned with traffic lights: \(frame.midY) vs \(traffic.midY)")
                }
                if frame.minX < 0 || frame.maxX > window.frame.width + 0.5 {
                    failures.append("\(label) \(name) escapes toolbar width: \(frame)")
                }
                if frame.minY < -0.5 || frame.maxY > 52.5 {
                    failures.append("\(label) \(name) escapes the 52pt titlebar height: \(frame)")
                }
            }
            for (left, right) in [("traffic-zoom", "header"), ("location", "search"), ("search", "refresh"), ("refresh", "open-folder")] {
                if let a = frames[left], let b = frames[right], a.maxX > b.minX + 0.5 {
                    failures.append("\(label) toolbar controls overlap: \(left) \(a), \(right) \(b)")
                }
            }
            if let location = frames["location"], let up = frames["up"], abs(location.minX - up.minX) > 1 {
                failures.append("\(label) location does not align with content navigation: \(location.minX) vs \(up.minX)")
            }
            if let firstWindow = wideToolbarFrames["window"], let firstAction = wideToolbarFrames["open-folder"],
               let action = frames["open-folder"], abs((firstWindow.width - firstAction.maxX) - (window.frame.width - action.maxX)) > 1 {
                failures.append("\(label) toolbar actions lost their trailing alignment: \(action)")
            }
        }
        func capture(_ key: String) async throws -> [String: CGRect] {
            try await settle()
            if abs(window.frame.width - installedSize.width) > 0.5 || abs(window.frame.height - installedSize.height) > 0.5 {
                failures.append("Harness window resized unexpectedly: \(key) \(window.frame.size), expected \(installedSize)")
            }
            let frames = recorder.frames.merging(toolbarFrames(), uniquingKeysWith: { _, native in native })
            if key.hasSuffix("/root") || key.hasSuffix("/resize-back") {
                print("TOOLBAR \(key): \(window.toolbar?.visibleItems?.count ?? 0) visible items; header \(String(describing: frames["header"]))")
            }
            reports[key] = serial(frames)
            if !key.hasPrefix("collection/") { checkToolbar(key, frames) }
            for (_, frame) in frames where frame.height < 0 || frame.width < 0 { failures.append("Negative frame: \(key) \(frame)") }
            return frames
        }
        func compare(_ label: String, _ baseline: [String: CGRect], _ frames: [String: CGRect], names: [String] = stable) {
            for name in names {
                guard let before = baseline[name], let after = frames[name] else { failures.append("Missing \(name): \(label)"); continue }
                let shift = delta(before, after)
                maximumDelta = max(maximumDelta, shift)
                if shift > 0.5 { failures.append("\(label) \(name) moved \(shift)pt: \(before) → \(after)") }
            }
            for kind in ["row-size:", "row-action:"] {
                guard let before = baseline.first(where: { $0.key.hasPrefix(kind) })?.value else { continue }
                for (name, after) in frames where name.hasPrefix(kind) {
                    let shift = max(abs(before.minX - after.minX), abs(before.width - after.width), abs(before.height - after.height))
                    maximumDelta = max(maximumDelta, shift)
                    if shift > 0.5 { failures.append("\(label) \(kind) column moved \(shift)pt: \(before) → \(after)") }
                }
            }
            let heights = frames.filter { $0.key.hasPrefix("row:") }.values.map { Double($0.height) }
            if let low = heights.min(), let high = heights.max() {
                if let existing = rowHeightRange { rowHeightRange = min(low, existing.lowerBound)...max(high, existing.upperBound) }
                else { rowHeightRange = low...high }
                if high-low > 0.5 { failures.append("Different row heights in \(label): \(low)...\(high)") }
            }
        }
        func checkTransition(_ label: String, _ baseline: [String: CGRect]) {
            var shifts = Set<Double>()
            for event in recorder.history {
                for kind in ["row-size:", "row-action:"] {
                    guard let before = baseline.first(where: { $0.key.hasPrefix(kind) })?.value else { continue }
                    for (name, after) in event where name.hasPrefix(kind) {
                        let shift = abs(before.minX - after.minX)
                        shifts.insert(shift)
                    }
                }
            }
            let maximum = shifts.max() ?? 0
            if maximum > 0.5 { failures.append("Transient \(label) column shift: \(maximum)pt in \(recorder.history.count) preference updates") }
            print("TRANSITION \(label): \(recorder.history.count) preference updates, maximum column shift \(maximum)pt")
        }
        for size in [CGSize(width: 1280, height: 810), CGSize(width: 1060, height: 680)] {
            let prefix = "\(Int(size.width))x\(Int(size.height))"
            model.query = ""; model.collection = []; model.selectedID = nil
            model.open(root)
            try await wait("root") { model.current?.id == root.id && !model.isOpeningFolder }
            install(AnyView(ContentView(model: model)), size: size)
            let baseline = try await capture(prefix + "/root")
            if size.width == 1280 { wideToolbarFrames = baseline }
            model.selectedID = short.id
            compare(prefix+"/file-selection", baseline, try await capture(prefix+"/file-selection"))
            model.selectedID = folder.id
            compare(prefix+"/folder-selection", baseline, try await capture(prefix+"/folder-selection"))
            model.selectedID = long.id
            compare(prefix+"/long-selection", baseline, try await capture(prefix+"/long-selection"))
            precondition(model.collect([long]), "The generated fixture must allow collection")
            compare(prefix+"/collected", baseline, try await capture(prefix+"/collected"))
            for (label, node) in [("folder", folder), ("empty", empty), ("long-folder", longFolder)] {
                recorder.history = []
                model.open(node)
                try await wait(label) { model.current?.id == node.id && !model.isOpeningFolder }
                compare(prefix+"/"+label, baseline, try await capture(prefix+"/"+label))
                checkTransition(prefix+"/"+label, baseline)
            }
            recorder.history = []
            model.open(root)
            try await wait("root for search") { model.current?.id == root.id && !model.isOpeningFolder }
            _ = try await capture(prefix+"/returned-root")
            checkTransition(prefix+"/returned-root", baseline)
            for query in ["short", "An extremely", "no-such-file", "match-"] {
                recorder.history = []
                model.query = query
                try await wait("search \(query)") { !model.isSearching }
                compare(prefix+"/search-"+query, baseline, try await capture(prefix+"/search-"+query))
                checkTransition(prefix+"/search-"+query, baseline)
            }
            model.query = ""
            model.hoveredMapNodes = [folder]
            compare(prefix+"/hover", baseline, try await capture(prefix+"/hover"))
            model.hoveredMapNodes = []
            precondition(model.beginCollectionDrag([short], label: "Long dragging label that must not grow the tray or change its review button position") != nil,
                         "The generated fixture must allow a collection drag")
            compare(prefix+"/drag", baseline, try await capture(prefix+"/drag"))
            model.endCollectionDrag()
            let gate = LayoutGate()
            let partial = DiskModel(cacheDirectory: base.appendingPathComponent("partial-\(prefix)"), preferences: prefs, scanProgressObserver: { gate.observe($0) })
            partial.scan(rootURL)
            try await wait("partial map") { partial.current?.children.count == root.children.count && partial.isPartialMap }
            install(AnyView(ContentView(model: partial)), size: size)
            compare(prefix+"/partial", baseline, try await capture(prefix+"/partial"))
            partial.selectedID = partial.current!.children.first { $0.name == "short.bin" }!.id
            compare(prefix+"/partial-selected", baseline, try await capture(prefix+"/partial-selected"))
            gate.semaphore.signal()
            try await wait("partial complete") { !partial.isScanning && !partial.isPartialMap }
            compare(prefix+"/partial-complete", baseline, try await capture(prefix+"/partial-complete"))
            partial.cancelScan()
        }
        installedSize = CGSize(width: 1280, height: 810)
        window.setContentSize(installedSize)
        compare("1280x810/resize-back", wideToolbarFrames, try await capture("1280x810/resize-back"),
                names: ["window", "header", "toolbar-location", "toolbar-actions", "location", "search", "refresh", "open-folder", "traffic-close", "traffic-minimize", "traffic-zoom"])
        model.showCollection = true
        let modalSize = CGSize(width: 720, height: 470)
        install(AnyView(CollectionView(model: model)), size: modalSize)
        let modal = try await capture("collection/normal")
        let modalNames = ["collection-header", "collection-list", "collection-actions", "collection-sheet"]
        model.errorMessage = Array(repeating: "A long removal error for an item whose identity changed. The original file remains in the collection for review.", count: 8).joined(separator: "\n")
        compare("collection/error", modal, try await capture("collection/error"), names: modalNames)
        model.errorMessage = nil; model.isRemoving = true
        compare("collection/busy", modal, try await capture("collection/busy"), names: modalNames)
        model.isRemoving = false; model.showCollection = false
        let report = try JSONSerialization.data(withJSONObject: reports, options: [.prettyPrinted, .sortedKeys])
        try report.write(to: reportDirectory.appendingPathComponent("frames.json"))
        let summary = "\(reports.count) rendered states; maximum structural delta \(maximumDelta)pt; row heights \(String(describing: rowHeightRange)); \(failures.count) failures\n" + failures.joined(separator: "\n")
        try summary.write(to: reportDirectory.appendingPathComponent("summary.txt"), atomically: true, encoding: .utf8)
        print(summary)
        if !failures.isEmpty { exit(1) }
    }
}
