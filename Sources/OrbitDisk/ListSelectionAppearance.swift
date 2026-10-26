import AppKit
import SwiftUI

struct ListSelectionAppearance: NSViewRepresentable {
    let hoverTracking: ListHoverTracking
    let itemID: String
    let onHoverChanged: (String?) -> Void

    func makeNSView(context: Context) -> SelectionAppearanceView {
        SelectionAppearanceView()
    }

    func updateNSView(_ nsView: SelectionAppearanceView, context: Context) {
        nsView.hoverTracking = hoverTracking
        nsView.itemID = itemID
        hoverTracking.onHoverChanged = onHoverChanged
        nsView.configureWhenAttached()
    }

    static func dismantleNSView(_ nsView: SelectionAppearanceView, coordinator: ()) {
        nsView.hoverTracking?.unregister(nsView)
    }
}

final class SelectionAppearanceView: NSView {
    weak var hoverTracking: ListHoverTracking?
    var itemID = ""
    private var configurationScheduled = false

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func viewDidMoveToSuperview() {
        super.viewDidMoveToSuperview()
        configureWhenAttached()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil { hoverTracking?.unregister(self) }
        configureWhenAttached()
    }

    func configureWhenAttached() {
        guard !configure(), !configurationScheduled else { return }
        configurationScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.configurationScheduled = false
            _ = self.configure()
        }
    }

    private func configure() -> Bool {
        var ancestor = superview
        while let view = ancestor {
            if let table = view as? NSTableView {
                // Hide native paint while SwiftUI keeps selection, focus, and accessibility ownership.
                if table.selectionHighlightStyle != .none { table.selectionHighlightStyle = .none }
                if let scrollView = table.enclosingScrollView {
                    // Keep the column positions unchanged when a folder fits without scrolling.
                    if scrollView.autohidesScrollers { scrollView.autohidesScrollers = false }
                    if !scrollView.hasVerticalScroller { scrollView.hasVerticalScroller = true }
                }
                if window != nil { hoverTracking?.register(self, id: itemID, table: table) }
                return true
            }
            ancestor = view.superview
        }
        return false
    }
}
