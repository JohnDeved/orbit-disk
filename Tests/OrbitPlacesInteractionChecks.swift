private struct PlaceFrames: PreferenceKey {
    static var defaultValue: [Int: CGRect] = [:]
    static func reduce(value: inout [Int: CGRect], nextValue: () -> [Int: CGRect]) {
        value.merge(nextValue(), uniquingKeysWith: { _, new in new })
    }
}

@MainActor private final class PlaceFixtureState: ObservableObject {
    @Published var selected = 0
    var hits = [0, 0, 0, 0]
    var frames: [Int: CGRect] = [:]
}

private struct PlaceFixture: View {
    @ObservedObject var state: PlaceFixtureState
    private let titles = ["Home", "Downloads", "A pinned folder with a very long name", "Choose a folder…"]
    private let icons = ["house", "arrow.down.circle", "folder", "folder.badge.plus"]

    var body: some View {
        VStack(spacing: 0) {
            ForEach(0..<titles.count, id: \.self) { index in
                PlaceRowButton(title: titles[index], icon: icons[index], isSelected: index < 3 && state.selected == index,
                               verticalPadding: index == 3 ? 12 : 11) {
                    state.hits[index] += 1
                    if index < 3 { state.selected = index }
                }
                .background(GeometryReader { geometry in
                    Color.clear.preference(key: PlaceFrames.self, value: [index: geometry.frame(in: .named("places"))])
                })
            }
            Button {} label: {
                Label("Home", systemImage: "house").frame(maxWidth: .infinity, alignment: .leading)
                    .font(.system(size: 13)).padding(.vertical, 11).padding(.horizontal, 24)
            }.buttonStyle(.plain)
                .background(GeometryReader { geometry in
                    Color.clear.preference(key: PlaceFrames.self, value: [4: geometry.frame(in: .named("places"))])
                })
            Button {} label: {
                Label("Choose a folder…", systemImage: "folder.badge.plus").frame(maxWidth: .infinity, alignment: .leading)
            }.buttonStyle(.plain).padding(.horizontal, 24).padding(.vertical, 12)
                .background(GeometryReader { geometry in
                    Color.clear.preference(key: PlaceFrames.self, value: [5: geometry.frame(in: .named("places"))])
                })
            Spacer(minLength: 0)
        }
        .frame(width: 210, height: 300, alignment: .topLeading)
        .coordinateSpace(name: "places")
        .background(OrbitStyle.sidebar)
        .controlSize(.large)
        .onPreferenceChange(PlaceFrames.self) { state.frames = $0 }
    }
}

@main private enum PlacesInteractionChecks {
    @MainActor static func main() async throws {
        let app = NSApplication.shared
        app.setActivationPolicy(.prohibited)
        let state = PlaceFixtureState()
        let host = NSHostingView(rootView: PlaceFixture(state: state))
        let window = NSWindow(contentRect: NSRect(x: -20_000, y: -20_000, width: 210, height: 300),
                              styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        window.orderFront(nil)
        defer { window.close() }
        for _ in 0..<100 {
            if state.frames.count == 6 { break }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        precondition(state.frames.count == 6, "Places rows did not lay out")
        let original = state.frames
        precondition(abs(original[0]!.height - original[4]!.height) < 0.5, "Place height must remain unchanged")
        precondition(abs(original[3]!.height - original[5]!.height) < 0.5, "Picker height must remain unchanged")
        for index in 0..<4 {
            let frame = original[index]!
            precondition(abs(frame.width - 210) < 0.5, "Each row must fill the sidebar")
            for point in [CGPoint(x: frame.maxX - 2, y: frame.midY), CGPoint(x: frame.minX + 2, y: frame.minY + 2)] {
                let location = host.convert(point, to: nil)
                let before = state.hits[index]
                let down = NSEvent.mouseEvent(with: .leftMouseDown, location: location, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                                              windowNumber: window.windowNumber, context: nil, eventNumber: 1, clickCount: 1, pressure: 1)!
                let up = NSEvent.mouseEvent(with: .leftMouseUp, location: location, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime + 0.01,
                                            windowNumber: window.windowNumber, context: nil, eventNumber: 2, clickCount: 1, pressure: 0)!
                window.sendEvent(down)
                window.sendEvent(up)
                try await Task.sleep(nanoseconds: 30_000_000)
                precondition(state.hits[index] == before + 1, "Blank row padding must activate row \(index)")
                precondition(state.frames == original, "Selection and button feedback must not change row geometry")
            }
        }
        print("PASS: full-width Places/picker clicks, padded-corner clicks and stable selection geometry")
    }
}
