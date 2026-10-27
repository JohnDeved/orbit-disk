import AppKit

@main
enum OrbitListHoverChecks {
    private static func precondition(_ condition: @autoclosure () -> Bool,
                                     _ message: @autoclosure () -> String = "",
                                     file: StaticString = #filePath, line: UInt = #line) {
        guard condition() else {
            fputs("FAIL \(file):\(line): \(message())\n", stderr)
            exit(1)
        }
    }

    final class HoverWindow: NSWindow {
        var keyForChecks = true
        override var isKeyWindow: Bool { keyForChecks }
    }

    final class Rows: NSObject, NSTableViewDataSource {
        var count = 40
        func numberOfRows(in tableView: NSTableView) -> Int { count }
        func tableView(_ tableView: NSTableView, objectValueFor tableColumn: NSTableColumn?, row: Int) -> Any? { "Item \(row)" }
    }

    final class RevealTable: NSTableView {
        var revealCalls = 0
        override func scrollRowToVisible(_ row: Int) {
            revealCalls += 1
            super.scrollRowToVisible(row)
        }
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
        setbuf(stdout, nil)
        _ = NSApplication.shared
        let window = HoverWindow(contentRect: NSRect(x: 80, y: 80, width: 360, height: 260),
                              styleMask: .borderless, backing: .buffered, defer: false)
        let scroll = NSScrollView(frame: window.contentView!.bounds)
        let table = RevealTable(frame: NSRect(x: 0, y: 0, width: 340, height: 2_080))
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

        tracking.register(first, id: "first", table: table)
        var indexLookups = 0
        precondition(!tracking.revealRow(expectedCount: 40) { indexLookups += 1; return 0 })
        precondition(!tracking.revealRow(expectedCount: 1_000) { indexLookups += 1; return 0 })
        precondition(indexLookups == 0 && table.revealCalls == 0)
        dataSource.count = 999
        table.reloadData()
        precondition(!tracking.revealRow(expectedCount: 999) { indexLookups += 1; return 0 })
        precondition(indexLookups == 0)
        dataSource.count = 1_000
        table.reloadData()
        scroll.contentView.setBoundsOrigin(.zero)
        precondition(!tracking.revealRow(expectedCount: 1_000) { nil })
        precondition(!tracking.revealRow(expectedCount: 1_000) { -1 })
        precondition(!tracking.revealRow(expectedCount: 1_000) { 1_000 })
        precondition(tracking.revealRow(expectedCount: 1_000) { 0 })
        precondition(table.revealCalls == 0)
        precondition(tracking.revealRow(expectedCount: 1_000) { 999 })
        precondition(table.revealCalls == 1 && NSLocationInRange(999, table.rows(in: table.visibleRect)))
        precondition(tracking.revealRow(expectedCount: 1_000) { 999 })
        precondition(table.revealCalls == 1)
        tracking.detach()
        precondition(!tracking.revealRow(expectedCount: 1_000) { indexLookups += 1; return 0 })
        precondition(indexLookups == 0)
        dataSource.count = 40
        table.reloadData()
        print("PASS large-list reveal skips visible rows and falls back for short, stale, missing, or detached rows")

        func runLoop(for duration: TimeInterval) {
            let end = Date(timeIntervalSinceNow: duration)
            while Date() < end { RunLoop.main.run(until: min(end, Date(timeIntervalSinceNow: 0.005))) }
        }
        func waitForRestore(_ tracking: ListHoverTracking, table: NSTableView) {
            let deadline = Date(timeIntervalSinceNow: 2)
            while tracking.hasPendingRestore && Date() < deadline { runLoop(for: 0.005) }
            precondition(!tracking.hasPendingRestore,
                         "Restore timed out: rows=\(table.numberOfRows), visible=\(table.rows(in: table.visibleRect))")
        }
        scroll.contentView.setBoundsOrigin(.zero)
        pointer = window.convertPoint(toScreen: point(in: first))
        let quietDelay: TimeInterval = 0.12
        let timed = ListHoverTracking(settleDelay: quietDelay, pointerLocation: { pointer })
        timed.register(first, id: "first", table: table)
        var timedChanges: [String?] = []
        var mayRestoreHover = false
        var quietDeadline = TimeInterval.infinity
        timed.onHoverChanged = { id in
            if id != nil {
                precondition(mayRestoreHover, "The timer restored hover during a gesture or momentum")
                precondition(ProcessInfo.processInfo.systemUptime >= quietDeadline,
                             "The timer restored hover before the latest wheel event became quiet")
            }
            timedChanges.append(id)
        }
        timed.scrollWheel(phase: .began, momentum: [])
        runLoop(for: 0.18)
        precondition(timed.isSuppressed && timedChanges == [nil])
        timed.scrollWheel(phase: .ended, momentum: [])
        timed.scrollWheel(phase: [], momentum: .began)
        runLoop(for: 0.18)
        precondition(timed.isSuppressed && timedChanges == [nil])
        mayRestoreHover = true
        quietDeadline = ProcessInfo.processInfo.systemUptime + quietDelay
        timed.scrollWheel(phase: [], momentum: .ended)
        // Leave the old timer due without delivering it, then reset it with a new wheel event.
        while ProcessInfo.processInfo.systemUptime < quietDeadline {
            Thread.sleep(forTimeInterval: max(0, quietDeadline - ProcessInfo.processInfo.systemUptime))
        }
        precondition(timed.isSuppressed && timedChanges == [nil])
        quietDeadline = ProcessInfo.processInfo.systemUptime + quietDelay
        timed.scrollWheel(phase: [], momentum: [])
        let completionDeadline = ProcessInfo.processInfo.systemUptime + 2
        while timed.isSuppressed && ProcessInfo.processInfo.systemUptime < completionDeadline { runLoop(for: 0.005) }
        precondition(!timed.isSuppressed && timedChanges == [nil, "first"],
                     "Quiet timer did not restore the pointer row: suppressed=\(timed.isSuppressed), changes=\(timedChanges), row=\(String(describing: timed.rowID(at: window.convertPoint(fromScreen: pointer))))")
        timed.detach()
        print("PASS the real quiet timer stays blocked through gesture and momentum, and restarts after each wheel event")

        dataSource.count = 100
        func resetList() -> (NSScrollView, RevealTable, NSView) {
            let scroller = NSScrollView(frame: window.contentView!.bounds)
            let list = RevealTable(frame: NSRect(x: 0, y: 0, width: 340, height: 5_200))
            list.dataSource = dataSource
            list.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("reset-files")))
            list.headerView = nil
            list.rowHeight = 52
            list.intercellSpacing = .zero
            scroller.documentView = list
            window.contentView!.addSubview(scroller)
            list.reloadData()
            let view = NSView(frame: list.rect(ofRow: 0))
            list.addSubview(view)
            return (scroller, list, view)
        }
        func offset(_ row: Int, in table: NSTableView, scroll: NSScrollView) -> CGFloat {
            table.rect(ofRow: row).minY - table.convert(scroll.contentView.bounds, from: scroll.contentView).minY
        }
        let resetTracking = ListHoverTracking()
        var restorationCount = 0
        resetTracking.onResetRestored = { restorationCount += 1 }
        let (oldScroll, oldTable, oldRow) = resetList()
        resetTracking.register(oldRow, id: "item-0", table: oldTable)
        oldScroll.contentView.scroll(to: NSPoint(x: 0, y: 16 * 52 + 13))
        oldScroll.reflectScrolledClipView(oldScroll.contentView)
        precondition(window.makeFirstResponder(oldTable))
        let before = offset(17, in: oldTable, scroll: oldScroll)
        precondition(resetTracking.prepareForReset(preferredRow: 17, itemIDAt: { "item-\($0)" },
                                                  rowIndexForID: { Int($0.dropFirst(5)).map { 99 - $0 } }))
        precondition(resetTracking.hasPendingRestore)
        oldScroll.removeFromSuperview()
        resetTracking.detach()
        precondition(resetTracking.hasPendingRestore, "List teardown must retain a pending reset")
        let (replacementScroll, replacementTable, replacementRow) = resetList()
        resetTracking.register(replacementRow, id: "item-99", table: replacementTable)
        waitForRestore(resetTracking, table: replacementTable)
        precondition(restorationCount == 1)
        precondition(abs(offset(82, in: replacementTable, scroll: replacementScroll) - before) < 0.5,
                     "The visible hovered row must keep its pixel offset after reordering")
        precondition(window.firstResponder === replacementTable, "A focused list must regain keyboard focus")

        replacementScroll.contentView.scroll(to: NSPoint(x: 0, y: 20 * 52 + 11))
        replacementScroll.reflectScrolledClipView(replacementScroll.contentView)
        let beforeTop = offset(20, in: replacementTable, scroll: replacementScroll)
        precondition(resetTracking.prepareForReset(preferredRow: 99, itemIDAt: { "item-\($0)" },
                                                  rowIndexForID: { Int($0.dropFirst(5)).map { $0 + 10 } }))
        let search = NSTextField(frame: NSRect(x: 0, y: 0, width: 100, height: 20))
        window.contentView!.addSubview(search)
        precondition(window.makeFirstResponder(search))
        replacementScroll.removeFromSuperview()
        resetTracking.detach()
        let (nextScroll, nextTable, nextRow) = resetList()
        resetTracking.register(nextRow, id: "item-10", table: nextTable)
        waitForRestore(resetTracking, table: nextTable)
        precondition(restorationCount == 2)
        precondition(abs(offset(30, in: nextTable, scroll: nextScroll) - beforeTop) < 0.5,
                     "An offscreen preferred row must fall back to the first visible row")
        precondition(window.firstResponder !== nextTable, "A newer focus choice must not be overwritten")

        let visibleRow = nextTable.rows(in: nextTable.convert(nextScroll.contentView.bounds, from: nextScroll.contentView)).location
        let beforeMissing = offset(visibleRow, in: nextTable, scroll: nextScroll)
        precondition(resetTracking.prepareForReset(preferredRow: nil, itemIDAt: { "item-\($0)" },
                                                  rowIndexForID: { _ in nil }))
        let sameScrollTable = RevealTable(frame: NSRect(x: 0, y: 0, width: 340, height: 5_200))
        sameScrollTable.dataSource = dataSource
        sameScrollTable.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("reused-scroll")))
        sameScrollTable.headerView = nil
        sameScrollTable.rowHeight = 52
        sameScrollTable.intercellSpacing = .zero
        nextScroll.documentView = sameScrollTable
        sameScrollTable.reloadData()
        let sameScrollRow = NSView(frame: sameScrollTable.rect(ofRow: 0))
        sameScrollTable.addSubview(sameScrollRow)
        resetTracking.register(sameScrollRow, id: "new-item", table: sameScrollTable)
        waitForRestore(resetTracking, table: sameScrollTable)
        precondition(restorationCount == 3)
        precondition(abs(offset(visibleRow, in: sameScrollTable, scroll: nextScroll) - beforeMissing) < 0.5,
                     "A missing item must fall back to the previous row index on a reused scroll view")
        let beforeDelayed = offset(visibleRow, in: sameScrollTable, scroll: nextScroll)
        precondition(resetTracking.prepareForReset(preferredRow: nil, itemIDAt: { "item-\($0)" },
                                                  rowIndexForID: { _ in 70 }))
        dataSource.count = 10
        let loadingTable = RevealTable(frame: NSRect(x: 0, y: 0, width: 340, height: 5_200))
        loadingTable.dataSource = dataSource
        loadingTable.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier("loading")))
        loadingTable.headerView = nil
        loadingTable.rowHeight = 52
        loadingTable.intercellSpacing = .zero
        nextScroll.documentView = loadingTable
        loadingTable.reloadData()
        let loadingRow = NSView(frame: loadingTable.rect(ofRow: 0))
        loadingTable.addSubview(loadingRow)
        resetTracking.register(loadingRow, id: "loading", table: loadingTable)
        runLoop(for: 0.02)
        precondition(resetTracking.hasPendingRestore, "An incomplete table must wait for its anchor row")
        dataSource.count = 100
        loadingTable.reloadData()
        waitForRestore(resetTracking, table: loadingTable)
        precondition(restorationCount == 4)
        precondition(abs(offset(70, in: loadingTable, scroll: nextScroll) - beforeDelayed) < 0.5,
                     "A reset must retry once its anchor row becomes available")
        let beforeRace = offset(70, in: loadingTable, scroll: nextScroll)
        precondition(resetTracking.prepareForReset(preferredRow: 70, itemIDAt: { "item-\($0)" },
                                                  rowIndexForID: { _ in 40 }))
        func replacementInSameScroll(_ identifier: String) -> (RevealTable, NSView) {
            let list = RevealTable(frame: NSRect(x: 0, y: 0, width: 340, height: 5_200))
            list.dataSource = dataSource
            list.addTableColumn(NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier)))
            list.headerView = nil
            list.rowHeight = 52
            list.intercellSpacing = .zero
            nextScroll.documentView = list
            list.reloadData()
            let view = NSView(frame: list.rect(ofRow: 0))
            list.addSubview(view)
            resetTracking.register(view, id: identifier, table: list)
            return (list, view)
        }
        let (intermediateTable, intermediateRow) = replacementInSameScroll("intermediate")
        let (finalTable, finalRow) = replacementInSameScroll("final")
        withExtendedLifetime((intermediateTable, intermediateRow, finalRow)) { waitForRestore(resetTracking, table: finalTable) }
        precondition(!resetTracking.hasPendingRestore && restorationCount == 5)
        precondition(abs(offset(40, in: finalTable, scroll: nextScroll) - beforeRace) < 0.5,
                     "A newer replacement table must inherit the pending restore")
        precondition(resetTracking.prepareForReset(preferredRow: nil, itemIDAt: { "item-\($0)" },
                                                  rowIndexForID: { _ in nil }))
        resetTracking.cancelPendingRestore()
        precondition(!resetTracking.hasPendingRestore && restorationCount == 5)
        nextScroll.removeFromSuperview()
        resetTracking.detach()
        print("PASS list reset preserves visible row offset and focus, honors later focus, and can be cancelled")
    }
}
