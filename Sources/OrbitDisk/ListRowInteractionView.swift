import AppKit
import SwiftUI
import OrbitCore

struct ListRowInteractionView: NSViewRepresentable {
    let node: DiskNode
    var trailingExclusion: CGFloat = 0
    var contentWidth: CGFloat?
    let onSelect: (DiskNode) -> Void
    let onOpen: (DiskNode) -> Void
    let makeDrag: ([DiskNode], String) -> NSPasteboardItem?
    let onDragEnded: () -> Void
    var onInteractionActive: (Bool) -> Void = { _ in }

    func makeNSView(context: Context) -> ListRowInteractionNSView {
        let view = ListRowInteractionNSView()
        view.setAccessibilityElement(false)
        return view
    }

    func updateNSView(_ view: ListRowInteractionNSView, context: Context) {
        view.interaction = self
    }

    static func dismantleNSView(_ view: ListRowInteractionNSView, coordinator: ()) {
        view.cancelInteraction(deferNotification: true)
    }
}

final class ListRowInteractionNSView: NSView, NSDraggingSource {
    var interaction: ListRowInteractionView? {
        didSet {
            if oldValue?.trailingExclusion != interaction?.trailingExclusion || oldValue?.contentWidth != interaction?.contentWidth {
                window?.invalidateCursorRects(for: self)
            }
        }
    }
    private var pressedNode: DiskNode?
    private var pressedEvent: NSEvent?
    private var pressedPoint = CGPoint.zero
    private var opensOnMouseUp = false
    private var lastClick: (nodeID: String, timestamp: TimeInterval)?
    private var movedBeyondClick = false
    private var isDragging = false
    private var interactionCompletion: (() -> Void)?
    private var dragCompletion: (() -> Void)?

    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { false }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil { cancelInteraction(deferNotification: true) }
    }

    private var excludedBounds: NSRect {
        let end = max(bounds.minX, min(bounds.maxX, bounds.minX + (interaction?.contentWidth ?? bounds.width)))
        let start = max(bounds.minX, end - (interaction?.trailingExclusion ?? 0))
        return NSRect(x: start, y: bounds.minY, width: end - start, height: bounds.height)
    }

    private func containsInteractionPoint(_ point: NSPoint) -> Bool {
        bounds.contains(point) && !excludedBounds.contains(point)
    }

    static func isContextClick(_ event: NSEvent?) -> Bool {
        guard let event else { return false }
        return event.type == .rightMouseDown || event.type == .rightMouseUp || event.type == .rightMouseDragged
            || ((event.type == .leftMouseDown || event.type == .leftMouseUp) && event.modifierFlags.contains(.control))
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !Self.isContextClick(NSApp.currentEvent),
              containsInteractionPoint(convert(point, from: superview)) else { return nil }
        return self
    }

    override func resetCursorRects() {
        super.resetCursorRects()
        let excluded = excludedBounds
        let left = NSRect(x: bounds.minX, y: bounds.minY, width: excluded.minX - bounds.minX, height: bounds.height).intersection(visibleRect)
        let right = NSRect(x: excluded.maxX, y: bounds.minY, width: bounds.maxX - excluded.maxX, height: bounds.height).intersection(visibleRect)
        if !left.isEmpty { addCursorRect(left, cursor: .pointingHand) }
        if !right.isEmpty { addCursorRect(right, cursor: .pointingHand) }
    }

    override func mouseDown(with event: NSEvent) {
        cancelInteraction()
        if Self.isContextClick(event) {
            tableView?.mouseDown(with: event)
            return
        }
        guard let interaction else { return }
        let point = convert(event.locationInWindow, from: nil)
        guard containsInteractionPoint(point) else { return }
        pressedNode = interaction.node
        pressedEvent = event
        pressedPoint = point
        movedBeyondClick = false
        if let lastClick {
            let interval = event.timestamp - lastClick.timestamp
            opensOnMouseUp = event.clickCount == 2 && lastClick.nodeID == interaction.node.id
                && interval >= 0 && interval <= NSEvent.doubleClickInterval
        }
        let onActivity = interaction.onInteractionActive
        interactionCompletion = { onActivity(false) }
        onActivity(true)
        if let tableView { window?.makeFirstResponder(tableView) }
        interaction.onSelect(interaction.node)
    }

    override func mouseDragged(with event: NSEvent) {
        guard !isDragging, !movedBeyondClick, let pressedNode, let pressedEvent, let interaction,
              pressedNode.id == interaction.node.id else { return }
        let point = convert(event.locationInWindow, from: nil)
        guard hypot(point.x - pressedPoint.x, point.y - pressedPoint.y) >= 5 else { return }
        movedBeyondClick = true
        guard let pasteboardItem = interaction.makeDrag([pressedNode], pressedNode.name) else { return }

        let preview = dragPreview(for: pressedNode)
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
        guard !movedBeyondClick, let pressedNode, let pressedEvent, let interaction,
              pressedNode.id == interaction.node.id,
              containsInteractionPoint(convert(event.locationInWindow, from: nil)) else { return }
        lastClick = (pressedNode.id, pressedEvent.timestamp)
        if opensOnMouseUp { interaction.onOpen(pressedNode) }
    }

    override func rightMouseDown(with event: NSEvent) {
        if let tableView { tableView.rightMouseDown(with: event) }
        else { super.rightMouseDown(with: event) }
    }

    override func scrollWheel(with event: NSEvent) {
        if let scrollView = enclosingScrollView { scrollView.scrollWheel(with: event) }
        else { super.scrollWheel(with: event) }
    }

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        context == .withinApplication ? .copy : []
    }

    func ignoreModifierKeys(for session: NSDraggingSession) -> Bool { true }

    func draggingSession(_ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
        isDragging = false
        resetPress()
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

    private var tableView: NSTableView? {
        var ancestor = superview
        while let view = ancestor {
            if let table = view as? NSTableView { return table }
            ancestor = view.superview
        }
        return nil
    }

    private func resetPress() {
        pressedNode = nil
        pressedEvent = nil
        opensOnMouseUp = false
        movedBeyondClick = false
    }

    private func dragPreview(for node: DiskNode) -> NSImage {
        let size = NSSize(width: min(240, bounds.width), height: min(56, bounds.height))
        return NSImage(size: size, flipped: false) { rect in
            NSColor(calibratedWhite: 0.14, alpha: 0.98).setFill()
            let background = NSBezierPath(roundedRect: rect.insetBy(dx: 1, dy: 1), xRadius: 7, yRadius: 7)
            background.fill()
            NSColor(calibratedWhite: 0.52, alpha: 1).setStroke()
            background.stroke()
            NSImage(systemSymbolName: node.isDirectory ? "folder.fill" : "doc.fill", accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(paletteColors: [.white]))?
                .draw(in: NSRect(x: 12, y: (rect.height - 20) / 2, width: 20, height: 20))
            let paragraph = NSMutableParagraphStyle()
            paragraph.lineBreakMode = .byTruncatingMiddle
            (node.name as NSString).draw(in: NSRect(x: 42, y: rect.height / 2, width: rect.width - 52, height: 17),
                                        withAttributes: [.font: NSFont.systemFont(ofSize: 12, weight: .medium),
                                                         .foregroundColor: NSColor.white, .paragraphStyle: paragraph])
            let bytes = ByteCountFormatter.string(fromByteCount: max(0, node.size), countStyle: .file)
            (bytes as NSString).draw(in: NSRect(x: 42, y: rect.height / 2 - 16, width: rect.width - 52, height: 14),
                                    withAttributes: [.font: NSFont.systemFont(ofSize: 10),
                                                     .foregroundColor: NSColor(calibratedWhite: 0.78, alpha: 1)])
            return true
        }
    }
}
