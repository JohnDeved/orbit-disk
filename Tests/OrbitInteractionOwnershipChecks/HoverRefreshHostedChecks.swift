import AppKit
import SwiftUI
import Foundation
import OrbitCore

final class ActiveTestApplication: NSApplication {
    override var isActive: Bool { true }
}
final class PointerTestWindow: NSWindow {
    var pointer = NSPoint.zero
    override var isKeyWindow: Bool { true }
    override var mouseLocationOutsideOfEventStream: NSPoint { pointer }
}
@MainActor func findMap(_ view: NSView) -> MapInteractionNSView? {
    if let map = view as? MapInteractionNSView { return map }
    for child in view.subviews { if let map = findMap(child) { return map } }
    return nil
}
struct HoverRefreshHostedChecks {
    @MainActor static func wait(_ label: String, _ predicate: () -> Bool) async throws {
        for _ in 0..<3000 { if predicate() { return }; try await Task.sleep(nanoseconds: 2_000_000) }
        fatalError("timeout: \(label)")
    }
    @MainActor static func run(_ base: URL) async throws {
        let app = ActiveTestApplication.shared; app.setActivationPolicy(.prohibited)
        precondition(app.isActive)
        try await HoverRefreshModelChecks.run(base.appendingPathComponent("model"))
        let url = base.appendingPathComponent("root")
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("cache"), preferences: UserDefaults(suiteName: "orbit-hosted-\(UUID())")!, volumeDiscovery: { [] })
        func folder(_ large: Bool) -> DiskNode {
            let children = (0..<1_024).map { i in
                let size: Int64 = large && i == 0 ? 100_000 : 1
                return DiskNode(url: url.appendingPathComponent("item-\(i)"), name: "item-\(i)", isDirectory: false,
                                size: size, allocatedSize: size, fileCount: 1, fileIdentity: "id:\(i)")
            }
            let size = children.reduce(Int64(0)) { $0 + $1.size }
            return DiskNode(url: url, name: "root", isDirectory: true, size: size, allocatedSize: size,
                            fileCount: children.count, children: children, fileIdentity: "root")
        }
        let before = folder(false), after = folder(true)
        try await model.testInstallColorFolder(before)
        let window = PointerTestWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 1_280, height: 810),
                              styleMask: [.titled, .closable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let host = NSHostingController(rootView: ContentView(model: model))
        host.sizingOptions = []; window.contentViewController = host; window.orderFront(nil)
        defer { window.close(); model.cancelScan() }
        func pump() { host.view.layoutSubtreeIfNeeded(); host.view.displayIfNeeded() }
        try await wait("map view") { pump(); return findMap(host.view)?.interaction != nil }
        let map = findMap(host.view)!, metrics = map.interaction!.metrics
        let angle = 0.8 - Double.pi / 2, radius = metrics.innerRadius + metrics.ringWidth / 2
        let point = NSPoint(x: metrics.center.x + radius * cos(angle), y: metrics.center.y + radius * sin(angle))
        window.pointer = map.convert(point, to: nil)
        let event = NSEvent.mouseEvent(with: .mouseMoved, location: window.pointer, modifierFlags: [], timestamp: 0,
                                       windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
        map.mouseMoved(with: event)
        try await wait("all grouped hover") { pump(); return model.mapRelatedRowIDs.count == 1_024 }
        let oldRevision = model.mapRevision, oldLayoutRevision = map.interaction!.layoutRevision, beforeIDs = model.mapRelatedRowIDs
        try await model.testPrepareDeferredColorFolder(after)
        precondition(model.testInstallDeferredColorFolder())
        precondition(model.mapRevision != oldRevision && model.mapRelatedRowIDs == beforeIDs,
                     "hover must remain synchronized until chart re-hit")
        let firstID = after.children[0].id
        try await wait("stationary pointer re-hit") {
            pump(); return map.interaction?.layoutRevision != oldLayoutRevision && model.mapRelatedRowIDs == [firstID]
        }
        precondition(model.hoveredMapNodes.count == 1 && model.hoveredMapNodes[0].id == firstID)
        print("PASS: hosted stationary pointer re-hit after all-group chart becomes mixed")
    }
}
