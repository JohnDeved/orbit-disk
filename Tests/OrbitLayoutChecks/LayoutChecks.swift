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
        let largeModel = DiskModel(cacheDirectory: base.appendingPathComponent("large-cache"), preferences: prefs)
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
        guard let table = contentsTable(in: host.view), let clip = table.enclosingScrollView?.contentView else {
            fatalError("The lazy-row fixture must expose all \(largeCount) rows in the native Contents table")
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
        largeModel.sortByName = true
        largeModel.setMapHover(nodes: [largeNodes[largeCount - 1]], groupID: nil)
        try await checkLazyPhase("same-count-sort-hover", row: largeCount - 1, hovered: true)
        if largeModel.selectedID != largeNodes[0].id { failures.append("Hover after sorting changed selection") }

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
