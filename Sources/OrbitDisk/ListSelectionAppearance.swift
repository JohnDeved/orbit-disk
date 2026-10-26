import AppKit
import SwiftUI

struct ListSelectionAppearance: NSViewRepresentable {
    func makeNSView(context: Context) -> SelectionAppearanceView {
        SelectionAppearanceView()
    }

    func updateNSView(_ nsView: SelectionAppearanceView, context: Context) {
        nsView.configureWhenAttached()
    }
}

final class SelectionAppearanceView: NSView {
    private var configurationScheduled = false

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func viewDidMoveToSuperview() {
        super.viewDidMoveToSuperview()
        configureWhenAttached()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
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
                return true
            }
            ancestor = view.superview
        }
        return false
    }
}
