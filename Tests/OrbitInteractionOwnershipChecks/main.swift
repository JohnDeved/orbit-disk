import AppKit
import Combine
import SwiftUI
import Foundation
import OrbitCore

@main struct Checks {
    @MainActor static func wait(_ check: () -> Bool) async throws {
        for _ in 0..<3_000 { if check() { return }; try await Task.sleep(nanoseconds: 2_000_000) }
        fatalError("timeout")
    }
    @MainActor static func main() async throws {
        if CommandLine.arguments.count > 2 && CommandLine.arguments[2] == "hover" {
            try await HoverRefreshHostedChecks.run(URL(fileURLWithPath: CommandLine.arguments[1]))
            return
        }
        NSApplication.shared.setActivationPolicy(.prohibited)
        let base = URL(fileURLWithPath: CommandLine.arguments[1])
        let rootURL = base.appendingPathComponent("root")
        try FileManager.default.createDirectory(at: rootURL, withIntermediateDirectories: true)
        try Data([1]).write(to: rootURL.appendingPathComponent("one"))
        try Data([2]).write(to: rootURL.appendingPathComponent("two"))
        try Data([3]).write(to: rootURL.appendingPathComponent("three"))
        try Data([4]).write(to: rootURL.appendingPathComponent("four"))
        let suite = "local.orbitdisk.ownership.\(UUID().uuidString)"
        let preferences = UserDefaults(suiteName: suite)!
        defer { preferences.removePersistentDomain(forName: suite) }
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("cache"), preferences: preferences)
        model.scan(rootURL)
        try await wait { model.root?.id == rootURL.path && !model.isScanning }
        let root = model.root!, child = root.children.first!
        let metrics = SunburstMetrics(size: CGSize(width: 300, height: 300))
        let segments = SunburstLayout.make(root: root)
        let segment = segments.first!
        let angle = (segment.start + segment.end) / 2 - .pi / 2
        let radius = metrics.innerRadius + metrics.ringWidth / 2
        let chartPoint = CGPoint(x: metrics.center.x + radius * cos(angle), y: metrics.center.y + radius * sin(angle))
        let window = NSWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 600, height: 400),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.orderFront(nil)
        defer { window.close(); model.cancelScan() }
        func chart(onActivate: @escaping () -> Void = {}, makeDrag: ((SunburstSegment) -> NSPasteboardItem?)? = nil) -> MapInteractionNSView {
            let view = MapInteractionNSView(frame: CGRect(x: 0, y: 0, width: 300, height: 300))
            view.interaction = MapInteractionView(rootID: root.id, layoutRevision: UUID(), segments: segments,
                metrics: metrics, canGoBack: false, onHover: { _ in }, onActivate: { _ in onActivate() },
                makeDrag: makeDrag ?? { model.beginCollectionDrag($0.members, label: $0.name) }, onDragEnded: model.endCollectionDrag,
                beginInteraction: model.beginMapInteraction)
            window.contentView!.addSubview(view); return view
        }
        func row(onOpen: @escaping () -> Void = {}, makeDrag: (([DiskNode], String) -> NSPasteboardItem?)? = nil) -> ListRowInteractionNSView {
            let view = ListRowInteractionNSView(frame: CGRect(x: 300, y: 0, width: 250, height: 50))
            view.interaction = ListRowInteractionView(node: child, onSelect: { _ in }, onOpen: { _ in onOpen() },
                makeDrag: makeDrag ?? model.beginCollectionDrag, onDragEnded: model.endCollectionDrag,
                beginInteraction: model.beginMapInteraction)
            window.contentView!.addSubview(view); return view
        }
        func event(_ type: NSEvent.EventType, _ view: NSView, _ point: CGPoint, count: Int = 1) -> NSEvent {
            NSEvent.mouseEvent(with: type, location: view.convert(point, to: nil), modifierFlags: [], timestamp: 0,
                windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: count, pressure: 1)!
        }
        func turn() async throws { try await Task.sleep(nanoseconds: 30_000_000) }
        let rowPoint = CGPoint(x: 20, y: 25)
        let firstChart = chart(); firstChart.mouseDown(with: event(.leftMouseDown, firstChart, chartPoint))
        precondition(model.testMapInteractionActive)
        MapInteractionView.dismantleNSView(firstChart, coordinator: ())
        MapInteractionView.dismantleNSView(firstChart, coordinator: ())
        let nextRow = row(); nextRow.mouseDown(with: event(.leftMouseDown, nextRow, rowPoint))
        try await turn()
        precondition(model.testMapInteractionActive, "Old chart completion cleared a new row press")
        nextRow.mouseUp(with: event(.leftMouseUp, nextRow, rowPoint))
        precondition(!model.testMapInteractionActive)

        let firstRow = row(); firstRow.mouseDown(with: event(.leftMouseDown, firstRow, rowPoint))
        ListRowInteractionView.dismantleNSView(firstRow, coordinator: ())
        let nextChart = chart(); nextChart.mouseDown(with: event(.leftMouseDown, nextChart, chartPoint))
        try await turn()
        precondition(model.testMapInteractionActive, "Old row completion cleared a new chart press")
        nextChart.mouseUp(with: event(.leftMouseUp, nextChart, chartPoint))
        precondition(!model.testMapInteractionActive)

        var activatedChart: MapInteractionNSView!
        activatedChart = chart(onActivate: { activatedChart.mouseDown(with: event(.leftMouseDown, activatedChart, chartPoint)) })
        activatedChart.mouseDown(with: event(.leftMouseDown, activatedChart, chartPoint))
        activatedChart.mouseUp(with: event(.leftMouseUp, activatedChart, chartPoint))
        precondition(model.testMapInteractionActive, "Chart activation ended the reentrant press")
        activatedChart.cancelInteraction()

        var openedRow: ListRowInteractionNSView!
        openedRow = row(onOpen: { openedRow.mouseDown(with: event(.leftMouseDown, openedRow, rowPoint)) })
        openedRow.mouseDown(with: event(.leftMouseDown, openedRow, rowPoint))
        openedRow.mouseUp(with: event(.leftMouseUp, openedRow, rowPoint))
        openedRow.mouseDown(with: event(.leftMouseDown, openedRow, rowPoint, count: 2))
        openedRow.mouseUp(with: event(.leftMouseUp, openedRow, rowPoint, count: 2))
        precondition(model.testMapInteractionActive, "Row opening ended the reentrant press")
        openedRow.cancelInteraction()

        var dragSetupChart: MapInteractionNSView!
        var newerChartItem: NSPasteboardItem?
        dragSetupChart = chart(makeDrag: { segment in
            let old = model.beginCollectionDrag(segment.members, label: "old setup")
            dragSetupChart.mouseDown(with: event(.leftMouseDown, dragSetupChart, chartPoint))
            newerChartItem = model.beginCollectionDrag([root.children[1]], label: "new setup")
            return old
        })
        dragSetupChart.mouseDown(with: event(.leftMouseDown, dragSetupChart, chartPoint))
        dragSetupChart.mouseDragged(with: event(.leftMouseDragged, dragSetupChart, CGPoint(x: chartPoint.x + 10, y: chartPoint.y)))
        precondition(model.testMapInteractionActive && model.draggingLabel == "new setup" && dragSetupChart.testLastDragSession == nil,
                     "Reentrant chart drag setup started a stale session")
        dragSetupChart.cancelInteraction(); model.endCollectionDrag(newerChartItem!)

        var dragSetupRow: ListRowInteractionNSView!
        var newerRowItem: NSPasteboardItem?
        dragSetupRow = row(makeDrag: { nodes, _ in
            let old = model.beginCollectionDrag(nodes, label: "old setup")
            dragSetupRow.mouseDown(with: event(.leftMouseDown, dragSetupRow, rowPoint))
            newerRowItem = model.beginCollectionDrag([root.children[1]], label: "new setup")
            return old
        })
        dragSetupRow.mouseDown(with: event(.leftMouseDown, dragSetupRow, rowPoint))
        dragSetupRow.mouseDragged(with: event(.leftMouseDragged, dragSetupRow, CGPoint(x: rowPoint.x + 10, y: rowPoint.y)))
        precondition(model.testMapInteractionActive && model.draggingLabel == "new setup" && dragSetupRow.testLastDragSession == nil,
                     "Reentrant row drag setup started a stale session")
        dragSetupRow.cancelInteraction(); model.endCollectionDrag(newerRowItem!)

        for notification in [1, 2] {
            var count = 0
            var nested: NSPasteboardItem?
            let subscriber = model.objectWillChange.sink { _ in
                count += 1
                if count == notification {
                    nested = model.beginCollectionDrag([root.children[notification + 1]], label: "nested")
                }
            }
            let outer = model.beginCollectionDrag([root.children[0]], label: "outer")
            subscriber.cancel()
            precondition(outer == nil && nested != nil && model.draggingLabel == "nested", "Reentrant drag replaced its newer owner")
            model.endCollectionDrag(nested!)
            precondition(model.collectDrag(data: nested!.data(forType: DiskModel.collectionDragType)!), "Newer drag token was lost")
            try await wait { !model.isValidatingCollection && model.collection.contains { $0.id == root.children[notification + 1].id } }
        }

        guard let oldItem = model.beginCollectionDrag([root.children[0]], label: "old"),
              let newItem = model.beginCollectionDrag([root.children[1]], label: "new") else { fatalError("fixture not collectable") }
        model.endCollectionDrag(oldItem)
        precondition(model.draggingLabel == "new", "Old drag completion cleared the newer drag")
        model.endCollectionDrag(NSPasteboardItem())
        precondition(model.draggingLabel == "new", "Item without a token ended a drag")
        model.endCollectionDrag(newItem)
        precondition(model.draggingLabel == nil)
        model.endCollectionDrag(newItem)
        precondition(model.draggingLabel == nil)

        if CommandLine.arguments.last != "row" {
        let dragChart = chart()
        dragChart.mouseDown(with: event(.leftMouseDown, dragChart, chartPoint))
        dragChart.mouseDragged(with: event(.leftMouseDragged, dragChart, CGPoint(x: chartPoint.x + 10, y: chartPoint.y)))
        guard let chartSession = dragChart.testLastDragSession,
              let replacement = model.beginCollectionDrag([root.children[1]], label: "replacement") else { fatalError("chart drag did not start") }
        dragChart.cancelInteraction()
        dragChart.mouseDown(with: event(.leftMouseDown, dragChart, chartPoint))
        dragChart.draggingSession(chartSession, endedAt: .zero, operation: [])
        precondition(model.testMapInteractionActive && model.draggingLabel == "replacement", "Old session ended the reused chart press")
        MapInteractionView.dismantleNSView(dragChart, coordinator: ())
        let activeRow = row(); activeRow.mouseDown(with: event(.leftMouseDown, activeRow, rowPoint))
        try await turn()
        precondition(model.testMapInteractionActive && model.draggingLabel == "replacement")
        dragChart.draggingSession(chartSession, endedAt: .zero, operation: [])
        precondition(model.testMapInteractionActive && model.draggingLabel == "replacement", "Duplicate chart completion changed new interaction")
        activeRow.mouseUp(with: event(.leftMouseUp, activeRow, rowPoint)); model.endCollectionDrag(replacement)
        dragChart.testLastDragSession = nil
        }

        if CommandLine.arguments.last != "chart" {
        let dragRow = row()
        dragRow.mouseDown(with: event(.leftMouseDown, dragRow, rowPoint))
        dragRow.mouseDragged(with: event(.leftMouseDragged, dragRow, CGPoint(x: rowPoint.x + 10, y: rowPoint.y)))
        guard let rowSession = dragRow.testLastDragSession,
              let secondReplacement = model.beginCollectionDrag([root.children[1]], label: "replacement") else { fatalError("row drag did not start") }
        dragRow.cancelInteraction()
        dragRow.mouseDown(with: event(.leftMouseDown, dragRow, rowPoint))
        dragRow.draggingSession(rowSession, endedAt: .zero, operation: [])
        precondition(model.testMapInteractionActive && model.draggingLabel == "replacement", "Old session ended the reused row press")
        ListRowInteractionView.dismantleNSView(dragRow, coordinator: ())
        let activeChart = chart(); activeChart.mouseDown(with: event(.leftMouseDown, activeChart, chartPoint))
        try await turn()
        precondition(model.testMapInteractionActive && model.draggingLabel == "replacement")
        dragRow.draggingSession(rowSession, endedAt: .zero, operation: [])
        precondition(model.testMapInteractionActive && model.draggingLabel == "replacement", "Duplicate row completion changed new interaction")
        activeChart.mouseUp(with: event(.leftMouseUp, activeChart, chartPoint)); model.endCollectionDrag(secondReplacement)
        dragRow.testLastDragSession = nil
        }

        guard let oldDrop = model.beginCollectionDrag([root.children[0]], label: "old drop"),
              let currentDrop = model.beginCollectionDrag([root.children[1]], label: "current drop"),
              let oldToken = oldDrop.data(forType: DiskModel.collectionDragType),
              let currentToken = currentDrop.data(forType: DiskModel.collectionDragType) else { fatalError("drop setup failed") }
        model.endCollectionDrag(oldDrop)
        precondition(!model.collectDrag(data: oldToken) && model.draggingLabel == "current drop")
        model.endCollectionDrag(currentDrop)
        precondition(model.collectDrag(data: currentToken), "Drop arriving after drag source completion was rejected")
        try await wait { !model.isValidatingCollection && model.collection.contains { $0.id == root.children[1].id } }
        precondition(model.collection.contains { $0.id == root.children[1].id }, "Delayed drop collected the wrong item")

        guard let oldRootItem = model.beginCollectionDrag([root.children[0]], label: "old root") else { fatalError("old root drag failed") }
        let otherRoot = base.appendingPathComponent("other")
        try FileManager.default.createDirectory(at: otherRoot, withIntermediateDirectories: true)
        try Data([5]).write(to: otherRoot.appendingPathComponent("other-file"))
        model.scan(otherRoot)
        try await wait { model.root?.id == otherRoot.path && !model.isScanning }
        let newRootItemNodeID = model.root!.children[0].id
        guard let newRootItem = model.beginCollectionDrag([model.root!.children[0]], label: "new root") else { fatalError("new root drag failed") }
        model.endCollectionDrag(oldRootItem)
        precondition(model.draggingLabel == "new root", "Old root's drag completion cleared new root's drag")
        model.endCollectionDrag(newRootItem)
        precondition(model.collectDrag(data: newRootItem.data(forType: DiskModel.collectionDragType)!), "New root drop failed")
        try await wait { !model.isValidatingCollection && model.collection.contains { $0.id == newRootItemNodeID } }

        let oldOwner = model.beginMapInteraction()
        model.cancelScan()
        let newOwner = model.beginMapInteraction()
        oldOwner()
        precondition(model.testMapInteractionActive, "Completion from stopped session cleared new interaction")
        newOwner(); newOwner()
        precondition(!model.testMapInteractionActive)
        var finishSecond: (() -> Void)?
        var finishThird: (() -> Void)?
        var secondCalls = 0
        let finishFirst = model.beginMapInteraction()
        model.testSetDeferredPublication(first: { finishSecond = model.beginMapInteraction() }, second: {
            secondCalls += 1
            finishThird = model.beginMapInteraction()
        })
        finishFirst()
        precondition(model.testMapInteractionActive && secondCalls == 0 && model.hasDeferredIndexedFolder && model.testHasDeferredNameSort,
                     "Reentrant first publication did not hold later stages")
        finishSecond?()
        precondition(model.testMapInteractionActive && secondCalls == 1 && model.testHasDeferredNameSort,
                     "Reentrant second publication did not hold sorting")
        finishThird?()
        precondition(!model.testMapInteractionActive && !model.testHasDeferredNameSort)
        print("PASS: chart and list teardown preserve newer presses, stale drag completions and stopped sessions")
    }
}
