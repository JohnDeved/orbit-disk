import AppKit
import SwiftUI

struct MapInteractionView: NSViewRepresentable {
    let rootID: String
    let layoutRevision: UUID
    let segments: [SunburstSegment]
    let metrics: SunburstMetrics
    let canGoBack: Bool
    let onHover: (SunburstSegment?) -> Void
    let onActivate: (SunburstSegment) -> Void
    let makeDrag: (SunburstSegment) -> NSPasteboardItem?
    let onDragEnded: (NSPasteboardItem) -> Void
    var beginInteraction: () -> (() -> Void) = { {} }

    func makeNSView(context: Context) -> MapInteractionNSView {
        let view = MapInteractionNSView()
        view.setAccessibilityElement(false)
        return view
    }

    func updateNSView(_ view: MapInteractionNSView, context: Context) {
        view.interaction = self
    }

    static func dismantleNSView(_ view: MapInteractionNSView, coordinator: ()) {
        view.cancelInteraction(deferNotification: true)
    }
}

final class MapInteractionNSView: NSView, NSDraggingSource {
    #if ORBIT_TESTING
    var testLastDragSession: NSDraggingSession?
    #endif
    var interaction: MapInteractionView? {
        didSet {
            let layoutChanged = oldValue?.layoutRevision != interaction?.layoutRevision ||
                oldValue?.rootID != interaction?.rootID || oldValue?.metrics != interaction?.metrics
            if layoutChanged {
                hitRegions = interaction.map { MapHitRegions(segments: $0.segments, metrics: $0.metrics) }
            }
            if layoutChanged || oldValue?.canGoBack != interaction?.canGoBack {
                scheduleHoverRefresh()
            }
        }
    }
    private(set) var hitRegions: MapHitRegions?
    private var tracking: NSTrackingArea?
    private var hoveredSegment: SunburstSegment?
    private var hoveredLayoutRevision: UUID?
    private var hoverRefreshScheduled = false
    private var pressedSegment: SunburstSegment?
    private var pressedEvent: NSEvent?
    private var pressedPoint = CGPoint.zero
    private var pressedRootID: String?
    private var gestureEpoch: UInt64 = 0
    private var pressedEpoch: UInt64?
    private var movedBeyondClick = false
    private var isDragging = false
    private weak var activeDragSession: NSDraggingSession?
    private var pendingDragStart: UUID?
    private var endedDuringStart: [NSDraggingSession] = []
    private var interactionCompletion: (() -> Void)?
    private var dragCompletion: (() -> Void)?

    override var isFlipped: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func hitTest(_ point: NSPoint) -> NSView? {
        let local = convert(point, from: superview)
        guard bounds.contains(local), let interaction else { return nil }
        let metrics = interaction.metrics
        guard hypot(local.x - metrics.center.x, local.y - metrics.center.y) > metrics.innerRadius else { return nil }
        return self
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil { cancelInteraction(deferNotification: true) }
        scheduleHoverRefresh()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(rect: .zero,
                                  options: [.mouseEnteredAndExited, .mouseMoved, .cursorUpdate, .activeInKeyWindow, .inVisibleRect],
                                  owner: self, userInfo: nil)
        addTrackingArea(area)
        tracking = area
    }

    override func mouseEntered(with event: NSEvent) { updateHover(with: event) }
    override func mouseMoved(with event: NSEvent) { updateHover(with: event) }
    override func mouseExited(with event: NSEvent) {
        guard !isDragging else { return }
        setHovered(nil)
    }

    override func cursorUpdate(with event: NSEvent) {
        guard !isDragging else { return }
        let point = convert(event.locationInWindow, from: nil)
        cursor(at: point, segment: segment(at: point)).set()
    }

    override func mouseDown(with event: NSEvent) {
        gestureEpoch &+= 1
        let epoch = gestureEpoch
        cancelInteraction(invalidateEpoch: false)
        guard gestureEpoch == epoch else { return }
        pressedEvent = event
        pressedPoint = convert(event.locationInWindow, from: nil)
        pressedSegment = segment(at: pressedPoint)
        pressedRootID = interaction?.rootID
        pressedEpoch = epoch
        movedBeyondClick = false
        if pressedSegment != nil, let interaction {
            let completion = interaction.beginInteraction()
            guard gestureEpoch == epoch, pressedEpoch == epoch else { completion(); return }
            interactionCompletion = completion
        }
        updateHover(with: event)
    }

    override func mouseDragged(with event: NSEvent) {
        guard !isDragging, !movedBeyondClick, let pressedSegment, let pressedEvent, let interaction, let epoch = pressedEpoch,
              pressedRootID == interaction.rootID else { return }
        let point = convert(event.locationInWindow, from: nil)
        guard hypot(point.x - pressedPoint.x, point.y - pressedPoint.y) >= 5 else { return }
        movedBeyondClick = true
        guard let pasteboardItem = interaction.makeDrag(pressedSegment) else { return }
        guard gestureEpoch == epoch, pressedEpoch == epoch, !isDragging else {
            interaction.onDragEnded(pasteboardItem)
            return
        }

        let preview = dragPreview(for: pressedSegment)
        let item = NSDraggingItem(pasteboardWriter: pasteboardItem)
        let origin = CGPoint(x: min(max(0, pressedPoint.x + 12), max(0, bounds.width - preview.size.width)),
                             y: min(max(0, pressedPoint.y - preview.size.height / 2), max(0, bounds.height - preview.size.height)))
        item.setDraggingFrame(NSRect(origin: origin, size: preview.size), contents: preview)
        isDragging = true
        let onDragEnded = interaction.onDragEnded
        dragCompletion = { onDragEnded(pasteboardItem) }
        let start = UUID()
        pendingDragStart = start
        let session = beginDraggingSession(with: [item], event: pressedEvent, source: self)
        guard pendingDragStart == start && isDragging else { return }
        pendingDragStart = nil
        let endedEarly = endedDuringStart.contains { $0 === session }
        endedDuringStart.removeAll()
        activeDragSession = session
        #if ORBIT_TESTING
        testLastDragSession = session
        #endif
        session.animatesToStartingPositionsOnCancelOrFail = true
        if endedEarly { draggingSession(session, endedAt: .zero, operation: []) }
    }

    override func mouseUp(with event: NSEvent) {
        guard !isDragging else { return }
        let activated: (MapInteractionView, SunburstSegment)?
        if !movedBeyondClick, let pressedSegment, let interaction,
           pressedRootID == interaction.rootID,
           segment(at: convert(event.locationInWindow, from: nil))?.id == pressedSegment.id {
            activated = (interaction, pressedSegment)
        } else { activated = nil }
        let completion = interactionCompletion
        interactionCompletion = nil
        resetPress()
        if let (interaction, segment) = activated { interaction.onActivate(segment) }
        completion?()
    }

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        context == .withinApplication ? .copy : []
    }

    func ignoreModifierKeys(for session: NSDraggingSession) -> Bool { true }

    func draggingSession(_ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
        guard activeDragSession === session else {
            if pendingDragStart != nil { endedDuringStart.append(session) }
            return
        }
        activeDragSession = nil
        isDragging = false
        resetPress()
        let completion = dragCompletion
        dragCompletion = nil
        let interactionCompletion = self.interactionCompletion
        self.interactionCompletion = nil
        setHovered(nil)
        completion?()
        interactionCompletion?()
    }

    func cancelInteraction(deferNotification: Bool = false, invalidateEpoch: Bool = true) {
        if invalidateEpoch { gestureEpoch &+= 1 }
        isDragging = false
        activeDragSession = nil
        pendingDragStart = nil
        endedDuringStart.removeAll()
        resetPress()
        let dragCompletion = self.dragCompletion
        self.dragCompletion = nil
        let interactionCompletion = self.interactionCompletion
        self.interactionCompletion = nil
        guard dragCompletion != nil || interactionCompletion != nil else { return }
        let finish: () -> Void = { dragCompletion?(); interactionCompletion?() }
        if deferNotification { DispatchQueue.main.async(execute: finish) }
        else { finish() }
    }

    private func resetPress() {
        pressedSegment = nil
        pressedEvent = nil
        pressedRootID = nil
        pressedEpoch = nil
        movedBeyondClick = false
    }

    private func segment(at point: CGPoint) -> SunburstSegment? {
        guard let interaction, let index = hitRegions?.index(at: point) else { return nil }
        return interaction.segments[index]
    }

    private func cursor(at point: CGPoint, segment: SunburstSegment?) -> NSCursor {
        guard let interaction else { return .arrow }
        let metrics = interaction.metrics
        let inCenter = hypot(point.x - metrics.center.x, point.y - metrics.center.y) <= metrics.innerRadius
        return segment != nil || (inCenter && interaction.canGoBack) ? .pointingHand : .arrow
    }

    private func updateHover(with event: NSEvent) {
        guard !isDragging else { return }
        let point = convert(event.locationInWindow, from: nil)
        let segment = segment(at: point)
        setHovered(segment)
        cursor(at: point, segment: segment).set()
    }

    private func scheduleHoverRefresh() {
        guard !hoverRefreshScheduled else { return }
        hoverRefreshScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.hoverRefreshScheduled = false
            guard !self.isDragging else { return }
            guard let window = self.window, window.isKeyWindow, NSApp.isActive else {
                self.setHovered(nil)
                return
            }
            let point = self.convert(window.mouseLocationOutsideOfEventStream, from: nil)
            guard self.bounds.contains(point) else {
                self.setHovered(nil)
                return
            }
            let segment = self.segment(at: point)
            self.setHovered(segment)
            self.cursor(at: point, segment: segment).set()
        }
    }

    private func setHovered(_ segment: SunburstSegment?) {
        if hoveredSegment?.id == segment?.id {
            guard segment != nil, hoveredLayoutRevision != interaction?.layoutRevision else { return }
        }
        hoveredSegment = segment
        hoveredLayoutRevision = interaction?.layoutRevision
        interaction?.onHover(segment)
    }

    private func dragPreview(for segment: SunburstSegment) -> NSImage {
        NSImage(size: NSSize(width: 240, height: 56), flipped: false) { rect in
            NSColor(calibratedWhite: 0.14, alpha: 0.98).setFill()
            let background = NSBezierPath(roundedRect: rect.insetBy(dx: 1, dy: 1), xRadius: 8, yRadius: 8)
            background.fill()
            NSColor(calibratedWhite: 0.52, alpha: 1).setStroke()
            background.lineWidth = 1
            background.stroke()

            let symbol = segment.node.map { $0.isDirectory ? "folder.fill" : "doc.fill" } ?? "square.stack.fill"
            NSImage(systemSymbolName: symbol, accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(paletteColors: [.white]))?
                .draw(in: NSRect(x: 13, y: 18, width: 20, height: 20))
            let paragraph = NSMutableParagraphStyle()
            paragraph.lineBreakMode = .byTruncatingMiddle
            (segment.name as NSString).draw(in: NSRect(x: 44, y: 29, width: 182, height: 17),
                                            withAttributes: [.font: NSFont.systemFont(ofSize: 13, weight: .medium),
                                                             .foregroundColor: NSColor.white, .paragraphStyle: paragraph])
            let size = ByteCountFormatter.string(fromByteCount: max(0, segment.size), countStyle: .file)
            (size as NSString).draw(in: NSRect(x: 44, y: 11, width: 182, height: 15),
                                   withAttributes: [.font: NSFont.systemFont(ofSize: 11),
                                                    .foregroundColor: NSColor(calibratedWhite: 0.78, alpha: 1)])
            return true
        }
    }
}

final class MapHitRegions {
    private let paths: [Path]
    private let bounds: CGRect
    private let buckets: [[Int]]
    private static let gridSize = 32

    init(segments: [SunburstSegment], metrics: SunburstMetrics) {
        let paths = segments.map { $0.path(metrics: metrics) }
        // SwiftUI path bounds can be rounded slightly inward relative to hit testing.
        let padding = max(0.01, max(abs(metrics.center.x), abs(metrics.center.y)) * 0.00001)
        let pathBounds = paths.map { $0.boundingRect.insetBy(dx: -padding, dy: -padding) }
        let bounds = pathBounds.reduce(CGRect.null) { $0.union($1) }
        var buckets = Array(repeating: [Int](), count: Self.gridSize * Self.gridSize)
        if !bounds.isNull && bounds.width > 0 && bounds.height > 0 {
            for (index, rect) in pathBounds.enumerated() {
                guard !rect.isNull else { continue }
                let minX = Self.cell(rect.minX, origin: bounds.minX, length: bounds.width)
                let maxX = Self.cell(rect.maxX, origin: bounds.minX, length: bounds.width)
                let minY = Self.cell(rect.minY, origin: bounds.minY, length: bounds.height)
                let maxY = Self.cell(rect.maxY, origin: bounds.minY, length: bounds.height)
                for y in minY...maxY {
                    for x in minX...maxX { buckets[y * Self.gridSize + x].append(index) }
                }
            }
        }
        self.paths = paths
        self.bounds = bounds
        self.buckets = buckets
    }

    func index(at point: CGPoint) -> Int? {
        guard point.x.isFinite, point.y.isFinite, !bounds.isNull,
              bounds.width > 0, bounds.height > 0,
              point.x >= bounds.minX, point.x <= bounds.maxX,
              point.y >= bounds.minY, point.y <= bounds.maxY else { return nil }
        let x = Self.cell(point.x, origin: bounds.minX, length: bounds.width)
        let y = Self.cell(point.y, origin: bounds.minY, length: bounds.height)
        return buckets[y * Self.gridSize + x].first { paths[$0].contains(point) }
    }

    private static func cell(_ value: CGFloat, origin: CGFloat, length: CGFloat) -> Int {
        min(gridSize - 1, max(0, Int((value - origin) / length * CGFloat(gridSize))))
    }
}
