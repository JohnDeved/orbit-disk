import SwiftUI
import OrbitCore

struct FileRowView: View {
    // The 340-point panel reserves room for the scrollbar before AppKit lays it out.
    private static let contentWidth: CGFloat = 266
    @ObservedObject var model: DiskModel
    let item: DiskNode
    let parentSize: Int64
    let related: Bool
    let listHoverTracking: ListHoverTracking

    var body: some View {
        let colorIndex = model.childColorIndices[item.id] ?? 0
        let percent = parentSize > 0 ? Double(item.size) / Double(parentSize) : 0
        let collectionState = model.collectionCoverage.state(for: item.id)
        let selected = model.selectedID == item.id
        let hovered = related || model.hoveredListID == item.id
        return HStack(spacing: 11) {
            RoundedRectangle(cornerRadius: 3).fill(model.isFiltering ? OrbitStyle.muted : DiskPalette.color(for: colorIndex)).frame(width: 7, height: 30)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(item.name).font(.system(size: 12, weight: .medium)).lineLimit(1).truncationMode(.middle)
                HStack(spacing: 4) {
                    if collectionState != .none {
                        Label(collectionState == .included ? "In Collection" : "Partly collected",
                              systemImage: collectionState == .included ? "checkmark.circle.fill" : "minus.circle.fill")
                            .font(.system(size: 10, weight: .medium)).foregroundStyle(.white).lineLimit(1).fixedSize()
                    }
                    if collectionState == .none || model.isFiltering {
                        Text(model.isFiltering ? item.url.deletingLastPathComponent().path : model.isUpdatingRemovalSizes && item.isDirectory ? "Updating sizes" : !item.isAccessible ? "Not scanned" : item.scanState == .pending ? "Waiting to map" : item.scanState == .scanning ? "\(item.fileCount.formatted()) files found · \(model.isScanning ? "Mapping" : "Incomplete")" : item.isSymbolicLink ? "Symbolic link" : item.isDirectory ? "\(item.fileCount.formatted()) \(item.fileCount == 1 ? "file" : "files")" : item.url.pathExtension.uppercased().isEmpty ? "File" : item.url.pathExtension.uppercased())
                            .font(.system(size: 10)).foregroundStyle(OrbitStyle.muted).lineLimit(1).truncationMode(.middle)
                    }
                }.frame(height: 13, alignment: .leading)
            }.frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
            VStack(alignment: .trailing, spacing: 4) {
                Text(model.isUpdatingRemovalSizes ? "Updating…" : !item.isAccessible ? "Unavailable" : item.scanState == .pending ? "Waiting" : formatBytes(item.size))
                    .font(.system(size: 12, weight: .medium)).monospacedDigit().lineLimit(1)
                ZStack(alignment: .trailing) {
                    if model.isUpdatingRemovalSizes {
                        Text("")
                    } else if item.scanState == .scanning {
                        Text("so far")
                    } else if !model.isFiltering && item.isScanComplete {
                        Text(percent, format: .percent.precision(.fractionLength(1)))
                    }
                }
                .font(.system(size: 10)).foregroundStyle(OrbitStyle.muted).lineLimit(1)
                .frame(height: 13)
            }.frame(width: 76, alignment: .trailing)
            if item.isDirectory {
                Button { model.open(item) } label: {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 12, weight: .semibold))
                        .frame(width: 32, height: 36).contentShape(Rectangle())
                }
                .buttonStyle(.borderless)
                .background(.white.opacity(0.07), in: RoundedRectangle(cornerRadius: 6))
                .pointingCursor()
                .help("Open \(item.name)")
                .accessibilityLabel("Open \(item.name)")
            } else {
                Image(systemName: "doc").font(.system(size: 10)).foregroundStyle(OrbitStyle.muted)
                    .frame(width: 32, height: 36)
            }
        }
        .frame(height: 40)
        .padding(.vertical, 7).padding(.horizontal, 5)
        .frame(maxWidth: Self.contentWidth, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 7)
            .fill(selected ? Color(red: 0.29, green: 0.18, blue: 0.16) : hovered ? Color(red: 0.145, green: 0.157, blue: 0.184) : .clear))
        .overlay {
            RoundedRectangle(cornerRadius: 7)
                .strokeBorder(selected ? OrbitStyle.accent : hovered ? Color(red: 0.47, green: 0.50, blue: 0.56) : .clear,
                              lineWidth: selected ? 2.5 : 1)
                .allowsHitTesting(false)
        }
        .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
        .background(ListSelectionAppearance(hoverTracking: listHoverTracking, itemID: item.id) { [weak model] id in
            guard let model, model.hoveredListID != id else { return }
            model.hoveredListID = id
        }.accessibilityHidden(true))
        .overlay {
            ListRowInteractionView(node: item, trailingExclusion: item.isDirectory ? 43 : 0,
                                   contentWidth: Self.contentWidth,
                                   onSelect: { model.selectedID = $0.id },
                                   onOpen: { node in
                                       if node.isDirectory { model.open(node) }
                                       else { model.selectedID = node.id; model.previewSelection() }
                                   }, makeDrag: model.beginCollectionDrag,
                                   onDragEnded: model.endCollectionDrag,
                                   onInteractionActive: model.setMapInteractionActive)
                .accessibilityHidden(true)
        }
        .onHover { inside in
            listHoverTracking.rowHoverChanged(id: item.id, inside: inside)
        }
        .help(item.url.path)
        .accessibilityElement(children: .contain)
        .accessibilityAction(named: "Open folder") { model.open(item) }
    }

}
