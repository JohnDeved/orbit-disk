import AppKit
import SwiftUI

extension View {
    func dismissSheetOnOutsideClick(isPresented: Bool, enabled: Bool = true,
                                    dismiss: @escaping () -> Void) -> some View {
        background(OutsideSheetDismissal(active: isPresented && enabled, dismiss: dismiss)
            .accessibilityHidden(true))
    }
}

private struct OutsideSheetDismissal: NSViewRepresentable {
    let active: Bool
    let dismiss: () -> Void

    func makeNSView(context: Context) -> OutsideSheetDismissalView { OutsideSheetDismissalView() }

    func updateNSView(_ view: OutsideSheetDismissalView, context: Context) {
        view.dismiss = dismiss
        view.active = active
        view.updateMonitor()
    }

    static func dismantleNSView(_ view: OutsideSheetDismissalView, coordinator: ()) {
        view.stopMonitoring()
    }
}

private final class OutsideSheetDismissalView: NSView {
    var active = false
    var dismiss: () -> Void = {}
    private var monitor: Any?

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        updateMonitor()
    }

    func updateMonitor() {
        guard active, window != nil else { stopMonitoring(); return }
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
            guard let self, self.active, let presenter = self.window,
                  let sheet = presenter.attachedSheet, sheet.attachedSheet == nil,
                  let eventWindow = event.window else { return event }
            var ancestor: NSWindow? = sheet
            while let candidate = ancestor, candidate !== eventWindow { ancestor = candidate.sheetParent }
            guard ancestor != nil,
                  !sheet.frame.contains(eventWindow.convertPoint(toScreen: event.locationInWindow)) else { return event }
            self.dismiss()
            // Consume the click so it cannot also activate a control behind the sheet.
            return nil
        }
    }

    func stopMonitoring() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
    }

    deinit { stopMonitoring() }
}
