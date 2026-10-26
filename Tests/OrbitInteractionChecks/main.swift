import AppKit
import SwiftUI
import OrbitCore

let app = NSApplication.shared
func pumpMainRunLoop(until completed: () -> Bool) {
    let deadline = Date(timeIntervalSinceNow: 1)
    while !completed() && Date() < deadline {
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.01))
    }
}
func leaf(_ name: String, _ size: Int64) -> DiskNode {
    DiskNode(url: URL(fileURLWithPath: "/fixture/\(name)"), name: name, isDirectory: false,
             size: size, allocatedSize: size, fileCount: 1)
}
let children = [leaf("large.bin", 10_000)] + (0..<40).map { leaf("small-\($0).bin", 1) }
let root = DiskNode(url: URL(fileURLWithPath: "/fixture"), name: "fixture", isDirectory: true,
                    size: 10_040, allocatedSize: 10_040, fileCount: 41, children: children)
let segments = SunburstLayout.make(root: root)
precondition(segments.count == 2)
let large = segments.first { $0.node != nil }!
let group = segments.first { $0.node == nil }!
precondition(group.members.count == 40 && group.size == 40)
precondition(group.matches(children.last!.id) && !group.matches(large.id))
precondition(Set(segments.flatMap { $0.members.map(\.id) }) == Set(children.map(\.id)))
print("PASS every grouped member remains available for drag and row highlighting")

let metrics = SunburstMetrics(size: CGSize(width: 400, height: 400))
func point(_ sector: SunburstSegment) -> CGPoint {
    let radius = metrics.innerRadius + (CGFloat(sector.depth) + 0.5) * metrics.ringWidth
    let angle = (sector.start + sector.end) / 2 - .pi / 2
    return CGPoint(x: metrics.center.x + cos(angle) * radius,
                   y: metrics.center.y + sin(angle) * radius)
}
let partialCoverage = CollectionCoverage(paths: [group.members[0].id])
let groupedCoverage = CollectionCoverage(paths: group.members.map(\.id))
let parentCoverage = CollectionCoverage(paths: [root.id])
precondition(group.collectionState(using: CollectionCoverage()) == .none)
precondition(group.collectionState(using: partialCoverage) == .containsItems)
precondition(group.collectionState(using: groupedCoverage) == .included)
precondition(large.collectionState(using: groupedCoverage) == .none)
precondition(group.collectionState(using: parentCoverage) == .included)
precondition(large.collectionState(using: parentCoverage) == .included)
precondition(group.collectionMarkerCenter(metrics: metrics, diameter: 16) == nil)
let markerCenter = large.collectionMarkerCenter(metrics: metrics, diameter: 16)!
precondition(large.path(metrics: metrics).contains(markerCenter))
print("PASS collection markings distinguish partial groups, full groups, and collected ancestors; tiny sectors omit badges")

let presentationCache = SunburstPresentationCache()
let mapRevision = UUID()
let prepared = presentationCache.presentation(root: root, mapRevision: mapRevision, coverage: CollectionCoverage())
let preparedGroup = prepared.layout.hoverGroup(for: group.id)!
precondition(preparedGroup.id == group.id && preparedGroup.parentID == root.id && preparedGroup.rootID == root.id)
precondition(preparedGroup.mapRevision == mapRevision && preparedGroup.memberIDs == Set(group.members.map(\.id)))
precondition(prepared.layout.hoverGroup(for: large.id) == nil && prepared.layout.hoverGroup(for: nil) == nil)
precondition(prepared.layout.segment(for: children.last!.id)?.id == group.id)
precondition(prepared.layout.segment(for: large.id)?.id == large.id)
precondition(prepared.layout.segment(for: nil) == nil && prepared.layout.segment(for: "/absent") == nil)
precondition(prepared.collectionStates.allSatisfy { $0 == .none })
for _ in 0..<50 {
    let reused = presentationCache.presentation(root: root, mapRevision: mapRevision, coverage: CollectionCoverage())
    precondition(reused.layout === prepared.layout)
    precondition(reused.layout.segment(for: children.last!.id)?.members.count == 40)
}
let marked = presentationCache.presentation(root: root, mapRevision: mapRevision, coverage: partialCoverage)
precondition(marked.layout === prepared.layout)
precondition(marked.collectionState(for: group.id) == .containsItems && marked.collectionState(for: large.id) == .none)
precondition(marked.rootCollectionState == .containsItems)
let allMarked = presentationCache.presentation(root: root, mapRevision: mapRevision, coverage: parentCoverage)
precondition(allMarked.layout === prepared.layout && allMarked.collectionStates.allSatisfy { $0 == .included })
let cleared = presentationCache.presentation(root: root, mapRevision: mapRevision, coverage: CollectionCoverage())
precondition(cleared.layout === prepared.layout && cleared.collectionStates.allSatisfy { $0 == .none })
var replacedChildren = children
replacedChildren[replacedChildren.count - 1] = leaf("replacement.bin", 1)
let replacedRoot = DiskNode(url: root.url, name: root.name, isDirectory: true, size: root.size,
                            allocatedSize: root.allocatedSize, fileCount: root.fileCount, children: replacedChildren)
let replacementRevision = UUID()
let refreshed = presentationCache.presentation(root: replacedRoot, mapRevision: replacementRevision, coverage: CollectionCoverage())
precondition(refreshed.layout !== prepared.layout)
precondition(refreshed.layout.segment(for: children.last!.id) == nil)
precondition(refreshed.layout.segment(for: replacedChildren.last!.id)?.id == group.id)
let refreshedGroup = refreshed.layout.hoverGroup(for: group.id)!
precondition(refreshedGroup.mapRevision == replacementRevision && !refreshedGroup.memberIDs.contains(children.last!.id)
             && refreshedGroup.memberIDs.contains(replacedChildren.last!.id))
let otherRoot = DiskNode(url: URL(fileURLWithPath: "/other"), name: "other", isDirectory: true,
                         size: root.size, allocatedSize: root.allocatedSize, fileCount: root.fileCount, children: children)
let other = presentationCache.presentation(root: otherRoot, mapRevision: replacementRevision, coverage: CollectionCoverage())
precondition(other.layout !== refreshed.layout)
precondition(other.layout.segment(for: children.last!.id)?.id == "group:/other")
var identityChildren = children
let originalMember = children.last!
identityChildren[identityChildren.count - 1] = DiskNode(url: originalMember.url, name: originalMember.name,
    isDirectory: false, size: originalMember.size, allocatedSize: originalMember.allocatedSize,
    fileCount: originalMember.fileCount, fileIdentity: "replacement-inode")
let identityRoot = DiskNode(url: root.url, name: root.name, isDirectory: true, size: root.size,
                            allocatedSize: root.allocatedSize, fileCount: root.fileCount, children: identityChildren)
let beforeIdentity = presentationCache.presentation(root: root, mapRevision: mapRevision, coverage: CollectionCoverage())
let afterIdentity = presentationCache.presentation(root: identityRoot, mapRevision: UUID(), coverage: CollectionCoverage())
precondition(beforeIdentity.layout !== afterIdentity.layout)
precondition(afterIdentity.layout.segment(for: originalMember.id)?.members.last?.fileIdentity == "replacement-inode")
print("PASS chart cache reuses exact groups across selection and collection changes, and refreshes equal-size replacement data")

let pendingFolder = DiskNode(url: URL(fileURLWithPath: "/fixture/waiting"), name: "waiting", isDirectory: true,
                              size: 0, allocatedSize: 0, fileCount: 0, scanState: .pending)
let scanningFolder = DiskNode(url: URL(fileURLWithPath: "/fixture/growing"), name: "growing", isDirectory: true,
                               size: 1, allocatedSize: 1, fileCount: 1, scanState: .scanning)
let partialRoot = DiskNode(url: root.url, name: root.name, isDirectory: true,
                           size: root.size + 1, allocatedSize: root.allocatedSize + 1, fileCount: root.fileCount + 1,
                           children: children + [pendingFolder, scanningFolder], scanState: .scanning)
let partialSegments = SunburstLayout.make(root: partialRoot)
let partialGroup = partialSegments.first { $0.node == nil }!
precondition(partialGroup.scanState == .scanning && partialGroup.members.contains { $0.id == scanningFolder.id })
precondition(!partialSegments.contains { $0.matches(pendingFolder.id) })
precondition(SunburstLayout.make(root: pendingFolder).isEmpty)
precondition(SunburstText.size(0, state: pendingFolder.scanState) == "Waiting…")
precondition(SunburstText.detail(root: pendingFolder, segment: nil, collection: .none) == "Waiting to map this folder")
precondition(SunburstText.detail(root: partialRoot, segment: nil, collection: .none) == "Mapped so far")
precondition(SunburstText.detail(root: partialRoot, segment: partialGroup, collection: .none) == "Mapped so far")
precondition(SunburstText.detail(root: partialRoot, segment: large, collection: .none).hasSuffix("of mapped bytes"))
precondition(SunburstText.detail(root: root, segment: large, collection: .none).hasSuffix("of this folder"))
print("PASS pending folders have no invented arc or zero-size claim, and partial groups and totals stay explicitly incomplete")

let colorA = leaf("color-a", 200)
let colorB = leaf("color-b", 100)
let colorRoot = DiskNode(url: root.url, name: root.name, isDirectory: true, size: 300, allocatedSize: 300,
                         fileCount: 2, children: [colorA, colorB], scanState: .scanning)
let reorderedRoot = DiskNode(url: root.url, name: root.name, isDirectory: true, size: 300, allocatedSize: 300,
                             fileCount: 2, children: [colorB, colorA], scanState: .scanning)
let stableColorIndices = [colorA.id: 0, colorB.id: 8]
let colorsBefore = SunburstLayout.make(root: colorRoot, colorIndices: stableColorIndices)
let colorsAfter = SunburstLayout.make(root: reorderedRoot, colorIndices: stableColorIndices)
for node in [colorA, colorB] {
    precondition(colorsBefore.first { $0.id == node.id }!.branchIndex == stableColorIndices[node.id])
    precondition(colorsAfter.first { $0.id == node.id }!.branchIndex == stableColorIndices[node.id])
}
precondition(colorsAfter[0].branchIndex != colorsAfter[1].branchIndex)
precondition(group.branchIndex != large.branchIndex)
print("PASS branch colors follow stable folder IDs after reordering, and shared palette colors retain separate branch identities")

let largePoint = point(large)
let groupPoint = point(group)
precondition(large.path(metrics: metrics).contains(largePoint))
precondition(group.path(metrics: metrics).contains(groupPoint))
let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 500, height: 500), styleMask: [], backing: .buffered, defer: false)
let container = NSView(frame: NSRect(x: 0, y: 0, width: 500, height: 500))
let view = MapInteractionNSView(frame: NSRect(x: 30, y: 40, width: 400, height: 400))
container.addSubview(view)
window.contentView = container
var activated: [String] = []
var requestedDrags: [String] = []
var hovered: [String?] = []
var hoveredMembers: [[String]] = []
var chartActivity: [Bool] = []
func configuration(rootID: String = root.id, layoutRevision: UUID = mapRevision,
                   sectors: [SunburstSegment] = segments, chartMetrics: SunburstMetrics = metrics,
                   canGoBack: Bool = true) -> MapInteractionView {
    MapInteractionView(rootID: rootID, layoutRevision: layoutRevision, segments: sectors, metrics: chartMetrics, canGoBack: canGoBack,
                       onHover: { hovered.append($0?.id); hoveredMembers.append($0?.members.map(\.id) ?? []) },
                       onActivate: { activated.append($0.id) },
                       makeDrag: { requestedDrags.append($0.id); return nil }, onDragEnded: {},
                       onInteractionActive: { chartActivity.append($0) })
}
view.interaction = configuration()
precondition(view.hitTest(view.convert(metrics.center, to: container)) == nil)
precondition(view.hitTest(view.convert(largePoint, to: container)) === view)
print("PASS chart event overlay preserves the center button hit target")

let initialHitRegions = view.hitRegions!
for _ in 0..<20 {
    view.interaction = configuration()
    precondition(view.hitRegions === initialHitRegions)
}
view.interaction = configuration(canGoBack: false)
precondition(view.hitRegions === initialHitRegions)
view.interaction = configuration(chartMetrics: SunburstMetrics(size: CGSize(width: 360, height: 210)))
let resizedHitRegions = view.hitRegions!
precondition(resizedHitRegions !== initialHitRegions)
view.interaction = configuration(rootID: "/changed", chartMetrics: SunburstMetrics(size: CGSize(width: 360, height: 210)))
let newRootHitRegions = view.hitRegions!
precondition(newRootHitRegions !== resizedHitRegions)
view.interaction = configuration(rootID: "/changed", layoutRevision: UUID(),
                                 chartMetrics: SunburstMetrics(size: CGSize(width: 360, height: 210)))
precondition(view.hitRegions !== newRootHitRegions)
view.interaction = nil
precondition(view.hitRegions == nil)
view.interaction = configuration()
print("PASS hit paths reuse the same layout and refresh on resize, root, revision, and removal")

let boundarySegments = (0..<72).map { index in
    let sector = index % 24
    return SunburstSegment(id: "boundary-\(index)", node: large.node, members: large.members,
                           name: "Boundary", size: 1,
                           start: Double(sector) * 2 * .pi / 24, end: Double(sector + 1) * 2 * .pi / 24,
                           depth: index / 24, branchIndex: sector)
}
for testMetrics in [metrics, SunburstMetrics(size: CGSize(width: 360, height: 210))] {
    for testSegments in [segments, boundarySegments] {
        let regions = MapHitRegions(segments: testSegments, metrics: testMetrics)
        var probes: [CGPoint] = [testMetrics.center, .zero, CGPoint(x: 500, y: 500)]
        for x in stride(from: 0, through: 400, by: 8) {
            for y in stride(from: 0, through: 400, by: 8) {
                probes.append(CGPoint(x: x, y: y))
            }
        }
        for segment in testSegments {
            let inner = testMetrics.innerRadius + CGFloat(segment.depth) * testMetrics.ringWidth + 1.4
            let outer = inner + max(1, testMetrics.ringWidth - 2.8)
            let inset = min((segment.end - segment.start) * 0.17, 1.2 / Double((inner + outer) / 2))
            for angle in [segment.start, segment.start + inset, (segment.start + segment.end) / 2,
                          segment.end - inset, segment.end] {
                for radius in [inner - 0.0001, inner, inner + 0.0001, (inner + outer) / 2,
                               outer - 0.0001, outer, outer + 0.0001] {
                    probes.append(CGPoint(x: testMetrics.center.x + cos(angle - .pi / 2) * radius,
                                          y: testMetrics.center.y + sin(angle - .pi / 2) * radius))
                }
            }
        }
        for probe in probes {
            precondition(regions.index(at: probe) == testSegments.firstIndex { $0.path(metrics: testMetrics).contains(probe) },
                         "Cached hit path changed a sector or gap boundary at \(probe)")
        }
    }
}
print("PASS cached hit regions preserve exact sector, radial gap, angular gap, center, and outer boundaries")

func event(_ type: NSEvent.EventType, _ point: CGPoint) -> NSEvent {
    NSEvent.mouseEvent(with: type, location: view.convert(point, to: nil), modifierFlags: [], timestamp: 0,
                      windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
}
view.mouseDown(with: event(.leftMouseDown, largePoint))
view.mouseUp(with: event(.leftMouseUp, largePoint))
precondition(activated == [large.id])
let nearby = CGPoint(x: largePoint.x + 2, y: largePoint.y + 2)
view.mouseDown(with: event(.leftMouseDown, largePoint))
view.mouseDragged(with: event(.leftMouseDragged, nearby))
view.mouseUp(with: event(.leftMouseUp, nearby))
precondition(activated == [large.id, large.id] && requestedDrags.isEmpty)
print("PASS click and minor mouse movement retain ordinary navigation")

view.mouseDown(with: event(.leftMouseDown, largePoint))
view.mouseDragged(with: event(.leftMouseDragged, groupPoint))
precondition(chartActivity.last == true)
view.mouseUp(with: event(.leftMouseUp, groupPoint))
precondition(requestedDrags == [large.id] && activated.count == 2)
view.mouseDown(with: event(.leftMouseDown, groupPoint))
view.mouseDragged(with: event(.leftMouseDragged, largePoint))
view.mouseUp(with: event(.leftMouseUp, largePoint))
precondition(requestedDrags == [large.id, group.id] && activated.count == 2)
print("PASS drag uses the pressed sector, including groups, and rejected drag never opens a folder")

view.mouseDown(with: event(.leftMouseDown, largePoint))
view.interaction = configuration(rootID: "/changed")
view.mouseDragged(with: event(.leftMouseDragged, groupPoint))
view.mouseUp(with: event(.leftMouseUp, largePoint))
precondition(requestedDrags.count == 2 && activated.count == 2)
precondition(chartActivity == (0..<5).flatMap { _ in [true, false] })
print("PASS navigation during a mouse gesture cannot activate stale sectors")
view.interaction = configuration()
view.mouseMoved(with: event(.mouseMoved, groupPoint))
view.mouseExited(with: event(.mouseMoved, groupPoint))
precondition(hovered.suffix(2).first! == group.id && hovered.last! == nil)
print("PASS native hover reports grouped sectors and clears on exit")
view.mouseMoved(with: event(.mouseMoved, groupPoint))
let beforeRepeatedHover = hovered.count
for _ in 0..<20 {
    view.interaction = configuration()
    view.mouseMoved(with: event(.mouseMoved, groupPoint))
}
precondition(hovered.count == beforeRepeatedHover)
view.interaction = configuration(layoutRevision: refreshed.layout.revision, sectors: refreshed.layout.segments)
view.mouseMoved(with: event(.mouseMoved, groupPoint))
precondition(hovered.count == beforeRepeatedHover + 1)
precondition(hoveredMembers.last!.contains(replacedChildren.last!.id) && !hoveredMembers.last!.contains(children.last!.id))
view.interaction = configuration()
print("PASS repeated native hover stays quiet while a new map revision refreshes exact grouped members")

final class RowTableDataSource: NSObject, NSTableViewDataSource {
    func numberOfRows(in tableView: NSTableView) -> Int { 1 }
    func tableView(_ tableView: NSTableView, objectValueFor tableColumn: NSTableColumn?, row: Int) -> Any? { "Fixture item" }
}
let rowDataSource = RowTableDataSource()
let table = NSTableView(frame: NSRect(x: 0, y: 0, width: 320, height: 240))
table.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name")))
table.dataSource = rowDataSource
table.rowHeight = 52
table.reloadData()
let row = ListRowInteractionNSView(frame: NSRect(x: 0, y: 0, width: 300, height: 52))
table.addSubview(row)
let rowScroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 320, height: 240))
rowScroll.documentView = table
container.addSubview(rowScroll)
var selectedRows: [String] = []
var openedRows: [String] = []
var draggedRows: [String] = []
var draggedRowSizes: [Int64] = []
var rowActivity: [Bool] = []
func rowConfiguration(_ node: DiskNode = children[0]) -> ListRowInteractionView {
    ListRowInteractionView(node: node, trailingExclusion: 43,
                           onSelect: { selectedRows.append($0.id) }, onOpen: { openedRows.append($0.id) },
                           makeDrag: { nodes, _ in
                               draggedRows.append(contentsOf: nodes.map(\.id))
                               draggedRowSizes.append(contentsOf: nodes.map(\.size))
                               return nil
                           },
                           onDragEnded: {}, onInteractionActive: { rowActivity.append($0) })
}
func rowEvent(_ type: NSEvent.EventType, _ point: CGPoint, count: Int = 1,
              modifiers: NSEvent.ModifierFlags = []) -> NSEvent {
    NSEvent.mouseEvent(with: type, location: row.convert(point, to: nil), modifierFlags: modifiers, timestamp: 0,
                      windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: count, pressure: 1)!
}
row.interaction = rowConfiguration()
let rowPoint = CGPoint(x: 100, y: 26)
precondition(row.hitTest(row.convert(rowPoint, to: table)) === row)
precondition(row.hitTest(row.convert(CGPoint(x: 280, y: 26), to: table)) == nil)
precondition(ListRowInteractionNSView.isContextClick(rowEvent(.rightMouseDown, rowPoint)))
precondition(ListRowInteractionNSView.isContextClick(rowEvent(.leftMouseDown, rowPoint, modifiers: .control)))
precondition(!ListRowInteractionNSView.isContextClick(rowEvent(.leftMouseDown, rowPoint)))
row.mouseDown(with: rowEvent(.leftMouseDown, rowPoint))
precondition(window.firstResponder === table)
row.mouseUp(with: rowEvent(.leftMouseUp, rowPoint))
precondition(selectedRows == [children[0].id] && openedRows.isEmpty)
print("PASS list-row click selects and restores table focus while arrow and context-click targets remain native")

row.mouseDown(with: rowEvent(.leftMouseDown, rowPoint, count: 2))
row.mouseDragged(with: rowEvent(.leftMouseDragged, CGPoint(x: 102, y: 28), count: 2))
row.mouseUp(with: rowEvent(.leftMouseUp, CGPoint(x: 102, y: 28), count: 2))
precondition(openedRows == [children[0].id] && draggedRows.isEmpty)
row.mouseDown(with: rowEvent(.leftMouseDown, rowPoint, count: 2))
row.mouseDragged(with: rowEvent(.leftMouseDragged, CGPoint(x: 100, y: 90), count: 2))
precondition(rowActivity.last == true)
row.mouseUp(with: rowEvent(.leftMouseUp, rowPoint, count: 2))
precondition(draggedRows == [children[0].id] && openedRows.count == 1)
print("PASS double-click opens once; a rejected row drag cannot trigger double-click navigation")

row.mouseDown(with: rowEvent(.leftMouseDown, rowPoint))
let changedSizeNode = leaf("large.bin", 99_999)
row.interaction = rowConfiguration(changedSizeNode)
row.mouseDragged(with: rowEvent(.leftMouseDragged, CGPoint(x: 110, y: 26)))
row.mouseUp(with: rowEvent(.leftMouseUp, rowPoint))
precondition(draggedRows == [children[0].id, children[0].id] && draggedRowSizes == [children[0].size, children[0].size])
row.interaction = rowConfiguration()
row.mouseDown(with: rowEvent(.leftMouseDown, rowPoint))
row.interaction = rowConfiguration(children[1])
row.mouseDragged(with: rowEvent(.leftMouseDragged, CGPoint(x: 110, y: 26)))
row.mouseUp(with: rowEvent(.leftMouseUp, rowPoint))
precondition(draggedRows.count == 2 && openedRows.count == 1)
print("PASS row drag holds its pressed item and reused rows cannot drag or open a replacement item")
precondition(rowActivity == (0..<5).flatMap { _ in [true, false] })
row.interaction = rowConfiguration(children[1])
row.mouseDown(with: rowEvent(.leftMouseDown, rowPoint, count: 2))
row.mouseUp(with: rowEvent(.leftMouseUp, rowPoint, count: 2))
precondition(openedRows == [children[0].id])
precondition(rowActivity.suffix(2) == [true, false])
print("PASS a row reordered between the two clicks is selected without opening the replacement item")

view.interaction = configuration()
let chartActivityCount = chartActivity.count
view.mouseDown(with: event(.leftMouseDown, largePoint))
var replacementChartActivity: [Bool] = []
var newChartConfiguration = configuration()
newChartConfiguration.onInteractionActive = { replacementChartActivity.append($0) }
view.interaction = newChartConfiguration
view.mouseUp(with: event(.leftMouseUp, largePoint))
precondition(Array(chartActivity.dropFirst(chartActivityCount)) == [true, false] && replacementChartActivity.isEmpty)
view.interaction = configuration()
view.mouseDown(with: event(.leftMouseDown, largePoint))
view.removeFromSuperview()
MapInteractionView.dismantleNSView(view, coordinator: ())
pumpMainRunLoop { chartActivity.count == chartActivityCount + 4 }
precondition(Array(chartActivity.dropFirst(chartActivityCount)) == [true, false, true, false])
view.cancelInteraction()
precondition(chartActivity.count == chartActivityCount + 4)
print("PASS chart gesture holds balance through clicks, rejected drags, callback replacement, and view removal")

row.interaction = rowConfiguration()
let rowActivityCount = rowActivity.count
row.mouseDown(with: rowEvent(.leftMouseDown, rowPoint))
var replacementRowActivity: [Bool] = []
var newRowConfiguration = rowConfiguration()
newRowConfiguration.onInteractionActive = { replacementRowActivity.append($0) }
row.interaction = newRowConfiguration
row.mouseUp(with: rowEvent(.leftMouseUp, rowPoint))
precondition(Array(rowActivity.dropFirst(rowActivityCount)) == [true, false] && replacementRowActivity.isEmpty)
row.interaction = rowConfiguration()
row.mouseDown(with: rowEvent(.leftMouseDown, rowPoint))
row.removeFromSuperview()
ListRowInteractionView.dismantleNSView(row, coordinator: ())
pumpMainRunLoop { rowActivity.count == rowActivityCount + 4 }
precondition(Array(rowActivity.dropFirst(rowActivityCount)) == [true, false, true, false],
             "Unexpected row activity after removal: \(Array(rowActivity.dropFirst(rowActivityCount)))")
row.cancelInteraction()
precondition(rowActivity.count == rowActivityCount + 4)
print("PASS row gesture holds balance through double-clicks, rejected drags, reuse, callback replacement, and view removal")
