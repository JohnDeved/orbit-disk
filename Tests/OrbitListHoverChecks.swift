import AppKit

@main
enum OrbitListHoverChecks {
    final class HoverWindow: NSWindow {
        var keyForChecks = true
        override var isKeyWindow: Bool { keyForChecks }
    }

    final class Rows: NSObject, NSTableViewDataSource {
        func numberOfRows(in tableView: NSTableView) -> Int { 40 }
        func tableView(_ tableView: NSTableView, objectValueFor tableColumn: NSTableColumn?, row: Int) -> Any? { "Item \(row)" }
    }

    final class WheelEvent: NSEvent {
        var targetWindow: NSWindow?
        var point = NSPoint.zero
        var scrollPhase: NSEvent.Phase = []
        override var type: NSEvent.EventType { .scrollWheel }
        override var window: NSWindow? { targetWindow }
        override var locationInWindow: NSPoint { point }
        override var phase: NSEvent.Phase { scrollPhase }
        override var momentumPhase: NSEvent.Phase { [] }
    }

    static func main() {
        _ = NSApplication.shared
        let window = HoverWindow(contentRect: NSRect(x: 80, y: 80, width: 360, height: 260),
                              styleMask: .borderless, backing: .buffered, defer: false)
        let scroll = NSScrollView(frame: window.contentView!.bounds)
        let table = NSTableView(frame: NSRect(x: 0, y: 0, width: 340, height: 2_080))
        let dataSource = Rows()
        table.dataSource = dataSource
        table.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("files")))
        table.headerView = nil
        table.rowHeight = 52
        table.intercellSpacing = .zero
        scroll.documentView = table
        window.contentView!.addSubview(scroll)
        table.reloadData()
        let first = NSView(frame: table.rect(ofRow: 0))
        let second = NSView(frame: table.rect(ofRow: 1))
        table.addSubview(first)
        table.addSubview(second)
        window.orderBack(nil)
        defer { window.orderOut(nil) }

        func point(in row: NSView) -> NSPoint { row.convert(NSPoint(x: 80, y: 26), to: nil) }
        var pointer = window.convertPoint(toScreen: point(in: first))
        let tracking = ListHoverTracking(pointerLocation: { pointer })
        var changes: [String?] = []
        tracking.onHoverChanged = { changes.append($0) }
        tracking.register(first, id: "first", table: table)
        tracking.register(second, id: "second", table: table)
        precondition(tracking.rowID(at: point(in: first)) == "first")
        precondition(tracking.rowID(at: point(in: second)) == "second")
        tracking.register(second, id: "replacement", table: table)
        precondition(tracking.rowID(at: point(in: second)) == "replacement")
        tracking.unregister(second)
        precondition(tracking.rowID(at: point(in: second)) == nil)
        tracking.register(second, id: "second", table: table)
        tracking.rowHoverChanged(id: "first", inside: true)
        tracking.scrollWheel(phase: [], momentum: [])
        for _ in 0..<100 {
            tracking.rowHoverChanged(id: "first", inside: false)
            tracking.rowHoverChanged(id: "second", inside: true)
            tracking.scrollWheel(phase: [], momentum: [])
        }
        precondition(changes == ["first", nil] && tracking.isSuppressed)
        pointer = window.convertPoint(toScreen: point(in: second))
        tracking.settleIfIdle()
        precondition(changes == ["first", nil, "second"] && !tracking.isSuppressed)
        print("PASS phase-less wheel scrolling clears once, ignores row churn, and restores the current pointer row")

        tracking.scrollWheel(phase: .began, momentum: [])
        tracking.settleIfIdle()
        precondition(tracking.isSuppressed)
        tracking.scrollWheel(phase: .ended, momentum: [])
        tracking.scrollWheel(phase: [], momentum: .began)
        for _ in 0..<20 {
            tracking.scrollWheel(phase: [], momentum: .changed)
            tracking.settleIfIdle()
            precondition(tracking.isSuppressed)
        }
        tracking.liveScrollChanged(active: false)
        tracking.settleIfIdle()
        precondition(tracking.isSuppressed && changes.last! == nil)
        tracking.scrollWheel(phase: [], momentum: .ended)
        tracking.settleIfIdle()
        precondition(!tracking.isSuppressed && changes.last! == "second")
        print("PASS finger-up and live-scroll end cannot restore hover while trackpad momentum remains active")

        tracking.liveScrollChanged(active: true)
        tracking.settleIfIdle()
        precondition(tracking.isSuppressed)
        tracking.liveScrollChanged(active: false)
        tracking.settleIfIdle()
        precondition(!tracking.isSuppressed && changes.last! == "second")
        tracking.scrollWheel(phase: .began, momentum: [])
        tracking.scrollWheel(phase: .cancelled, momentum: .cancelled)
        tracking.settleIfIdle()
        precondition(!tracking.isSuppressed)
        print("PASS scrollbar live scrolling and cancelled gestures complete their suppression lifecycle")

        let beforeUnrelated = changes.count
        let unrelatedClip = NSClipView()
        NotificationCenter.default.post(name: NSView.boundsDidChangeNotification, object: unrelatedClip)
        precondition(changes.count == beforeUnrelated && !tracking.isSuppressed)
        let otherWindow = NSWindow(contentRect: window.frame, styleMask: .borderless, backing: .buffered, defer: false)
        let wheel = WheelEvent()
        wheel.targetWindow = otherWindow
        wheel.point = point(in: first)
        tracking.handle(wheel)
        precondition(changes.count == beforeUnrelated)
        wheel.targetWindow = window
        wheel.point = NSPoint(x: -20, y: -20)
        tracking.handle(wheel)
        precondition(changes.count == beforeUnrelated)
        wheel.point = point(in: first)
        wheel.scrollPhase = .began
        tracking.handle(wheel)
        precondition(tracking.isSuppressed && changes.last! == nil)
        wheel.point = NSPoint(x: -20, y: -20)
        wheel.scrollPhase = .ended
        tracking.handle(wheel)
        tracking.settleIfIdle()
        precondition(!tracking.isSuppressed && changes.last! == "second")
        print("PASS wheel routing is scoped to this list while an already-started gesture can finish outside it")

        pointer = window.convertPoint(toScreen: point(in: first))
        scroll.contentView.setBoundsOrigin(NSPoint(x: 0, y: 52))
        NotificationCenter.default.post(name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        precondition(tracking.isSuppressed && changes.last! == nil)
        tracking.rowHoverChanged(id: "first", inside: true)
        tracking.settleIfIdle()
        precondition(changes.last! == "second")
        print("PASS native clip scrolling resolves the row now under a stationary pointer instead of stale hover")

        window.keyForChecks = false
        NotificationCenter.default.post(name: NSWindow.didResignKeyNotification, object: window)
        scroll.contentView.setBoundsOrigin(.zero)
        NotificationCenter.default.post(name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        tracking.settleIfIdle()
        precondition(!tracking.isSuppressed && changes.last! == nil)
        window.keyForChecks = true
        tracking.scrollWheel(phase: [], momentum: [])
        tracking.settleIfIdle()
        precondition(changes.last! == "first")
        print("PASS later layout changes cannot restore hover in an inactive window")

        tracking.scrollWheel(phase: [], momentum: [])
        pointer = window.convertPoint(toScreen: NSPoint(x: -20, y: -20))
        tracking.settleIfIdle()
        precondition(!tracking.isSuppressed && changes.last! == nil)
        tracking.scrollWheel(phase: .began, momentum: .began)
        NotificationCenter.default.post(name: NSWindow.didResignKeyNotification, object: window)
        precondition(!tracking.isSuppressed && changes.last! == nil)
        tracking.detach()
        let afterDetach = changes.count
        NotificationCenter.default.post(name: NSScrollView.willStartLiveScrollNotification, object: scroll)
        scroll.contentView.setBoundsOrigin(NSPoint(x: 0, y: 104))
        tracking.rowHoverChanged(id: "first", inside: true)
        precondition(tracking.scrollView == nil && !tracking.isSuppressed && changes.count == afterDetach)
        print("PASS pointer exit, window deactivation, and detach clear hover without leaving active observers")

        weak var released: ListHoverTracking?
        autoreleasepool {
            let temporary = ListHoverTracking()
            released = temporary
            temporary.register(first, id: "first", table: table)
            temporary.scrollWheel(phase: [], momentum: [])
        }
        precondition(released == nil)
        print("PASS pending timers and event monitors do not retain a discarded list hover tracker")

        func runLoop(for duration: TimeInterval) {
            let end = Date(timeIntervalSinceNow: duration)
            while Date() < end { RunLoop.main.run(until: min(end, Date(timeIntervalSinceNow: 0.005))) }
        }
        scroll.contentView.setBoundsOrigin(.zero)
        pointer = window.convertPoint(toScreen: point(in: first))
        let timed = ListHoverTracking(settleDelay: 0.12, pointerLocation: { pointer })
        timed.register(first, id: "first", table: table)
        var timedChanges: [String?] = []
        timed.onHoverChanged = { timedChanges.append($0) }
        timed.scrollWheel(phase: .began, momentum: [])
        runLoop(for: 0.18)
        precondition(timed.isSuppressed && timedChanges == [nil])
        timed.scrollWheel(phase: .ended, momentum: [])
        runLoop(for: 0.02)
        timed.scrollWheel(phase: [], momentum: .began)
        runLoop(for: 0.18)
        precondition(timed.isSuppressed && timedChanges == [nil])
        timed.scrollWheel(phase: [], momentum: .ended)
        runLoop(for: 0.03)
        timed.scrollWheel(phase: [], momentum: [])
        runLoop(for: 0.04)
        precondition(timed.isSuppressed && timedChanges == [nil])
        runLoop(for: 0.12)
        precondition(!timed.isSuppressed && timedChanges == [nil, "first"])
        timed.detach()
        print("PASS the real quiet timer stays blocked through gesture and momentum, and restarts after each wheel event")
    }
}
