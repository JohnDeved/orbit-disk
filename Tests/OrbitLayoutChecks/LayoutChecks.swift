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
    var keyForChecks = false
    override var isKeyWindow: Bool { keyForChecks || super.isKeyWindow }
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
}
@MainActor final class FrameRecorder {
    var frames: [String: CGRect] = [:]
    var history: [[String: CGRect]] = []
}
@MainActor enum RowEvaluationProbe {
    struct State { let selected: Bool; let hovered: Bool }
    static var pathPrefix: String?
    static var evaluations = 0
    static var states: [String: State] = [:]

    static func reset() { evaluations = 0; states.removeAll(keepingCapacity: true) }
    static func record(id: String, selected: Bool, hovered: Bool) {
        guard let pathPrefix, id.hasPrefix(pathPrefix) else { return }
        evaluations += 1
        states[id] = State(selected: selected, hovered: hovered)
    }
}
final class LayoutGate: @unchecked Sendable {
    let lock = NSLock()
    let semaphore = DispatchSemaphore(value: 0)
    private var waiting = false
    private var used = false
    var isWaiting: Bool { lock.lock(); defer { lock.unlock() }; return waiting }
    func pauseOnce() {
        lock.lock(); let block = !used; used = true; waiting = block; lock.unlock()
        if block { semaphore.wait() }
    }
}
final class SortGate: @unchecked Sendable {
    private let lock = NSLock()
    private var armed = false
    private var started = false
    let release = DispatchSemaphore(value: 0)

    func arm() { lock.lock(); armed = true; started = false; lock.unlock() }
    var hasStarted: Bool { lock.lock(); defer { lock.unlock() }; return started }
    func observe() {
        lock.lock()
        let shouldWait = armed
        armed = false
        if shouldWait { started = true }
        lock.unlock()
        if shouldWait { release.wait() }
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
                .onPreferenceChange(FramePreference.self) { recorder.frames = $0; recorder.history.append($0) }
                // Reinstalling identical geometry must still populate the fresh recorder.
                .id(UUID()))
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
            if let title = baseline["metric-title:File size"], let value = baseline["metric-value:File size"],
               let divider = baseline["metric-divider"] {
                if abs(title.maxX - value.maxX) > 0.5 || abs(divider.minX - value.maxX - 24) > 0.5 {
                    failures.append("File size metric is not aligned toward its divider at \(prefix)")
                }
            } else { failures.append("Missing File size alignment probes at \(prefix)") }
            let originalProgress = model.mappingProgressState.value
            let estimated = MappingProgressPresentation(rootName: "Home", discoveredFiles: 128_400, cachedFiles: 80_892,
                discoveryFraction: 0.52, discoveryState: "52% estimated", discoveryDetail: "From this scan",
                cacheFraction: 0.63, cacheState: "Saving changes", cacheDetail: "", isDiscovering: true, isPaused: false)
            var estimating = estimated
            estimating.discoveryFraction = nil; estimating.discoveryState = "Estimating…"
            estimating.cachedFiles = nil; estimating.cacheFraction = nil; estimating.cacheState = "Reading cache"
            var memoryUnknown = estimated
            memoryUnknown.discoveryFraction = nil; memoryUnknown.discoveryState = "Estimating…"
            var cacheAhead = estimated
            cacheAhead.discoveryFraction = 0.28; cacheAhead.discoveryState = "28% estimated"
            cacheAhead.cacheFraction = 0.82
            var equalFractions = estimated
            equalFractions.discoveryFraction = 0.5; equalFractions.discoveryState = "50% estimated"
            equalFractions.cacheFraction = 0.5
            var cacheCaughtUp = equalFractions
            cacheCaughtUp.cacheFraction = 1; cacheCaughtUp.cachedFiles = cacheCaughtUp.discoveredFiles
            var revised = estimated
            revised.discoveryFraction = 0.47; revised.discoveryState = "47% estimated"
            revised.discoveryDetail = "More files than expected"
            var catchingUp = estimated
            catchingUp.discoveredFiles = 360_000; catchingUp.cachedFiles = 272_400
            catchingUp.discoveryFraction = 1; catchingUp.discoveryState = "Complete"; catchingUp.discoveryDetail = "Discovery complete"
            catchingUp.cacheFraction = 0.76; catchingUp.isDiscovering = false; catchingUp.phase = .saving
            var saved = catchingUp
            saved.cachedFiles = 360_000; saved.cacheFraction = 1; saved.cacheState = "Saved"; saved.phase = .saved
            var paused = estimated
            paused.isDiscovering = false; paused.isPaused = true; paused.discoveryState = "Stopped"
            paused.discoveryDetail = "Progress kept"; paused.cacheState = "Stopped"; paused.phase = .paused
            var cacheError = estimated
            cacheError.isDiscovering = false; cacheError.isPaused = true; cacheError.discoveryState = "Stopped"
            cacheError.cacheState = "Interrupted"; cacheError.cacheDetail = "Retry mapping"; cacheError.phase = .interrupted
            cacheError.rootName = "A very long map root name while browsing a deeply nested folder"
            for (label, progress) in [("estimating", estimating), ("estimated", estimated),
                                      ("memory-unknown", memoryUnknown), ("cache-ahead", cacheAhead), ("equal", equalFractions),
                                      ("cache-caught-up", cacheCaughtUp), ("revised", revised),
                                      ("cache-catchup", catchingUp), ("saved", saved), ("paused", paused), ("cache-error", cacheError)] {
                model.mappingProgressState.value = progress
                let frames = try await capture(prefix + "/progress-" + label)
                compare(prefix + "/progress-" + label, baseline, frames)
                if let footer = frames["footer"], abs(footer.height - (MappingProgressFooter.contentHeight + 12)) > 0.5 {
                    failures.append("Progress footer changed height: \(label) \(footer.height)")
                }
                if label != "revised", let content = window.contentView,
                   let bitmap = content.bitmapImageRepForCachingDisplay(in: content.bounds) {
                    content.cacheDisplay(in: content.bounds, to: bitmap)
                    try bitmap.representation(using: .png, properties: [:])?.write(to:
                        reportDirectory.appendingPathComponent("progress-\(prefix)-\(label).png"))
                }
            }
            model.mappingProgressState.value = nil
            compare(prefix + "/progress-fallback", baseline, try await capture(prefix + "/progress-fallback"))
            model.mappingProgressState.value = originalProgress
            let originalVolumes = model.volumes
            let fallback = DiskVolume(url: URL(fileURLWithPath: "/"), name: "Mac", capacity: 0, available: 0)
            model.isDiscoveringVolumes = true; model.volumes = [fallback]
            let deviceLoading = try await capture(prefix + "/device-loading")
            if size.width == 1280, let content = window.contentView,
               let bitmap = content.bitmapImageRepForCachingDisplay(in: content.bounds) {
                content.cacheDisplay(in: content.bounds, to: bitmap)
                try bitmap.representation(using: .png, properties: [:])?.write(to: reportDirectory.appendingPathComponent("device-loading.png"))
            }
            model.isDiscoveringVolumes = false
            compare(prefix + "/device-unavailable", deviceLoading, try await capture(prefix + "/device-unavailable"), names: ["device:/"])
            model.volumes = [DiskVolume(url: fallback.url, name: "Macintosh HD", capacity: 100, available: 40)]
            compare(prefix + "/device-resolved", deviceLoading, try await capture(prefix + "/device-resolved"), names: ["device:/"])
            if size.width == 1280, let content = window.contentView,
               let bitmap = content.bitmapImageRepForCachingDisplay(in: content.bounds) {
                content.cacheDisplay(in: content.bounds, to: bitmap)
                try bitmap.representation(using: .png, properties: [:])?.write(to: reportDirectory.appendingPathComponent("device-resolved.png"))
            }
            model.volumes = originalVolumes
            model.isRemoving = true
            compare(prefix + "/updating-sizes", baseline, try await capture(prefix + "/updating-sizes"))
            model.isRemoving = false
            if size.width == 1280 { wideToolbarFrames = baseline }
            model.selectedID = short.id
            compare(prefix+"/file-selection", baseline, try await capture(prefix+"/file-selection"))
            model.selectedID = folder.id
            compare(prefix+"/folder-selection", baseline, try await capture(prefix+"/folder-selection"))
            model.selectedID = long.id
            compare(prefix+"/long-selection", baseline, try await capture(prefix+"/long-selection"))
            precondition(model.collect([long]), "The generated fixture must allow collection")
            try await wait("collection validation") { !model.isValidatingCollection && model.collection.contains { $0.id == long.id } }
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
            let partial = DiskModel(cacheDirectory: base.appendingPathComponent("partial-\(prefix)"), preferences: prefs)
            let expectedCount = root.children.count
            partial.testAfterStageStep = { stage in
                guard let view = try? stage.view(path: nil, removals: [], cancellation: ScanCancellation()),
                      view.folder.children.count == expectedCount, !view.isListing else { return }
                gate.pauseOnce()
            }
            partial.scan(rootURL)
            try await wait("partial map") { gate.isWaiting && partial.current?.children.count == expectedCount && partial.isPartialMap }
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
        precondition(model.beginCollectionReview() != nil)
        let tracking = ListHoverTracking()
        let narrowRow = FileRowView(model: model, item: folder, parentSize: root.size, related: false, listHoverTracking: tracking)
        let wideRow = FileRowView(model: model, item: short, parentSize: root.size, related: false, listHoverTracking: tracking)
        install(AnyView(VStack(alignment: .leading, spacing: 0) {
            narrowRow.frame(width: 266)
            wideRow.frame(width: 283)
        }.frame(maxWidth: .infinity, alignment: .leading)), size: CGSize(width: 720, height: 180))
        try await settle()
        for kind in ["row-size:", "row-action:"] {
            guard let narrow = recorder.frames[kind + folder.id], let wide = recorder.frames[kind + short.id] else {
                failures.append("Missing fixed-width row probe: \(kind)")
                continue
            }
            if abs(narrow.minX - wide.minX) > 0.5 {
                failures.append("Row column moved when offered 17 extra points: \(kind)")
            }
        }
        let modalSize = CGSize(width: 720, height: 470)
        install(AnyView(CollectionView(model: model)), size: modalSize)
        let modal = try await capture("collection/normal")
        let modalNames = ["collection-header", "collection-list", "collection-actions", "collection-sheet"]
        model.errorMessage = Array(repeating: "A long removal error for an item whose identity changed. The original file remains in the collection for review.", count: 8).joined(separator: "\n")
        compare("collection/error", modal, try await capture("collection/error"), names: modalNames)
        model.errorMessage = nil; model.isRemoving = true
        compare("collection/busy", modal, try await capture("collection/busy"), names: modalNames)
        model.isRemoving = false
        let pendingNodes = (0..<1_200).map { index in
            DiskNode(url: rootURL.appendingPathComponent("pending-\(index)"), name: "pending-\(index)", isDirectory: false,
                     size: 1, allocatedSize: 1, fileCount: 1, fileIdentity: "fake:\(index)")
        }
        model.collection = [short] + pendingNodes
        model.showCollection = false
        install(AnyView(ContentView(model: model)), size: CGSize(width: 1280, height: 810))
        let reviewBeforePending = try await capture("collection/review-before-pending")
        let reviewBeforeImage: NSBitmapImageRep = {
            let bounds = host.view.bounds
            guard let image = host.view.bitmapImageRepForCachingDisplay(in: bounds), let data = image.bitmapData else {
                fatalError("Could not render review controls")
            }
            host.view.cacheDisplay(in: bounds, to: image)
            _ = data
            return image
        }()
        model.showCollection = true
        install(AnyView(CollectionView(model: model)), size: modalSize)
        let pendingGate = LayoutGate()
        model.testBeforeCollectionEdit = { pendingGate.pauseOnce() }
        func renderedBitmap() -> NSBitmapImageRep {
            let bounds = host.view.bounds
            guard let image = host.view.bitmapImageRepForCachingDisplay(in: bounds), image.bitmapData != nil else {
                fatalError("Could not render the collection sheet")
            }
            host.view.cacheDisplay(in: bounds, to: image)
            return image
        }
        func changedPixels(_ first: NSBitmapImageRep, _ second: NSBitmapImageRep, frame: CGRect) -> Int {
            let scaleX = Double(first.pixelsWide) / host.view.bounds.width
            let scaleY = Double(first.pixelsHigh) / host.view.bounds.height
            let x0 = max(0, Int(frame.minX * scaleX)), x1 = min(first.pixelsWide, Int(ceil(frame.maxX * scaleX)))
            let y0 = max(0, Int(frame.minY * scaleY)), y1 = min(first.pixelsHigh, Int(ceil(frame.maxY * scaleY)))
            func count(mirror: Bool) -> Int {
                var changed = 0
                for y in y0..<y1 {
                    let row = mirror ? first.pixelsHigh - y - 1 : y
                    for x in x0..<x1 {
                        if first.colorAt(x: x, y: row) != second.colorAt(x: x, y: row) { changed += 1 }
                    }
                }
                return changed
            }
            return max(count(mirror: false), count(mirror: true))
        }
        let beforePending = try await capture("collection/before-pending")
        let beforeImage = renderedBitmap()
        model.removeFromCollection(short.id)
        try await wait("pending collection edit") { pendingGate.isWaiting && model.isUpdatingCollection }
        let pendingNames = modalNames + ["collection-remove:" + short.id]
        compare("collection/pending", beforePending, try await capture("collection/pending"), names: pendingNames)
        let pendingImage = renderedBitmap()
        guard let buttonFrame = beforePending["collection-remove:" + short.id] else { fatalError("Missing pending button frame") }
        precondition(changedPixels(beforeImage, pendingImage, frame: buttonFrame) > 0,
                     "Pending removal must visibly change its button")
        precondition(model.isRemovingFromCollection(short.id) && model.isUpdatingCollection)
        model.showCollection = false
        install(AnyView(ContentView(model: model)), size: CGSize(width: 1280, height: 810))
        let reviewPending = try await capture("collection/review-pending")
        compare("collection/review-pending", reviewBeforePending, reviewPending, names: ["review-action"])
        let reviewPendingImage = renderedBitmap()
        guard let reviewFrame = reviewBeforePending["review-action"] else { fatalError("Missing review button frame") }
        precondition(changedPixels(reviewBeforeImage, reviewPendingImage, frame: reviewFrame) > 0,
                     "Pending edits must visibly disable Review Collection")
        model.showCollection = true
        install(AnyView(CollectionView(model: model)), size: modalSize)
        try await settle()
        pendingGate.semaphore.signal()
        try await wait("pending collection edit finished") { !model.isUpdatingCollection }
        precondition(!model.collection.contains { $0.id == short.id })
        model.testBeforeCollectionEdit = {}
        model.showCollection = false

        let largeURL = base.appendingPathComponent("large-list", isDirectory: true)
        let largeCount = 10_000
        let largeNodes = (0..<largeCount).map { index in
            DiskNode(url: largeURL.appendingPathComponent(String(format: "item-%05d.bin", index)),
                     name: String(format: "item-%05d.bin", index), isDirectory: false,
                     size: Int64(largeCount - index), allocatedSize: Int64(largeCount - index), fileCount: 1)
        }
        let largeRoot = DiskNode(url: largeURL, name: "Large folder", isDirectory: true,
                                 size: largeNodes.reduce(0) { $0 + $1.size },
                                 allocatedSize: largeNodes.reduce(0) { $0 + $1.allocatedSize },
                                 fileCount: largeCount, children: largeNodes)
        let sortGate = SortGate()
        defer { sortGate.release.signal() }
        let largeModel = DiskModel(cacheDirectory: base.appendingPathComponent("large-cache"), preferences: prefs,
                                   sortStartObserver: { sortGate.observe() })
        largeModel.root = largeRoot
        largeModel.navigation = [largeRoot]
        RowEvaluationProbe.pathPrefix = largeURL.path + "/"
        RowEvaluationProbe.reset()
        install(AnyView(ContentView(model: largeModel)), size: CGSize(width: 1280, height: 810))
        try await settle()
        func contentsTable(in view: NSView) -> NSTableView? {
            if let table = view as? NSTableView, table.numberOfRows == largeCount { return table }
            for child in view.subviews {
                if let table = contentsTable(in: child) { return table }
            }
            return nil
        }
        guard let initialTable = contentsTable(in: host.view), let initialClip = initialTable.enclosingScrollView?.contentView else {
            fatalError("The lazy-row fixture must expose all \(largeCount) rows in the native Contents table")
        }
        var table = initialTable
        var clip = initialClip
        func rebindAfterReset(_ label: String) async throws {
            try await wait(label) { contentsTable(in: host.view).map { $0 !== table } ?? false }
            guard let replacement = contentsTable(in: host.view), let replacementClip = replacement.enclosingScrollView?.contentView else {
                fatalError("The replacement list must expose its native table")
            }
            table = replacement
            clip = replacementClip
        }
        func renderedItemID(in view: NSView) -> String? {
            if let appearance = view as? SelectionAppearanceView { return appearance.itemID }
            for child in view.subviews {
                if let id = renderedItemID(in: child) { return id }
            }
            return nil
        }
        var lazyReports: [String: [String: Int]] = [:]
        func checkLazyPhase(_ label: String, row: Int, expectedID: String? = nil,
                            selected: Bool? = nil, hovered: Bool? = nil) async throws {
            try await settle()
            let visible = table.rows(in: table.convert(clip.bounds, from: clip).intersection(table.bounds))
            let count = RowEvaluationProbe.evaluations
            let unique = RowEvaluationProbe.states.count
            // The native viewport currently needs 11 bodies; allow additional AppKit prefetch and update passes.
            if count > 100 || unique > 100 {
                failures.append("Lazy rows \(label) rebuilt \(count) bodies / \(unique) distinct rows in a \(largeCount)-row folder")
            }
            if table.numberOfRows != largeCount || visible.location == NSNotFound || !NSLocationInRange(row, visible) {
                failures.append("Lazy rows \(label) did not reveal row \(row): \(visible), table count \(table.numberOfRows)")
            }
            let id = expectedID ?? largeNodes[row].id
            if table.rowView(atRow: row, makeIfNecessary: false).flatMap({ renderedItemID(in: $0) }) != id {
                failures.append("Lazy rows \(label) displayed a stale item at native row \(row)")
            }
            if let selected, RowEvaluationProbe.states[id]?.selected != selected {
                failures.append("Lazy rows \(label) did not render selection=\(selected) for row \(row)")
            }
            if let hovered, RowEvaluationProbe.states[id]?.hovered != hovered {
                failures.append("Lazy rows \(label) did not render hover=\(hovered) for row \(row)")
            }
            lazyReports[label] = ["evaluations": count, "distinctRows": unique,
                                  "visibleStart": visible.location, "visibleCount": visible.length,
                                  "totalRows": table.numberOfRows]
            print("LAZY \(label): \(count) bodies / \(unique) distinct rows; visible \(visible) of \(largeCount)")
        }
        try await checkLazyPhase("initial", row: 0, selected: false, hovered: false)
        RowEvaluationProbe.reset()
        largeModel.hoveredListID = largeNodes[0].id
        try await checkLazyPhase("list-hover", row: 0, hovered: true)
        RowEvaluationProbe.reset()
        largeModel.hoveredListID = nil
        try await checkLazyPhase("list-hover-exit", row: 0, hovered: false)
        RowEvaluationProbe.reset()
        largeModel.selectedID = largeNodes[0].id
        try await checkLazyPhase("selection", row: 0, selected: true)
        RowEvaluationProbe.reset()
        largeModel.selectedID = nil
        try await checkLazyPhase("selection-clear", row: 0, selected: false)
        RowEvaluationProbe.reset()
        largeModel.setMapHover(nodes: [largeNodes[largeCount - 1]], groupID: nil)
        try await checkLazyPhase("graph-hover-last", row: largeCount - 1, hovered: true)
        RowEvaluationProbe.reset()
        largeModel.setMapHover(nodes: [largeNodes[0]], groupID: nil)
        try await checkLazyPhase("graph-hover-first", row: 0, hovered: true)
        RowEvaluationProbe.reset()
        table.scrollRowToVisible(largeCount - 1)
        try await checkLazyPhase("scroll-last", row: largeCount - 1)
        RowEvaluationProbe.reset()
        table.scrollRowToVisible(0)
        try await checkLazyPhase("scroll-first", row: 0)
        RowEvaluationProbe.reset()
        largeModel.setMapHover(nodes: [largeNodes[largeCount - 1], largeNodes[0], largeNodes[largeCount / 2]], groupID: "group:layout")
        try await checkLazyPhase("graph-group-first", row: 0, hovered: true)

        largeModel.setMapHover(nodes: nil, groupID: nil)
        largeModel.navigation = [DiskNode(url: largeURL, name: largeRoot.name, isDirectory: true,
                                         size: largeRoot.size, allocatedSize: largeRoot.allocatedSize,
                                         fileCount: largeCount, children: largeNodes.reversed())]
        largeModel.selectedID = largeNodes[0].id
        try await settle()
        RowEvaluationProbe.reset()
        sortGate.arm()
        largeModel.sortByName = true
        try await wait("large sort starts") { sortGate.hasStarted && largeModel.isSortingRows }
        let hoverID = largeNodes[largeCount - 1].id
        largeModel.setMapHover(nodes: [largeNodes[largeCount - 1]], groupID: nil)
        try await checkLazyPhase("hover-before-sort", row: 0, expectedID: hoverID, hovered: true)
        precondition(largeModel.isSortingRows && largeModel.firstDisplayedRowIndex(matching: [hoverID]) == 0)
        RowEvaluationProbe.reset()
        sortGate.release.signal()
        try await wait("large sort finishes") { !largeModel.isSortingRows }
        try await rebindAfterReset("name sort list reset")
        try await checkLazyPhase("same-count-sort-hover", row: largeCount - 1, expectedID: hoverID, hovered: true)
        precondition(largeModel.firstDisplayedRowIndex(matching: [hoverID]) == largeCount - 1)
        if largeModel.selectedID != largeNodes[0].id { failures.append("Hover after sorting changed selection") }

        RowEvaluationProbe.reset()
        largeModel.sortByName = false
        try await rebindAfterReset("size sort list reset")
        try await checkLazyPhase("size-order-hover", row: 0, expectedID: hoverID, hovered: true)
        precondition(largeModel.firstDisplayedRowIndex(matching: [hoverID]) == 0)
        RowEvaluationProbe.reset()
        largeModel.navigation = [largeRoot]
        try await checkLazyPhase("same-count-reorder-hover", row: largeCount - 1, expectedID: hoverID, hovered: true)
        precondition(largeModel.firstDisplayedRowIndex(matching: [hoverID]) == largeCount - 1)

        largeModel.setMapHover(nodes: nil, groupID: nil)
        largeModel.navigation = [DiskNode(url: largeURL, name: largeRoot.name, isDirectory: true,
                                         size: largeRoot.size, allocatedSize: largeRoot.allocatedSize,
                                         fileCount: largeCount, children: largeNodes.reversed())]
        try await settle()
        table.scrollRowToVisible(4_000)
        let originalRect = table.rect(ofRow: 4_000)
        clip.scroll(to: NSPoint(x: clip.bounds.minX, y: originalRect.minY + 7))
        table.enclosingScrollView?.reflectScrolledClipView(clip)
        try await settle()
        let originalVisible = table.convert(clip.bounds, from: clip).intersection(table.bounds)
        let anchorIndex = table.rows(in: originalVisible).location
        precondition(anchorIndex != NSNotFound && anchorIndex > 0)
        let anchorID = largeNodes[largeCount - 1 - anchorIndex].id
        let anchorOffset = table.rect(ofRow: anchorIndex).minY - originalVisible.minY
        window.keyForChecks = true
        let acceptedFocus = window.makeFirstResponder(table)
        precondition(window.isKeyWindow && acceptedFocus, "The large list must accept keyboard focus")
        largeModel.sortByName = true
        try await wait("scroll-context name sort") { !largeModel.isSortingRows }
        try await rebindAfterReset("scroll-context list reset")
        try await settle()
        let restoredIndex = largeCount - 1 - anchorIndex
        let restoredVisible = table.convert(clip.bounds, from: clip).intersection(table.bounds)
        let restoredRows = table.rows(in: restoredVisible)
        if !NSLocationInRange(restoredIndex, restoredRows)
            || table.rowView(atRow: restoredIndex, makeIfNecessary: false).flatMap({ renderedItemID(in: $0) }) != anchorID
            || abs((table.rect(ofRow: restoredIndex).minY - restoredVisible.minY) - anchorOffset) > 1 {
            failures.append("Large sort failed to restore the visible row and pixel offset")
        }
        let responder = window.firstResponder as? NSView
        if responder !== table && !(responder?.isDescendant(of: table) ?? false) {
            failures.append("Large sort failed to restore list keyboard focus")
        }
        if largeModel.selectedID != largeNodes[0].id { failures.append("Large sort changed the selected item") }
        largeModel.sortByName = false
        try await rebindAfterReset("scroll-context size reset")
        try await settle()
        window.keyForChecks = false

        largeModel.query = "item"
        largeModel.searchResults = largeNodes
        largeModel.searchMatchCount = largeCount
        try await settle()
        let replacementNodes = (0..<largeCount).map { index in
            DiskNode(url: largeURL.appendingPathComponent(String(format: "replacement-%05d.bin", index)),
                     name: String(format: "replacement-%05d.bin", index), isDirectory: false,
                     size: Int64(largeCount - index), allocatedSize: Int64(largeCount - index), fileCount: 1)
        }
        RowEvaluationProbe.reset()
        largeModel.searchResults = replacementNodes
        largeModel.searchMatchCount = largeCount
        largeModel.setMapHover(nodes: [replacementNodes[largeCount - 1]], groupID: nil)
        try await checkLazyPhase("same-count-search-hover", row: largeCount - 1,
                                 expectedID: replacementNodes[largeCount - 1].id, hovered: true)
        RowEvaluationProbe.pathPrefix = nil
        largeModel.cancelScan()
        let lazyReport = try JSONSerialization.data(withJSONObject: lazyReports, options: [.prettyPrinted, .sortedKeys])
        try lazyReport.write(to: reportDirectory.appendingPathComponent("lazy-rows.json"))
        let report = try JSONSerialization.data(withJSONObject: reports, options: [.prettyPrinted, .sortedKeys])
        try report.write(to: reportDirectory.appendingPathComponent("frames.json"))
        let summary = "\(reports.count) rendered states; \(lazyReports.count) lazy-row phases at \(largeCount) rows; maximum structural delta \(maximumDelta)pt; row heights \(String(describing: rowHeightRange)); \(failures.count) failures\n" + failures.joined(separator: "\n")
        try summary.write(to: reportDirectory.appendingPathComponent("summary.txt"), atomically: true, encoding: .utf8)
        print(summary)
        if !failures.isEmpty { exit(1) }
    }
}
