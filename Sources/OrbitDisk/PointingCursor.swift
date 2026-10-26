import AppKit
import SwiftUI

extension View {
    func pointingCursor(_ active: Bool = true) -> some View {
        modifier(PointingCursor(active: active))
    }
}

private struct PointingCursor: ViewModifier {
    @Environment(\.isEnabled) private var isEnabled
    let active: Bool

    @ViewBuilder
    func body(content: Content) -> some View {
        if #available(macOS 15, *) {
            content.pointerStyle(isEnabled && active ? .link : .default)
        } else {
            content.background(CursorRegion(active: isEnabled && active).accessibilityHidden(true))
        }
    }
}

private struct CursorRegion: NSViewRepresentable {
    let active: Bool

    func makeNSView(context: Context) -> CursorRegionView { CursorRegionView() }

    func updateNSView(_ view: CursorRegionView, context: Context) {
        guard view.active != active else { return }
        view.active = active
        view.window?.invalidateCursorRects(for: view)
    }
}

private final class CursorRegionView: NSView {
    var active = false

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func resetCursorRects() {
        super.resetCursorRects()
        // AppKit owns restoration when leaving, scrolling, or changing windows.
        let region = bounds.intersection(visibleRect)
        if active && !region.isEmpty { addCursorRect(region, cursor: .pointingHand) }
    }
}
