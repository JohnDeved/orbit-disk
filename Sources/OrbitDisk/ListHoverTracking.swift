import AppKit

final class ListHoverTracking {
    var onHoverChanged: ((String?) -> Void)?
    private(set) var isSuppressed = false
    private(set) weak var scrollView: NSScrollView?
    private var hoveredID: String?
    private var gestureActive = false
    private var momentumActive = false
    private var liveScrollActive = false
    private var lastClipOrigin = NSPoint.zero
    private var settleTimer: Timer?
    private var eventMonitor: Any?
    private var observers: [NSObjectProtocol] = []
    private let rows = NSMapTable<NSView, NSString>.weakToStrongObjects()
    private let pointerLocation: () -> NSPoint
    private let settleDelay: TimeInterval

    init(settleDelay: TimeInterval = 0.18, pointerLocation: @escaping () -> NSPoint = { NSEvent.mouseLocation }) {
        self.settleDelay = settleDelay
        self.pointerLocation = pointerLocation
    }

    deinit { removeObservers() }

    func register(_ row: NSView, id: String, table: NSTableView) {
        guard let scroll = table.enclosingScrollView else { return }
        if scrollView !== scroll { attach(to: scroll) }
        rows.setObject(id as NSString, forKey: row)
    }

    func unregister(_ row: NSView) { rows.removeObject(forKey: row) }

    func rowHoverChanged(id: String, inside: Bool) {
        guard scrollView != nil, !isSuppressed else { return }
        if inside { publish(id) }
        else if hoveredID == id { publish(nil) }
    }

    func detach() {
        removeObservers()
        scrollView = nil
        rows.removeAllObjects()
        cancelScrolling()
    }

    private func attach(to scroll: NSScrollView) {
        removeObservers()
        rows.removeAllObjects()
        gestureActive = false
        momentumActive = false
        liveScrollActive = false
        isSuppressed = false
        scrollView = scroll
        lastClipOrigin = scroll.contentView.bounds.origin
        scroll.contentView.postsBoundsChangedNotifications = true
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: NSView.boundsDidChangeNotification,
                                            object: scroll.contentView, queue: nil) { [weak self] _ in
            guard let self, let clip = self.scrollView?.contentView else { return }
            guard clip.bounds.origin != self.lastClipOrigin else { return }
            self.lastClipOrigin = clip.bounds.origin
            self.beginScrolling()
        })
        observers.append(center.addObserver(forName: NSScrollView.willStartLiveScrollNotification,
                                            object: scroll, queue: nil) { [weak self] _ in
            self?.liveScrollChanged(active: true)
        })
        observers.append(center.addObserver(forName: NSScrollView.didEndLiveScrollNotification,
                                            object: scroll, queue: nil) { [weak self] _ in
            self?.liveScrollChanged(active: false)
        })
        if let window = scroll.window {
            observers.append(center.addObserver(forName: NSWindow.didResignKeyNotification,
                                                object: window, queue: nil) { [weak self] _ in
                self?.cancelScrolling()
            })
        }
        eventMonitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { [weak self] event in
            self?.handle(event)
            return event
        }
    }

    func handle(_ event: NSEvent) {
        guard event.type == .scrollWheel, let scroll = scrollView,
              let window = scroll.window, event.window === window else { return }
        let point = scroll.convert(event.locationInWindow, from: nil)
        let inside = scroll.bounds.contains(point) && scroll.visibleRect.contains(point)
        guard inside || gestureActive || momentumActive else { return }
        scrollWheel(phase: event.phase, momentum: event.momentumPhase)
    }

    func scrollWheel(phase: NSEvent.Phase, momentum: NSEvent.Phase) {
        if !phase.intersection([.began, .changed, .stationary]).isEmpty { gestureActive = true }
        if !phase.intersection([.ended, .cancelled]).isEmpty { gestureActive = false }
        if !momentum.intersection([.began, .changed, .stationary]).isEmpty { momentumActive = true }
        if !momentum.intersection([.ended, .cancelled]).isEmpty { momentumActive = false }
        beginScrolling()
    }

    func liveScrollChanged(active: Bool) {
        liveScrollActive = active
        beginScrolling()
    }

    private func beginScrolling() {
        if !isSuppressed {
            isSuppressed = true
            publish(nil)
        }
        settleTimer?.invalidate()
        let timer = Timer(timeInterval: settleDelay, repeats: false) { [weak self] _ in self?.settleIfIdle() }
        settleTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    func settleIfIdle() {
        guard isSuppressed, !gestureActive, !momentumActive, !liveScrollActive else { return }
        settleTimer?.invalidate()
        settleTimer = nil
        isSuppressed = false
        guard let window = scrollView?.window, window.isKeyWindow, window.isVisible, window.attachedSheet == nil else {
            publish(nil)
            return
        }
        publish(rowID(at: window.convertPoint(fromScreen: pointerLocation())))
    }

    func rowID(at windowPoint: NSPoint) -> String? {
        guard let scroll = scrollView else { return nil }
        let clip = scroll.contentView
        let clipPoint = clip.convert(windowPoint, from: nil)
        guard clip.bounds.contains(clipPoint), clip.visibleRect.contains(clipPoint) else { return nil }
        for case let row as NSView in rows.keyEnumerator() {
            guard row.window === scroll.window, !row.isHiddenOrHasHiddenAncestor,
                  row.bounds.contains(row.convert(windowPoint, from: nil)),
                  row.visibleRect.contains(row.convert(windowPoint, from: nil)) else { continue }
            return rows.object(forKey: row) as String?
        }
        return nil
    }

    private func cancelScrolling() {
        settleTimer?.invalidate()
        settleTimer = nil
        gestureActive = false
        momentumActive = false
        liveScrollActive = false
        isSuppressed = false
        publish(nil)
    }

    private func publish(_ id: String?) {
        hoveredID = id
        onHoverChanged?(id)
    }

    private func removeObservers() {
        settleTimer?.invalidate()
        settleTimer = nil
        if let eventMonitor { NSEvent.removeMonitor(eventMonitor) }
        eventMonitor = nil
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers.removeAll()
    }
}
