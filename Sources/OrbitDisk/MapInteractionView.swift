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
    let onDragEnded: () -> Void
    var onInteractionActive: (Bool) -> Void = { _ in }

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
    private var movedBeyondClick = false
    private var isDragging = false
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
        cancelInteraction()
        pressedEvent = event
        pressedPoint = convert(event.locationInWindow, from: nil)
        pressedSegment = segment(at: pressedPoint)
        pressedRootID = interaction?.rootID
        movedBeyondClick = false
        if pressedSegment != nil, let onActivity = interaction?.onInteractionActive {
            interactionCompletion = { onActivity(false) }
            onActivity(true)
        }
        updateHover(with: event)
    }

    override func mouseDragged(with event: NSEvent) {
        guard !isDragging, !movedBeyondClick, let pressedSegment, let pressedEvent, let interaction,
              pressedRootID == interaction.rootID else { return }
        let point = convert(event.locationInWindow, from: nil)
        guard hypot(point.x - pressedPoint.x, point.y - pressedPoint.y) >= 5 else { return }
        movedBeyondClick = true
        guard let pasteboardItem = interaction.makeDrag(pressedSegment) else { return }

        let preview = dragPreview(for: pressedSegment)
        let item = NSDraggingItem(pasteboardWriter: pasteboardItem)
        let origin = CGPoint(x: min(max(0, pressedPoint.x + 12), max(0, bounds.width - preview.size.width)),
                             y: min(max(0, pressedPoint.y - preview.size.height / 2), max(0, bounds.height - preview.size.height)))
        item.setDraggingFrame(NSRect(origin: origin, size: preview.size), contents: preview)
        isDragging = true
        dragCompletion = interaction.onDragEnded
        let session = beginDraggingSession(with: [item], event: pressedEvent, source: self)
        session.animatesToStartingPositionsOnCancelOrFail = true
    }

    override func mouseUp(with event: NSEvent) {
        guard !isDragging else { return }
        defer { resetPress(); finishInteraction() }
        guard !movedBeyondClick, let pressedSegment, let interaction,
              pressedRootID == interaction.rootID,
              segment(at: convert(event.locationInWindow, from: nil))?.id == pressedSegment.id else { return }
        interaction.onActivate(pressedSegment)
    }

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        context == .withinApplication ? .copy : []
    }

    func ignoreModifierKeys(for session: NSDraggingSession) -> Bool { true }

    func draggingSession(_ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
        isDragging = false
        resetPress()
        setHovered(nil)
        let completion = dragCompletion
        dragCompletion = nil
        completion?()
        finishInteraction()
    }

    func cancelInteraction(deferNotification: Bool = false) {
        isDragging = false
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

    private func finishInteraction() {
        let completion = interactionCompletion
        interactionCompletion = nil
        completion?()
    }

    private func resetPress() {
        pressedSegment = nil
        pressedEvent = nil
        pressedRootID = nil
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

    init(segments: [SunburstSegment], metrics: SunburstMetrics) {
        paths = segments.map { $0.path(metrics: metrics) }
    }

    func index(at point: CGPoint) -> Int? {
        paths.firstIndex { $0.contains(point) }
    }
}
