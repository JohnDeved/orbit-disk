import AppKit
import SwiftUI
import OrbitCore

enum DiskPalette {
    private static let hues: [Double] = [0.045, 0.115, 0.46, 0.58, 0.69, 0.94, 0.32, 0.51]

    static func color(for index: Int, depth: Int = 0) -> Color {
        let hue = hues[abs(index % hues.count)]
        return Color(hue: hue, saturation: max(0.42, 0.66 - Double(depth) * 0.08), brightness: 0.96)
    }
}

final class SunburstPresentationCache {
    private var rootID: String?
    private var mapRevision: UUID?
    private var coverage: CollectionCoverage?
    private var prepared: SunburstPresentation?

    func presentation(root: DiskNode, mapRevision: UUID, coverage: CollectionCoverage,
                      colorIndices: [String: Int] = [:], rootChildIDs: [String]? = nil) -> SunburstPresentation {
        let rootID = root.id
        let layout: SunburstPreparedLayout
        if self.rootID == rootID, self.mapRevision == mapRevision, let prepared {
            if self.coverage == coverage { return prepared }
            layout = prepared.layout
        } else {
            layout = SunburstPreparedLayout(root: root, mapRevision: mapRevision, colorIndices: colorIndices, rootChildIDs: rootChildIDs)
        }
        let states = coverage.isEmpty
            ? Array(repeating: CollectionState.none, count: layout.segments.count)
            : layout.segments.indices.map { index in
                let members = layout.memberIDs[index]
                guard let parent = layout.segments[index].groupParentID else { return coverage.state(for: members) }
                return coverage.state(forGroup: members, distinct: layout.groupMembersAreDistinct(at: index), parent: parent) {
                    layout.contains(groupMember: $0, at: index)
                }
            }
        let prepared = SunburstPresentation(layout: layout, collectionStates: states,
                                            rootCollectionState: coverage.state(for: rootID))
        self.rootID = rootID
        self.mapRevision = mapRevision
        self.coverage = coverage
        self.prepared = prepared
        return prepared
    }
}

final class SunburstPreparedLayout {
    let revision = UUID()
    let segments: [SunburstSegment]
    let memberIDs: [[String]]
    private let segmentIndices: [String: Int]
    private let hoverGroups: [String: PreparedMapHoverGroup]
    private let groupMemberSets: [Set<String>?]

    init(root: DiskNode, mapRevision: UUID, colorIndices: [String: Int] = [:], rootChildIDs: [String]? = nil) {
        let segments = SunburstLayout.make(root: root, colorIndices: colorIndices, rootChildIDs: rootChildIDs)
        let rootID = root.id
        let memberIDs = segments.map { $0.preparedMemberIDs ?? $0.members.map(\.id) }
        var indices: [String: Int] = [:]
        var hoverGroups: [String: PreparedMapHoverGroup] = [:]
        var groupMemberSets: [Set<String>?] = Array(repeating: nil, count: segments.count)
        indices.reserveCapacity(memberIDs.reduce(segments.count) { $0 + $1.count })
        for (index, segment) in segments.enumerated() {
            indices[segment.id] = index
            for memberID in memberIDs[index] { indices[memberID] = index }
            if let parentID = segment.groupParentID {
                let members = Set(memberIDs[index])
                hoverGroups[segment.id] = PreparedMapHoverGroup(id: segment.id, parentID: parentID,
                                                               rootID: rootID, mapRevision: mapRevision,
                                                               memberIDs: members)
                groupMemberSets[index] = members
            }
        }
        self.segments = segments
        self.memberIDs = memberIDs
        self.segmentIndices = indices
        self.hoverGroups = hoverGroups
        self.groupMemberSets = groupMemberSets
    }

    func index(for id: String?) -> Int? {
        guard let id else { return nil }
        return segmentIndices[id]
    }

    func contains(groupMember id: Data, at index: Int) -> Bool {
        guard let members = groupMemberSets[index],
              let memberIndex = members.firstIndex(of: String(decoding: id, as: UTF8.self)) else { return false }
        if members[memberIndex].utf8.elementsEqual(id) { return true }
        // Swift sets merge canonically equivalent strings, but filesystem paths can differ in bytes.
        return memberIDs[index].contains { $0.utf8.elementsEqual(id) }
    }

    func groupMembersAreDistinct(at index: Int) -> Bool {
        groupMemberSets[index]?.count == memberIDs[index].count
    }

    func segment(for id: String?) -> SunburstSegment? {
        index(for: id).map { segments[$0] }
    }

    func hoverGroup(for id: String?) -> PreparedMapHoverGroup? {
        id.flatMap { hoverGroups[$0] }
    }
}

struct SunburstPresentation {
    let revision = UUID()
    let layout: SunburstPreparedLayout
    let collectionStates: [CollectionState]
    let rootCollectionState: CollectionState

    func collectionState(for id: String) -> CollectionState {
        layout.index(for: id).map { collectionStates[$0] } ?? .none
    }
}

struct SunburstBaseLayer: View, Equatable {
    let presentation: SunburstPresentation
    let metrics: SunburstMetrics

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.presentation.revision == rhs.presentation.revision && lhs.metrics == rhs.metrics
    }

    var body: some View {
        Canvas { context, _ in
            if presentation.layout.segments.isEmpty {
                let radius = metrics.innerRadius + 13
                let ring = Path(ellipseIn: CGRect(x: metrics.center.x - radius, y: metrics.center.y - radius,
                                                  width: radius * 2, height: radius * 2))
                context.stroke(ring, with: .color(.white.opacity(0.12)), lineWidth: 18)
            }

            for (index, segment) in presentation.layout.segments.enumerated() {
                let path = segment.path(metrics: metrics)
                let color = segment.node == nil
                    ? Color(red: 0.52, green: 0.55, blue: 0.61)
                    : DiskPalette.color(for: segment.branchIndex, depth: segment.depth)
                context.fill(path, with: .color(color))
                drawCollectionState(presentation.collectionStates[index], segment: segment,
                                    path: path, context: context)
            }
        }
    }

    private func drawCollectionState(_ state: CollectionState, segment: SunburstSegment,
                                     path: Path, context: GraphicsContext) {
        guard state != .none else { return }
        let ink = Color(red: 5 / 255.0, green: 7 / 255.0, blue: 10 / 255.0)
        if state == .included {
            var clipped = context
            clipped.clip(to: path)
            let bounds = path.boundingRect
            var hatch = Path()
            for x in stride(from: bounds.minX - bounds.height, through: bounds.maxX, by: 8) {
                hatch.move(to: CGPoint(x: x, y: bounds.minY))
                hatch.addLine(to: CGPoint(x: x + bounds.height, y: bounds.maxY))
            }
            clipped.stroke(hatch, with: .color(ink), lineWidth: 1.5)
        }

        let diameter: CGFloat = state == .included ? 16 : 14
        guard let point = segment.collectionMarkerCenter(metrics: metrics, diameter: diameter) else { return }
        let radius = diameter / 2
        let badge = Path(ellipseIn: CGRect(x: point.x - radius, y: point.y - radius,
                                          width: diameter, height: diameter))
        context.fill(badge, with: .color(ink))
        var glyph = Path()
        if state == .included {
            glyph.move(to: CGPoint(x: point.x - radius * 0.45, y: point.y))
            glyph.addLine(to: CGPoint(x: point.x - radius * 0.1, y: point.y + radius * 0.35))
            glyph.addLine(to: CGPoint(x: point.x + radius * 0.48, y: point.y - radius * 0.35))
        } else {
            glyph.move(to: CGPoint(x: point.x - radius * 0.42, y: point.y))
            glyph.addLine(to: CGPoint(x: point.x + radius * 0.42, y: point.y))
        }
        context.stroke(glyph, with: .color(.white), style: StrokeStyle(lineWidth: 1.8, lineCap: .round, lineJoin: .round))
    }
}

struct SunburstView: View {
    let root: DiskNode
    let selectedID: String?
    let hoveredID: String?
    let updatingSizes: Bool
    let onSelect: (DiskNode) -> Void
    let onOpen: (DiskNode) -> Void
    let onBack: () -> Void
    let canGoBack: Bool
    let onHover: ([DiskNode]?, PreparedMapHoverGroup?) -> Void
    let makeDrag: ([DiskNode], String) -> NSPasteboardItem?
    let onDragEnded: (NSPasteboardItem) -> Void
    let beginInteraction: () -> (() -> Void)

    private let presentation: SunburstPresentation
    private var segments: [SunburstSegment] { presentation.layout.segments }
    @State private var chartHoveredID: String?
    @State private var isCenterHovered = false
    @State private var pinnedGroupID: String?
    @FocusState private var isCenterFocused: Bool

    init(root: DiskNode, mapRevision: UUID, cache: SunburstPresentationCache,
         selectedID: String?, hoveredID: String?, collectionCoverage: CollectionCoverage,
         updatingSizes: Bool = false,
         colorIndices: [String: Int] = [:], rootChildIDs: [String]? = nil, onSelect: @escaping (DiskNode) -> Void,
         onOpen: @escaping (DiskNode) -> Void, onBack: @escaping () -> Void, canGoBack: Bool,
         onHover: @escaping ([DiskNode]?, PreparedMapHoverGroup?) -> Void,
         makeDrag: @escaping ([DiskNode], String) -> NSPasteboardItem?, onDragEnded: @escaping (NSPasteboardItem) -> Void,
         beginInteraction: @escaping () -> (() -> Void) = { {} }) {
        self.root = root
        self.selectedID = selectedID
        self.hoveredID = hoveredID
        self.updatingSizes = updatingSizes
        self.onSelect = onSelect
        self.onOpen = onOpen
        self.onBack = onBack
        self.canGoBack = canGoBack
        self.onHover = onHover
        self.makeDrag = makeDrag
        self.onDragEnded = onDragEnded
        self.beginInteraction = beginInteraction
        self.presentation = cache.presentation(root: root, mapRevision: mapRevision, coverage: collectionCoverage,
                                               colorIndices: colorIndices, rootChildIDs: rootChildIDs)
    }

    private var activeSegment: SunburstSegment? {
        let id = chartHoveredID ?? hoveredID ?? pinnedGroupID ?? selectedID
        return presentation.layout.segment(for: id)
    }

    var body: some View {
        GeometryReader { geometry in
            let metrics = SunburstMetrics(size: geometry.size)
            let active = activeSegment
            let selected = presentation.layout.segment(for: selectedID)

            ZStack {
                SunburstBaseLayer(presentation: presentation, metrics: metrics).equatable()
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("Storage map for \(root.name)")
                .accessibilityValue(updatingSizes ? "Updating sizes. Folder contents are available in the file list." : "\(SunburstText.size(root.size, state: root.scanState)), \(root.fileCount) \(root.fileCount == 1 ? "file" : "files")\(root.isScanComplete ? "" : " mapped so far"). Folder contents are available in the file list.")

                Canvas { context, _ in
                    if let active {
                        context.stroke(active.path(metrics: metrics), with: .color(.white.opacity(0.95)), lineWidth: 2)
                    }
                    if let selected, selected.id != active?.id {
                        context.stroke(selected.path(metrics: metrics), with: .color(.white.opacity(0.95)), lineWidth: 2)
                    }
                }
                .allowsHitTesting(false)
                .accessibilityHidden(true)

                MapInteractionView(rootID: root.id, layoutRevision: presentation.layout.revision,
                                   segments: segments, metrics: metrics, canGoBack: canGoBack,
                                   onHover: { segment in
                                       chartHoveredID = segment?.id
                                       onHover(segment?.members, presentation.layout.hoverGroup(for: segment?.id))
                                   }, onActivate: activate,
                                   makeDrag: { makeDrag($0.members, $0.name) }, onDragEnded: onDragEnded,
                                   beginInteraction: beginInteraction)
                    .accessibilityHidden(true)

                center(active, radius: metrics.innerRadius, hovered: isCenterHovered)
                    .position(metrics.center)
            }
            .contentShape(Rectangle())
        }
        .onChange(of: root.id) { _, _ in
            pinnedGroupID = nil
            chartHoveredID = nil
            onHover(nil, nil)
        }
        .onChange(of: selectedID) { _, _ in pinnedGroupID = nil }
    }

    @ViewBuilder
    private func center(_ segment: SunburstSegment?, radius: CGFloat, hovered: Bool) -> some View {
        if canGoBack {
            Button(action: onBack) {
                centerLabel(segment, radius: radius)
                    .frame(width: radius * 2, height: radius * 2)
                    .background(Circle().fill(.white.opacity(hovered ? 0.055 : 0)))
                    .overlay {
                        Circle().stroke(.white.opacity(isCenterFocused ? 0.95 : 0), lineWidth: 2)
                    }
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .pointingCursor()
            .onHover { isCenterHovered = $0 }
            .focused($isCenterFocused)
            .accessibilityLabel("Go up to the containing folder")
            .accessibilityValue(updatingSizes ? "\(root.name), updating sizes" : "\(root.name), \(SunburstText.size(root.size, state: root.scanState))\(root.isScanComplete ? "" : ", mapped so far")")
            .help("Go up to the containing folder")
        } else {
            centerLabel(segment, radius: radius)
                .allowsHitTesting(false)
        }
    }

    private func centerLabel(_ segment: SunburstSegment?, radius: CGFloat) -> some View {
        let compact = radius < 60
        let titleLines = radius < 80 ? 1 : 2
        let detailLines = compact ? 1 : 2
        let valueFontSize = min(38, radius * 0.37)
        let spacing: CGFloat = compact ? 3 : radius < 80 ? 4 : 7
        return VStack(spacing: spacing) {
            Text(segment?.name ?? root.name)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Color(red: 0.76, green: 0.78, blue: 0.83))
                .lineLimit(titleLines, reservesSpace: true)
                .truncationMode(.middle)
                .multilineTextAlignment(.center)
                .frame(width: max(1, radius * 1.2), height: CGFloat(titleLines) * 16)

            Text(updatingSizes ? "Updating…" : SunburstText.size(segment?.size ?? root.size, state: segment?.scanState ?? root.scanState))
                .font(.system(size: valueFontSize, weight: .light, design: .rounded))
                .foregroundStyle(.white)
                .monospacedDigit()
                .lineLimit(1)
                .minimumScaleFactor(0.6)
                .frame(height: ceil(valueFontSize * 1.25))

            Text(centerDetail(segment))
                .font(.system(size: 11))
                .foregroundStyle(Color(red: 0.655, green: 0.67, blue: 0.72))
                .multilineTextAlignment(.center)
                .lineLimit(detailLines, reservesSpace: true)
                .frame(height: CGFloat(detailLines) * 14)

            Group {
                if compact { Image(systemName: "chevron.up") }
                else { Label("Up one folder", systemImage: "chevron.up") }
            }
            .font(.system(size: 11, weight: .medium))
            .foregroundStyle(.white)
            .lineLimit(1)
            .frame(height: 14)
            .padding(.top, radius >= 80 ? 7 : 0)
            .opacity(canGoBack ? 1 : 0)
            .accessibilityHidden(!canGoBack)
        }
        .frame(width: max(1, radius * 1.65))
    }

    private func centerDetail(_ segment: SunburstSegment?) -> String {
        if updatingSizes { return "Updating sizes" }
        let state = segment.map { presentation.collectionState(for: $0.id) } ?? presentation.rootCollectionState
        return SunburstText.detail(root: root, segment: segment, collection: state)
    }

    private func activate(_ segment: SunburstSegment) {
        guard let node = segment.node else {
            pinnedGroupID = segment.id
            return
        }
        pinnedGroupID = nil
        if node.isDirectory { onOpen(node) } else { onSelect(node) }
    }

}

private func chartBytes(_ bytes: Int64) -> String {
    ByteCountFormatter.string(fromByteCount: max(0, bytes), countStyle: .file)
}

enum SunburstText {
    static func size(_ bytes: Int64, state: NodeScanState) -> String {
        state == .pending ? "Waiting…" : chartBytes(bytes)
    }

    static func detail(root: DiskNode, segment: SunburstSegment?, collection: CollectionState) -> String {
        let scanState = segment?.scanState ?? root.scanState
        if scanState == .pending { return "Waiting to map" }
        if scanState == .scanning { return "Mapped so far" }
        if collection == .included { return "In Collection" }
        if collection == .containsItems { return "Partly collected" }
        guard let segment else {
            return root.size == 0 ? "No measurable file data" : "\(root.fileCount.formatted()) \(root.fileCount == 1 ? "file" : "files")"
        }
        if segment.node == nil { return "Drag these items to Collection" }
        let percent = Double(segment.size) / Double(max(1, root.size))
        let total = root.isScanComplete ? "of this folder" : "of mapped bytes"
        return "\(percent.formatted(.percent.precision(.fractionLength(1)))) \(total)"
    }
}

struct SunburstMetrics: Equatable {
    let center: CGPoint
    let innerRadius: CGFloat
    let ringWidth: CGFloat

    init(size: CGSize) {
        center = CGPoint(x: size.width / 2, y: size.height / 2)
        let radius = max(1, min(size.width, size.height) / 2 - 14)
        innerRadius = radius * 0.43
        ringWidth = (radius - innerRadius) / 4
    }
}

struct SunburstSegment {
    let id: String
    let node: DiskNode?
    let members: [DiskNode]
    let name: String
    let size: Int64
    let start: Double
    let end: Double
    let depth: Int
    let branchIndex: Int
    var scanState: NodeScanState = .complete
    var groupParentID: String? = nil
    var preparedMemberIDs: [String]? = nil

    func matches(_ target: String?) -> Bool {
        guard let target else { return false }
        return id == target || members.contains { $0.id == target }
    }

    func collectionState(using coverage: CollectionCoverage) -> CollectionState {
        guard !coverage.isEmpty else { return .none }
        return coverage.state(for: members.map(\.id))
    }

    func collectionMarkerCenter(metrics: SunburstMetrics, diameter: CGFloat) -> CGPoint? {
        let width = max(1, metrics.ringWidth - 2.8)
        let radius = metrics.innerRadius + CGFloat(depth) * metrics.ringWidth + 1.4 + width / 2
        let inset = min((end - start) * 0.17, 1.2 / Double(radius))
        let halfSweep = max(0, (end - start) / 2 - inset)
        let availableWidth = 2 * radius * sin(min(.pi / 2, halfSweep))
        guard width >= diameter + 4, availableWidth >= diameter + 4 else { return nil }
        let angle = (start + end) / 2 - .pi / 2
        return CGPoint(x: metrics.center.x + radius * cos(angle), y: metrics.center.y + radius * sin(angle))
    }

    func path(metrics: SunburstMetrics) -> Path {
        let inner = metrics.innerRadius + CGFloat(depth) * metrics.ringWidth + 1.4
        let outer = inner + max(1, metrics.ringWidth - 2.8)
        let inset = min((end - start) * 0.17, 1.2 / Double((inner + outer) / 2))
        let first = start + inset - .pi / 2
        let last = end - inset - .pi / 2
        var path = Path()
        path.addArc(center: metrics.center, radius: outer,
                    startAngle: .radians(first), endAngle: .radians(last), clockwise: false)
        path.addArc(center: metrics.center, radius: inner,
                    startAngle: .radians(last), endAngle: .radians(first), clockwise: true)
        path.closeSubpath()
        return path
    }
}

enum SunburstLayout {
    private struct Branch {
        let node: DiskNode
        let start: Double
        let end: Double
        let depth: Int
        let color: Int?
    }

    static func make(root: DiskNode, colorIndices: [String: Int] = [:], rootChildIDs: [String]? = nil) -> [SunburstSegment] {
        guard root.size > 0 else { return [] }
        var result: [SunburstSegment] = []
        var queue = [Branch(node: root, start: 0, end: 2 * .pi, depth: 0, color: nil)]
        var next = 0

        while next < queue.count && result.count < 768 {
            let branch = queue[next]
            next += 1
            guard branch.depth < 4, !branch.node.children.isEmpty else { continue }
            let children = branch.node.children
            let branchIDs = branch.depth == 0 && rootChildIDs?.count == children.count ? rootChildIDs : nil
            let total = children.reduce(0.0) { $0 + Double(max(0, $1.size)) }
            let denominator = max(Double(branch.node.size), total)
            guard denominator > 0 else { continue }
            let sweep = branch.end - branch.start
            let remaining = max(0, min(48, 768 - result.count - 1))
            var cursor = branch.start
            var groupedSize: Int64 = 0
            var groupedMembers: [DiskNode] = []
            var groupedIDs: [String] = []
            var groupedComplete = true
            var visible = 0

            for (index, child) in children.enumerated() where child.size > 0 {
                let childID = branchIDs?[index] ?? child.id
                let angle = sweep * Double(child.size) / denominator
                if angle < 0.014 || visible >= remaining {
                    let sum = groupedSize.addingReportingOverflow(child.size)
                    groupedSize = sum.overflow ? Int64.max : sum.partialValue
                    groupedMembers.append(child)
                    groupedIDs.append(childID)
                    groupedComplete = groupedComplete && child.isScanComplete
                    continue
                }

                let color = branch.color ?? colorIndices[childID] ?? index
                result.append(SunburstSegment(id: childID, node: child, members: [child], name: child.name, size: child.size,
                                              start: cursor, end: cursor + angle, depth: branch.depth,
                                              branchIndex: color, scanState: child.scanState, preparedMemberIDs: [childID]))
                if child.isDirectory && branch.depth < 3 {
                    queue.append(Branch(node: child, start: cursor, end: cursor + angle,
                                        depth: branch.depth + 1, color: color))
                }
                cursor += angle
                visible += 1
            }

            if groupedSize > 0 {
                result.append(SunburstSegment(id: "group:\(branch.node.id)", node: nil, members: groupedMembers,
                                              name: "\(groupedMembers.count.formatted()) grouped \(groupedMembers.count == 1 ? "item" : "items")", size: groupedSize,
                                              start: cursor, end: cursor + sweep * Double(groupedSize) / denominator,
                                              depth: branch.depth, branchIndex: branch.color ?? Int.min,
                                              scanState: groupedComplete ? .complete : .scanning,
                                              groupParentID: branch.node.id, preparedMemberIDs: groupedIDs))
            }
        }
        return result
    }
}
