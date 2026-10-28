import SwiftUI

struct MappingProgressFooter: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @ObservedObject var model: DiskModel
    @ObservedObject var progressState: MappingProgressState

    static let contentHeight = 51.0

    var body: some View {
        Group {
            if let progress = progressState.value {
                VStack(alignment: .leading, spacing: 3) {
                    MappingProgressView(progress: progress)
                        .opacity(progress.phase == .saved ? 0 : 1)
                        .accessibilityHidden(progress.phase == .saved)
                        .allowsHitTesting(progress.phase != .saved)
                        .animation(reduceMotion ? nil : .easeOut(duration: 0.18), value: progress.phase == .saved)
                    HStack(spacing: 10) {
                        Text(progress.rootName).fontWeight(.medium)
                            .lineLimit(1).truncationMode(.middle)
                            .frame(maxWidth: 160, alignment: .leading)
                            .fixedSize(horizontal: true, vertical: false)
                            .help("Map of \(progress.rootName)")
                        if progress.phase == .saved {
                            completedStatus
                        } else {
                            indexDate
                        }
                        skippedItems
                        Text(model.statusMessage ?? "")
                            .lineLimit(1).truncationMode(.middle)
                            .help(model.statusMessage ?? "")
                            .frame(maxWidth: .infinity, alignment: .trailing)
                        progressAction(progress)
                    }.frame(height: 20)
                }
            } else if model.root != nil || model.isScanning || model.isRefreshing {
                legacyStatus.frame(maxHeight: .infinity, alignment: .bottom)
            } else {
                Color.clear
            }
        }
        .font(.system(size: 11)).foregroundStyle(OrbitStyle.muted)
        .frame(height: Self.contentHeight, alignment: .top)
        .padding(.horizontal, 28).padding(.bottom, 12)
    }

    private func progressAction(_ progress: MappingProgressPresentation) -> some View {
        Group {
            if progress.isDiscovering {
                Button("Stop", action: model.cancelScan).keyboardShortcut(.cancelAction)
                    .help("Stop mapping this location").pointingCursor()
            } else if progress.isPaused {
                Button("Resume", action: model.resumeMapping)
                    .help("Continue mapping this location").pointingCursor()
            } else if progress.phase == .paused {
                Button("Refresh", action: model.rescan)
                    .help("Refresh this map and restart updates").pointingCursor()
            } else if model.isRefreshing && !model.isScanning && !model.isPartialMap {
                Button("Pause", action: model.cancelScan).help("Pause the current refresh").pointingCursor()
            } else {
                Color.clear.accessibilityHidden(true)
            }
        }.buttonStyle(.bordered).controlSize(.small)
            .frame(width: 64, height: 20, alignment: .trailing)
    }

    private var indexDate: some View {
        HStack(spacing: 4) {
            if let date = model.indexedAt {
                Text("Indexed")
                Text(date, style: .relative)
                Text("ago")
            }
        }.lineLimit(1).fixedSize(horizontal: true, vertical: false)
    }

    private var completedStatus: some View {
        HStack(spacing: 4) {
            Text("Indexed").fontWeight(.medium).foregroundStyle(.white)
            if let date = model.indexedAt {
                Text("· updated")
                Text(date, style: .relative)
                Text("ago")
            }
        }
        .lineLimit(1).fixedSize(horizontal: true, vertical: false)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var skippedItems: some View {
        if let skipped = model.scanResult?.skippedCount, skipped > 0 {
            Button("\(skipped.formatted()) skipped") { model.showIssues = true }
                .buttonStyle(.link).pointingCursor().fixedSize()
        }
    }

    private var legacyStatus: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 7) {
                ZStack {
                    if model.isScanning || model.isRefreshing { ProgressView().controlSize(.mini) }
                    else { Image(systemName: model.isPartialMap || model.updatesPaused ? "pause.circle" : "checkmark.circle") }
                }.frame(width: 14, height: 14)
                if model.isPartialMap {
                    Text(model.isScanning ? model.scanTitle : "Partial map · Not saved")
                        .fontWeight(.medium).foregroundStyle(.white).lineLimit(1)
                        .frame(width: 172, alignment: .leading)
                    let found = model.root?.fileCount ?? 0
                    Text(model.isUpdatingRemovalSizes ? "Updating sizes…" : "\(found.formatted()) \(found == 1 ? "file" : "files") found")
                        .monospacedDigit().lineLimit(1).frame(width: 140, alignment: .leading)
                } else if model.scanResult != nil {
                    indexDate.frame(width: 172, alignment: .leading)
                    skippedItems
                }
                Spacer(minLength: 8)
                if model.isScanning {
                    Button(action: model.cancelScan) { Text("Stop Mapping").frame(width: 110) }
                        .pointingCursor().keyboardShortcut(.cancelAction)
                        .frame(width: 130, alignment: .trailing)
                } else if model.isPartialMap {
                    Button(action: model.rescan) { Text("Restart Mapping").frame(width: 110) }
                        .pointingCursor().frame(width: 130, alignment: .trailing)
                } else if model.isRefreshing {
                    Button(action: model.cancelScan) { Text("Pause").frame(width: 110) }
                        .pointingCursor().frame(width: 130, alignment: .trailing)
                }
            }.controlSize(.small).frame(height: 26)
            Text(model.isPartialMap ? (model.statusMessage ?? (model.isScanning ? model.scanPath : "Restart mapping to finish."))
                 : model.statusMessage ?? (model.isScanning ? model.scanTitle : ""))
                .lineLimit(1).truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading).frame(height: 14, alignment: .top)
        }
    }
}

struct MappingProgressView: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let progress: MappingProgressPresentation
    private let memoryColor = Color(red: 0.914, green: 0.875, blue: 0.812)

    var body: some View {
        let hasFiles = progress.discoveredFiles > 0 || progress.phase == .saved
        let savedShare = progress.savedShare ?? 0
        VStack(spacing: 4) {
            HStack(spacing: 16) {
                MappingProgressSummary(count: progress.discoveredFiles, label: "found", color: memoryColor,
                                       detail: "Files discovered in memory and available to browse. The total grows while finding files.")
                MappingProgressSummary(count: progress.cachedFiles, label: "indexed", color: OrbitStyle.accent,
                                       detail: "Discovered files stored in the local index. Orange shows their share of the files found so far.")
                Spacer(minLength: 8)
                HStack(spacing: 6) {
                    phaseIndicator.frame(width: 12, height: 12).accessibilityHidden(true)
                    Text(phaseText).lineLimit(1)
                }.foregroundStyle(OrbitStyle.muted)
                    .accessibilityElement(children: .combine)
            }.font(.system(size: 11)).monospacedDigit().frame(height: 14)
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    Rectangle().fill(Color(red: 0.208, green: 0.224, blue: 0.267))
                    if hasFiles {
                        Rectangle().fill(memoryColor)
                        if progress.savedShare == nil {
                            Canvas { context, size in
                                var stripes = Path()
                                for x in stride(from: -size.height, to: size.width, by: 10) {
                                    stripes.move(to: CGPoint(x: x, y: size.height))
                                    stripes.addLine(to: CGPoint(x: x + size.height, y: 0))
                                }
                                context.stroke(stripes, with: .color(OrbitStyle.canvas), lineWidth: 2)
                            }
                        } else {
                            Rectangle().fill(OrbitStyle.accent)
                                .frame(width: geometry.size.width * savedShare)
                            if savedShare > 0 && savedShare < 1 {
                                Rectangle().fill(OrbitStyle.canvas).frame(width: 1)
                                    .offset(x: geometry.size.width * savedShare)
                            }
                        }
                    }
                }
                .animation(reduceMotion ? nil : .easeOut(duration: 0.35), value: savedShare)
            }.frame(height: 10).clipShape(RoundedRectangle(cornerRadius: 5))
                .help(progress.savedShare == nil ? "The indexed share is being updated. Stripes do not represent a percentage."
                      : "Files found so far: ivory is available in memory; orange is indexed. Finding files continues until the status changes.")
                .accessibilityHidden(true)
        }.frame(height: 28, alignment: .top)
    }

    private var phaseText: String {
        switch progress.phase {
        case .finding: return progress.discoveredFiles > 0 ? "Finding more files" : "Finding files"
        case .saving: return "Indexing files"
        case .finishing: return "Finishing index"
        case .saved: return "Indexed"
        case .updating: return "Updating map"
        case .paused: return "Paused"
        case .interrupted: return "Interrupted"
        }
    }

    @ViewBuilder private var phaseIndicator: some View {
        switch progress.phase {
        case .saved: Image(systemName: "checkmark")
        case .paused: Image(systemName: "pause.fill")
        case .interrupted: Image(systemName: "exclamationmark.triangle")
        default:
            if reduceMotion { Image(systemName: "ellipsis") }
            else { ProgressView().controlSize(.mini) }
        }
    }
}

private struct MappingProgressSummary: View {
    let count: Int?
    let label: String
    let color: Color
    let detail: String

    var body: some View {
        HStack(spacing: 6) {
            RoundedRectangle(cornerRadius: 2).fill(color).frame(width: 7, height: 7)
            if let count {
                Text(count.formatted()).fontWeight(.medium).foregroundStyle(.white)
                Text(label).foregroundStyle(OrbitStyle.muted)
            } else {
                Text("Updating indexed count").foregroundStyle(OrbitStyle.muted)
            }
        }.lineLimit(1).fixedSize(horizontal: true, vertical: false)
        .frame(width: 164, height: 14, alignment: .leading)
        .layoutPriority(1)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label == "found" ? "Files found" : "Files indexed")
        .accessibilityValue(count.map { "\($0.formatted()) \($0 == 1 ? "file" : "files")" } ?? "Updating indexed count")
        .help(detail)
    }
}
