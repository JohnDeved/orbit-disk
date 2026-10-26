import SwiftUI
import AppKit
import Quartz
import OrbitCore

enum OrbitStyle {
    static let canvas = Color(red: 0.075, green: 0.082, blue: 0.10)
    static let sidebar = Color(red: 0.09, green: 0.10, blue: 0.125)
    static let panel = Color(red: 0.105, green: 0.118, blue: 0.145)
    static let muted = Color(red: 0.69, green: 0.72, blue: 0.78)
    static let accent = Color(red: 1, green: 0.56, blue: 0.40)
}

struct ContentView: View {
    @ObservedObject var model: DiskModel
    @FocusState private var searchFocused: Bool
    @FocusState private var fileListFocused: Bool
    @State private var collectionTargeted = false
    @State private var mapPresentation = SunburstPresentationCache()
    @State private var listHoverTracking = ListHoverTracking()

    var body: some View {
        HStack(spacing: 0) {
            sidebar
            VStack(spacing: 0) {
                if let current = model.current {
                    explorer(current)
                } else if model.isScanning {
                    scanningView
                } else {
                    welcomeView
                }
                Divider()
                collectionBar
            }
        }
        .background {
            HStack(spacing: 0) {
                OrbitStyle.sidebar.frame(width: 210)
                    .overlay(alignment: .trailing) { Rectangle().fill(.white.opacity(0.06)).frame(width: 1) }
                OrbitStyle.canvas
            }.ignoresSafeArea(.container, edges: .top)
        }
        .foregroundStyle(.white)
        .tint(OrbitStyle.accent)
        .buttonStyle(.bordered)
        .controlSize(.large)
        .toolbar { windowToolbar }
        .toolbarBackground(.hidden, for: .windowToolbar)
        .onChange(of: model.selectedID) { _, selected in
            if selected != nil { searchFocused = false; fileListFocused = true }
        }
        .sheet(isPresented: $model.showCollection) { CollectionView(model: model) }
        .sheet(isPresented: $model.showIssues) { issuesView }
        .sheet(isPresented: Binding(get: { model.previewURL != nil }, set: { if !$0 { model.previewURL = nil } })) {
            if let url = model.previewURL {
                VStack(spacing: 0) {
                    HStack {
                        Text(url.lastPathComponent).font(.headline).lineLimit(1).truncationMode(.middle)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Button("Done") { model.previewURL = nil }.pointingCursor().keyboardShortcut(.cancelAction).fixedSize()
                    }.padding(.horizontal, 16).frame(height: 52)
                    QuickLookView(url: url).frame(width: 720, height: 480)
                }.frame(width: 720, height: 532)
            }
        }
        .alert("Orbit Disk", isPresented: Binding(get: { model.errorMessage != nil && !model.showCollection && !model.showIssues && model.previewURL == nil }, set: { if !$0 && !model.showCollection && !model.showIssues && model.previewURL == nil { model.errorMessage = nil } })) {
            Button("OK", role: .cancel) { model.errorMessage = nil }
        } message: { Text(model.errorMessage ?? "") }
        .dismissSheetOnOutsideClick(isPresented: model.showCollection || model.showIssues || model.previewURL != nil || model.errorMessage != nil,
                                   enabled: !model.isRemoving) {
            if model.showCollection { model.errorMessage = nil; model.showCollection = false }
            else if model.showIssues { model.showIssues = false }
            else if model.previewURL != nil { model.previewURL = nil }
            else { model.errorMessage = nil }
        }
        .background {
            Button("Find", action: { searchFocused = true })
                .keyboardShortcut("f").disabled(model.isPartialMap).hidden().accessibilityHidden(true)
        }
    }

    @ToolbarContentBuilder
    private var windowToolbar: some ToolbarContent {
        if #available(macOS 26, *) {
            ToolbarItem(placement: .navigation) { headerLocation }
                .sharedBackgroundVisibility(.hidden)
            ToolbarSpacer(.flexible)
            ToolbarItem(placement: .primaryAction) { header }
                .sharedBackgroundVisibility(.hidden)
        } else {
            ToolbarItem(placement: .navigation) { headerLocation }
            ToolbarItem(placement: .principal) { Spacer() }
            ToolbarItem(placement: .primaryAction) { header }
        }
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Devices").font(.system(size: 11, weight: .semibold)).foregroundStyle(OrbitStyle.muted)
                .padding(.horizontal, 24).padding(.top, 20).padding(.bottom, 12)
            ScrollView {
              VStack(spacing: 0) {
               ForEach(model.volumes) { volume in
                Button { model.scan(volume.url) } label: {
                    VStack(alignment: .leading, spacing: 10) {
                        Label(volume.name, systemImage: "internaldrive")
                            .font(.system(size: 13, weight: .medium)).lineLimit(1)
                        GeometryReader { geo in
                            ZStack(alignment: .leading) {
                                Capsule().fill(.white.opacity(0.14))
                                Capsule().fill(OrbitStyle.accent)
                                    .frame(width: geo.size.width * CGFloat(max(0, min(1, Double(volume.capacity - volume.available) / Double(max(1, volume.capacity))))))
                            }
                        }.frame(height: 4)
                        Text("\(formatBytes(volume.available)) available")
                            .font(.system(size: 11)).foregroundStyle(OrbitStyle.muted).lineLimit(1).monospacedDigit()
                    }
                    .padding(13)
                    .background(RoundedRectangle(cornerRadius: 9).fill(.white.opacity(0.045)))
                }
                .buttonStyle(.plain).pointingCursor().padding(.horizontal, 12).padding(.bottom, 14)
                .help("Open saved map of \(volume.name)").disabled(model.isRemoving)
               }
              }
            }.frame(height: 100)
            Text("Places").font(.system(size: 11, weight: .semibold)).foregroundStyle(OrbitStyle.muted)
                .padding(.horizontal, 24).padding(.top, 22).padding(.bottom, 10)
            place("Home", icon: "house", url: FileManager.default.homeDirectoryForCurrentUser)
            place("Downloads", icon: "arrow.down.circle", url: FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Downloads"))
            place("Documents", icon: "doc.text", url: FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Documents"))
            place("Applications", icon: "square.grid.2x2", url: URL(fileURLWithPath: "/Applications"))
            Button(action: model.chooseFolder) { Label("Choose a folder…", systemImage: "folder.badge.plus").frame(maxWidth: .infinity, alignment: .leading) }
                .buttonStyle(.plain).pointingCursor().padding(.horizontal, 24).padding(.vertical, 12).disabled(model.isRemoving)
            Spacer(minLength: 24)
        }
        .frame(width: 210).frame(maxHeight: .infinity)
        .background(OrbitStyle.sidebar)
        .overlay(alignment: .trailing) { Rectangle().fill(.white.opacity(0.06)).frame(width: 1) }
    }

    private func place(_ title: String, icon: String, url: URL) -> some View {
        Button { model.scan(url) } label: {
            Label(title, systemImage: icon).frame(maxWidth: .infinity, alignment: .leading)
                .font(.system(size: 13)).padding(.vertical, 11).padding(.horizontal, 24)
        }.buttonStyle(.plain).pointingCursor().disabled(model.isRemoving)
    }

    private var headerLocation: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(model.current?.name ?? "Make room for what matters.")
                .font(.system(size: 17, weight: .semibold)).lineLimit(1)
            Text(model.current?.url.path ?? "A clearer view of your storage.")
                .font(.system(size: 11)).foregroundStyle(OrbitStyle.muted).lineLimit(1).truncationMode(.middle)
        }.frame(width: 280, alignment: .leading)
            .frame(height: 40)
            .padding(.leading, 142)
            .foregroundStyle(.white)
            .accessibilityElement(children: .contain)
    }

    private var header: some View {
        HStack(spacing: 18) {
            Group {
                HStack(spacing: 7) {
                    Image(systemName: "magnifyingglass").foregroundStyle(OrbitStyle.muted)
                    TextField(model.isPartialMap ? "Search after mapping" : "Search indexed files", text: $model.query)
                        .textFieldStyle(.plain).focused($searchFocused)
                        .accessibilityLabel("Search indexed files")
                    Button { model.query = "" } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.plain).pointingCursor().help("Clear search")
                        .opacity(model.query.isEmpty ? 0 : 1).disabled(model.query.isEmpty)
                        .accessibilityHidden(model.query.isEmpty)
                }
                .padding(9).frame(width: 210, height: 34).background(OrbitStyle.panel, in: RoundedRectangle(cornerRadius: 7))
                .overlay(RoundedRectangle(cornerRadius: 7).stroke(searchFocused ? OrbitStyle.accent : OrbitStyle.muted.opacity(0.6), lineWidth: 1))
                .disabled(model.isPartialMap || model.isScanning)
                .help(model.isPartialMap ? "Search becomes available when the map is saved. You can browse folders now." : "Search the saved file index")
                Button(action: model.rescan) { Label("Refresh", systemImage: "arrow.clockwise").padding(.vertical, 2) }
                    .pointingCursor().help("Refresh the full map (⌘R)").fixedSize()
                    .accessibilityLabel("Refresh")
                    .disabled(model.isScanning || model.isRefreshing || model.isRemoving)
            }.opacity(model.root == nil ? 0 : 1).disabled(model.root == nil).accessibilityHidden(model.root == nil)
            Button(action: model.chooseFolder) { Label("Open Folder…", systemImage: "folder.badge.plus").padding(.vertical, 2) }
                .pointingCursor().disabled(model.isRemoving).fixedSize()
                .accessibilityLabel("Open Folder…")
        }.padding(.trailing, 12)
            .frame(height: 40)
            .foregroundStyle(.white).tint(OrbitStyle.accent)
            .buttonStyle(.bordered).controlSize(.regular)
            .labelStyle(.titleAndIcon)
            .accessibilityElement(children: .contain)
    }

    private func explorer(_ node: DiskNode) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: 9) {
                Button(action: model.goBack) { Label("Up", systemImage: "arrow.up").padding(.vertical, 3) }
                    .pointingCursor()
                    .disabled(model.navigation.count < 2).help("Go up one folder (⌘[)")
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 7) {
                        ForEach(Array(model.navigation.enumerated()), id: \.element.id) { index, ancestor in
                            if index > 0 { Image(systemName: "chevron.right").font(.system(size: 9)).foregroundStyle(OrbitStyle.muted) }
                            Button(ancestor.name) { model.open(ancestor) }.buttonStyle(.plain).pointingCursor()
                                .foregroundStyle(index == model.navigation.count - 1 ? .white : OrbitStyle.muted).lineLimit(1).fixedSize()
                        }
                    }.font(.system(size: 12))
                }.frame(maxWidth: .infinity)
                ZStack {
                    if model.isOpeningFolder { ProgressView().controlSize(.mini).help("Opening folder") }
                }.frame(width: 14, height: 14)
                Text("\(node.fileCount.formatted()) \(node.fileCount == 1 ? "file" : "files")\(node.isScanComplete ? "" : " found")")
                    .font(.system(size: 12)).foregroundStyle(OrbitStyle.muted).monospacedDigit().lineLimit(1)
                    .frame(width: 138, alignment: .trailing)
            }.padding(.horizontal, 28).frame(height: 48)
            HStack(spacing: 0) {
                VStack(spacing: 0) {
                    SunburstView(root: node, mapRevision: model.mapRevision, cache: mapPresentation,
                                 selectedID: model.selectedID, hoveredID: model.hoveredListID,
                                 collectionCoverage: model.collectionCoverage,
                                 colorIndices: model.childColorIndices,
                                 onSelect: { model.selectedID = $0.id }, onOpen: model.open,
                                 onBack: model.goBack, canGoBack: model.navigation.count > 1,
                                 onHover: { nodes, group in
                                     model.setMapHover(nodes: nodes, groupID: group?.id, preparedGroup: group)
                                 }, makeDrag: model.beginCollectionDrag, onDragEnded: model.endCollectionDrag,
                                 onInteractionActive: model.setMapInteractionActive)
                        .padding(.horizontal, 16).frame(maxWidth: .infinity, maxHeight: .infinity)
                    HStack(spacing: 24) {
                        metric(node.isScanComplete ? "File size" : "File size so far", value: node.scanState == .pending ? "Waiting" : formatBytes(node.size))
                        Rectangle().fill(.white.opacity(0.14)).frame(width: 1, height: 30)
                        metric(node.isScanComplete ? "Allocated" : "Allocated so far", value: node.scanState == .pending ? "Waiting" : formatBytes(node.allocatedSize))
                    }.padding(.bottom, 12)
                    HStack(spacing: 18) {
                        Label("In collection", systemImage: "checkmark.circle.fill")
                        Label("Partly collected", systemImage: "minus.circle.fill")
                    }
                    .font(.system(size: 10)).foregroundStyle(OrbitStyle.muted)
                    .padding(.bottom, 10)
                    Text(model.isPartialMap ? (model.isScanning ? "Explore now · Collect finished items · Sizes update as mapping continues" : "Explore partial results · Restart mapping to finish") : "Drag any segment to collect · Click a folder to open · Center to go up")
                        .font(.system(size: 11)).foregroundStyle(OrbitStyle.muted)
                        .lineLimit(2).multilineTextAlignment(.center)
                        .frame(maxWidth: .infinity).frame(height: 28)
                        .padding(.horizontal, 16).padding(.bottom, 12)
                }
                filePanel(node).frame(width: 340)
            }.frame(maxHeight: .infinity)
            scanFooter
        }
    }

    private func metric(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.system(size: 11)).foregroundStyle(OrbitStyle.muted).lineLimit(1).frame(height: 14)
            Text(value).font(.system(size: 18, weight: .medium)).monospacedDigit().lineLimit(1).frame(height: 22)
        }.frame(width: 130, alignment: .leading)
    }

    private func filePanel(_ node: DiskNode) -> some View {
        let relatedIDs = model.mapRelatedRowIDs
        return VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(model.isFiltering ? "Search results" : "Contents").font(.system(size: 15, weight: .semibold))
                Spacer()
                Menu { Button("Largest first") { model.sortByName = false }; Button("Name") { model.sortByName = true } } label: {
                    Label(model.sortByName ? "Name" : "Size", systemImage: "arrow.down").frame(width: 58, alignment: .trailing)
                }.menuStyle(.borderlessButton).fixedSize().font(.system(size: 11)).pointingCursor().help("Sort contents")
            }.padding(.horizontal, 18).padding(.top, 18).padding(.bottom, 10)
            VStack(alignment: .leading, spacing: 4) {
                Text(model.isFiltering ? "\(model.searchMatchCount.formatted()) \(model.searchMatchCount == 1 ? "match" : "matches") in this index" : "\(node.children.count.formatted()) items · \(node.isScanComplete ? (model.sortByName ? "sorted by name" : "largest first") : "sizes measured so far")")
                    .foregroundStyle(OrbitStyle.muted).lineLimit(1)
                if let description = model.mapHoverDescription {
                    Label(description, systemImage: "cursorarrow")
                        .foregroundStyle(OrbitStyle.accent).lineLimit(2).truncationMode(.middle)
                } else {
                    Text(model.isFiltering && model.searchMatchCount > 300 ? "Showing the 300 largest matches. Refine your search for more." : "Hover to locate an item in the map.")
                        .foregroundStyle(OrbitStyle.muted).lineLimit(2)
                }
            }
            .font(.system(size: 11)).frame(height: 48, alignment: .topLeading)
            .padding(.horizontal, 18).padding(.bottom, 10)
            Divider().padding(.horizontal, 18)
            if model.displayedNodes.isEmpty {
                VStack(spacing: 12) {
                    Image(systemName: model.isFiltering ? "magnifyingglass" : "folder").font(.system(size: 25)).foregroundStyle(OrbitStyle.muted)
                    Text(model.isSearching ? "Searching…" : model.isFiltering ? "No matching files" : !node.isAccessible ? "Contents unavailable. See skipped items." : node.scanState == .pending ? "Waiting to map this folder" : !node.isScanComplete ? "Reading this folder…" : "This folder is empty")
                        .font(.system(size: 13)).foregroundStyle(OrbitStyle.muted)
                    if !node.isScanComplete {
                        Text(model.isScanning ? "You can explore other folders while mapping continues." : "Restart mapping to finish this folder.")
                            .font(.system(size: 11)).foregroundStyle(OrbitStyle.muted)
                            .multilineTextAlignment(.center).padding(.horizontal, 22)
                    }
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollViewReader { proxy in
                    List(selection: $model.selectedID) {
                        ForEach(model.displayedItems) { item in
                            FileRowView(model: model, item: item.node, parentSize: node.size, related: relatedIDs.contains(item.id),
                                        listHoverTracking: listHoverTracking).tag(item.id)
                                .listRowSeparator(.hidden)
                                .listRowBackground(Color.clear)
                                .listRowInsets(EdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8))
                        }
                    }.listStyle(.plain).scrollContentBackground(.hidden).focused($fileListFocused)
                    .onDisappear { listHoverTracking.detach() }
                    .onChange(of: relatedIDs) { _, ids in
                        guard !ids.isEmpty else { return }
                        let items = model.displayedItems
                        if listHoverTracking.revealRow(expectedCount: items.count, rowIndex: {
                            items.firstIndex { ids.contains($0.id) }
                        }) { return }
                        if ids.count == 1, let id = ids.first {
                            proxy.scrollTo(id)
                        } else if let first = model.displayedNodes.first(where: { ids.contains($0.id) }) {
                            proxy.scrollTo(first.id)
                        }
                    }
                    .contextMenu(forSelectionType: String.self) { ids in
                        if let id = ids.first, let item = model.node(withID: id) {
                            itemActions(item)
                        }
                    } primaryAction: { ids in
                        guard let id = ids.first, let item = model.node(withID: id) else { return }
                        if item.isDirectory { model.open(item) }
                        else { model.selectedID = item.id; model.previewSelection() }
                    }
                    .onKeyPress(.return) {
                        guard model.selected?.isDirectory == true else { return .ignored }
                        model.openSelection()
                        return .handled
                    }
                    .onKeyPress(.space) {
                        guard model.selected != nil, model.selected?.isDirectory == false else { return .ignored }
                        model.previewSelection()
                        return .handled
                    }
                    .onKeyPress(.downArrow, phases: .down) { key in
                        guard key.modifiers.contains(.command), model.selected?.isDirectory == true else { return .ignored }
                        model.openSelection()
                        return .handled
                    }
                }
            }
            inspector
        }
        .background(OrbitStyle.panel)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .padding(.trailing, 24).padding(.bottom, 18)
    }


    @ViewBuilder
    private func itemActions(_ item: DiskNode) -> some View {
        if item.isDirectory { Button("Open Folder") { model.open(item) } }
        Button("Reveal in Finder") { NSWorkspace.shared.activateFileViewerSelecting([item.url]) }
        if !item.isDirectory { Button("Quick Look") { model.previewURL = item.url } }
        Button("Add to Collection") { model.selectedID = item.id; model.collectSelection() }
            .disabled(!model.canCollect(item))
    }

    private var inspector: some View {
        let selected = model.selected
        let collected = selected.map { model.collectionCoverage.state(for: $0.id) == .included } ?? false
        return VStack(alignment: .leading, spacing: 10) {
            Divider()
                Text(selected?.name ?? "Select an item").font(.system(size: 12, weight: .semibold)).lineLimit(1).truncationMode(.middle)
                    .frame(height: 15, alignment: .leading)
                Text(selected?.url.path ?? "Drag a row or segment to collect, or select one to use these actions.")
                    .font(.system(size: 10)).foregroundStyle(OrbitStyle.muted)
                    .lineLimit(2).truncationMode(.middle).textSelection(.enabled)
                    .frame(height: 26, alignment: .topLeading)
                HStack(spacing: 8) {
                    Button(action: model.revealSelection) {
                        Label("Finder", systemImage: "folder").frame(maxWidth: .infinity)
                    }.pointingCursor().help("Reveal in Finder (⇧⌘R)")
                    if selected?.isDirectory == true {
                        Button(action: model.openSelection) {
                            Label("Open", systemImage: "arrow.up.forward").frame(maxWidth: .infinity)
                        }.pointingCursor().help("Open selected folder (Return)")
                    } else {
                        Button(action: model.previewSelection) {
                            Label("Preview", systemImage: "eye").frame(maxWidth: .infinity)
                        }.pointingCursor().help("Quick Look (Space)")
                    }
                }.controlSize(.regular).frame(height: 26).disabled(selected == nil)
                Button(action: model.collectSelection) {
                    Label(collected ? "In Collection" : selected?.isScanComplete == false ? "Available when mapped" : "Add to Collection", systemImage: collected ? "checkmark" : selected?.isScanComplete == false ? "clock" : "plus")
                        .frame(maxWidth: .infinity).padding(.vertical, 3)
                        .foregroundStyle(model.canCollectSelection ? OrbitStyle.canvas : OrbitStyle.muted)
                }
                .buttonStyle(.borderedProminent).tint(OrbitStyle.accent).foregroundStyle(OrbitStyle.canvas)
                .pointingCursor().disabled(!model.canCollectSelection).help("Add selected item (⌘K)")
                .frame(height: 32)
        }.padding(.horizontal, 18).padding(.bottom, 18).frame(height: 164, alignment: .top)
    }

    private var scanFooter: some View {
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
                    Text("\(found.formatted()) \(found == 1 ? "file" : "files") found").monospacedDigit().lineLimit(1)
                        .frame(width: 140, alignment: .leading)
                } else if let result = model.scanResult {
                    HStack(spacing: 4) {
                        if let date = model.indexedAt {
                            Text(model.isSavedMap ? "Saved map" : "Indexed")
                            Text(date, style: .relative)
                            Text("ago")
                        }
                    }.lineLimit(1).frame(width: 172, alignment: .leading)
                    if result.skippedCount > 0 {
                        Button("\(result.skippedCount.formatted()) skipped") { model.showIssues = true }
                            .buttonStyle(.link).pointingCursor().fixedSize()
                    }
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
            Text(model.isPartialMap ? (model.isScanning ? model.statusMessage ?? model.scanPath : "You can keep exploring. Restart mapping to finish and save this map.") : model.statusMessage ?? (model.isScanning ? model.scanTitle : ""))
                .lineLimit(1).truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading).frame(height: 14, alignment: .top)
        }
        .font(.system(size: 11)).foregroundStyle(OrbitStyle.muted)
        .padding(.horizontal, 28).frame(height: 44, alignment: .top)
        .padding(.bottom, 12)
    }

    private var collectionBar: some View {
        let dragging = model.draggingLabel != nil
        let ready = collectionTargeted && model.canAcceptCollectionDrop
        return HStack(spacing: 16) {
            ZStack {
                if model.isValidatingCollection { ProgressView().controlSize(.small) }
                else {
                    Image(systemName: ready ? "tray.and.arrow.down.fill" : "tray.and.arrow.down")
                        .font(.system(size: 28)).foregroundStyle(OrbitStyle.accent)
                }
            }.frame(width: 36, height: 36)
            VStack(alignment: .leading, spacing: 5) {
                Text(model.isValidatingCollection ? "Checking your collection…" : ready ? "Release to collect" : dragging ? "Drop here to collect" : model.collection.isEmpty ? "Drag files and folders here" : "\(model.collection.count) \(model.collection.count == 1 ? "item" : "items") in your collection")
                    .font(.system(size: 14, weight: .semibold)).lineLimit(1).truncationMode(.middle)
                Text(dragging ? model.draggingLabel ?? "" : model.isPartialMap ? "Collect finished items now. Removal is available after the map is saved." : model.collection.isEmpty ? "Drag from the map or file list. Review before removing anything." : "\(formatBytes(model.collectionSize)) · Drag more items here. Files stay in place until reviewed.")
                    .font(.system(size: 11)).foregroundStyle(OrbitStyle.muted).lineLimit(1).truncationMode(.middle)
            }.frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
            Button { model.showCollection = true } label: {
                Label("Review Collection", systemImage: "tray.full").padding(.vertical, 3)
            }
            .pointingCursor().disabled(model.collection.isEmpty || model.isValidatingCollection || dragging).fixedSize()
        }
        .padding(.horizontal, 20).frame(maxWidth: .infinity).frame(height: 80)
        .background(ready ? OrbitStyle.accent.opacity(0.16) : dragging ? OrbitStyle.accent.opacity(0.06) : .clear)
        .overlay {
            RoundedRectangle(cornerRadius: 9)
                .stroke(ready || dragging ? OrbitStyle.accent : .clear,
                        style: StrokeStyle(lineWidth: ready ? 2 : 1, dash: dragging && !ready ? [6, 4] : []))
        }
        .contentShape(Rectangle())
        .onDrop(of: [DiskModel.collectionDragType.rawValue], isTargeted: $collectionTargeted) { providers in
            guard model.canAcceptCollectionDrop, let provider = providers.first else { return false }
            provider.loadDataRepresentation(forTypeIdentifier: DiskModel.collectionDragType.rawValue) { data, _ in
                guard let data else { return }
                Task { @MainActor in _ = model.collectDrag(data: data) }
            }
            return true
        }
        .accessibilityElement(children: .contain)
        .padding(.horizontal, 20).padding(.vertical, 12)
        .background(OrbitStyle.sidebar)
    }

    private var welcomeView: some View {
        VStack(spacing: 22) {
            Image(systemName: "chart.pie").font(.system(size: 74, weight: .ultraLight)).foregroundStyle(OrbitStyle.accent)
            VStack(spacing: 10) {
                Text("Every byte has a place.").font(.system(size: 30, weight: .medium))
                Text("Choose a disk or folder to see what’s taking up space.")
                    .font(.system(size: 14)).foregroundStyle(OrbitStyle.muted)
            }
            Button("Choose Folder", action: model.chooseFolder)
                .buttonStyle(.borderedProminent).foregroundStyle(OrbitStyle.canvas).controlSize(.large)
                .pointingCursor()
                .padding(.top, 6)
            Text("Files stay on your Mac. Nothing is moved without your review.")
                .font(.system(size: 11)).foregroundStyle(OrbitStyle.muted)
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var scanningView: some View {
        VStack(spacing: 20) {
            ProgressView().controlSize(.large)
            Text(model.scanTitle).font(.system(size: 26, weight: .medium))
            Text("Folders appear as they’re found. You can start exploring before mapping finishes.")
                .font(.system(size: 12)).foregroundStyle(OrbitStyle.muted)
            HStack(spacing: 24) {
                metric("Items found", value: model.progressCount.formatted())
                metric("File size", value: formatBytes(model.progressBytes))
            }
            Text(model.scanPath).font(.system(size: 11)).foregroundStyle(OrbitStyle.muted)
                .lineLimit(1).truncationMode(.middle).frame(maxWidth: 520)
            Button("Cancel Scan", action: model.cancelScan).pointingCursor().keyboardShortcut(.cancelAction)
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var issuesView: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Some items could not be scanned").font(.title2)
            Text("macOS may restrict protected folders. The map includes accessible items only. You can grant Orbit Disk Full Disk Access in System Settings and rescan.")
                .foregroundStyle(OrbitStyle.muted)
            ScrollView { Text(model.scanResult?.issues.joined(separator: "\n\n") ?? "").frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled) }
            HStack {
                Button("Privacy Settings") { NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")!) }
                    .pointingCursor()
                Spacer()
                Button("Done") { model.showIssues = false }.pointingCursor().keyboardShortcut(.cancelAction)
            }
        }.padding(28).frame(width: 650, height: 420)
    }
}

struct CollectionView: View {
    @ObservedObject var model: DiskModel
    @State private var confirmPermanentDeletion = false
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Review your collection").font(.system(size: 23, weight: .semibold)).lineLimit(1).frame(height: 28)
                    Group {
                        if let message = model.errorMessage {
                            VStack(alignment: .leading, spacing: 6) {
                                HStack {
                                    Label("Some items could not be removed", systemImage: "exclamationmark.triangle")
                                        .font(.system(size: 12, weight: .semibold)).lineLimit(1)
                                    Spacer()
                                    Button("Dismiss") { model.errorMessage = nil }.pointingCursor().controlSize(.small)
                                }
                                ScrollView {
                                    Text(message).font(.system(size: 11)).textSelection(.enabled)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                }
                            }
                        } else {
                            Text(model.isPartialMap ? "\(formatBytes(model.collectionSize)) collected so far. Removal is available after the map is saved." : "\(formatBytes(model.collectionSize)) in file sizes. Choose Trash or permanent deletion after review.")
                                .font(.system(size: 12)).foregroundStyle(OrbitStyle.muted).lineLimit(2)
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading).frame(height: 56, alignment: .top)
                }
                Button("Done") { model.errorMessage = nil; model.showCollection = false }.pointingCursor().keyboardShortcut(.cancelAction).disabled(model.isRemoving).fixedSize()
            }
            List {
                ForEach(model.collection) { item in
                    HStack {
                        Image(systemName: item.isDirectory ? "folder" : "doc").foregroundStyle(OrbitStyle.accent)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(item.name).fontWeight(.medium).lineLimit(1).truncationMode(.middle)
                            Text(item.url.path).font(.system(size: 10)).foregroundStyle(OrbitStyle.muted).lineLimit(1).truncationMode(.middle)
                        }
                        Spacer()
                        Text(formatBytes(item.size)).monospacedDigit().lineLimit(1).frame(width: 80, alignment: .trailing)
                        Button { model.collection.removeAll { $0.id == item.id } } label: { Label("Remove", systemImage: "minus.circle").padding(.vertical, 3) }
                            .buttonStyle(.bordered).pointingCursor().help("Remove from collection").disabled(model.isRemoving || model.isValidatingCollection)
                    }.frame(height: 40).padding(.vertical, 6)
                }
            }.listStyle(.inset)
            HStack {
                Button("Clear Collection") { model.collection = []; model.errorMessage = nil; model.showCollection = false }.pointingCursor().disabled(model.isRemoving || model.isValidatingCollection)
                Spacer()
                if model.isRemoving { ProgressView().controlSize(.small) }
                Button(role: .destructive) { confirmPermanentDeletion = true } label: {
                    Text("Delete Permanently…")
                }
                    .pointingCursor()
                    .disabled(model.collection.isEmpty || model.isRemoving || model.isValidatingCollection || model.isPartialMap || model.isScanning)
                Button { model.confirmTrash = true } label: {
                    Text("Move to Trash…").foregroundStyle(OrbitStyle.canvas)
                }
                    .buttonStyle(.borderedProminent).tint(OrbitStyle.accent).foregroundStyle(OrbitStyle.canvas)
                    .pointingCursor()
                    .disabled(model.collection.isEmpty || model.isRemoving || model.isValidatingCollection || model.isPartialMap || model.isScanning)
            }
        }
        .padding(26).frame(width: 720, height: 470)
        .background(OrbitStyle.canvas)
        .buttonStyle(.bordered).controlSize(.large)
        .interactiveDismissDisabled(model.isRemoving)
        .sheet(isPresented: $confirmPermanentDeletion) { permanentDeletionConfirmation }
        .alert("Move \(model.collection.count) \(model.collection.count == 1 ? "item" : "items") to Trash?", isPresented: $model.confirmTrash) {
            Button("Cancel", role: .cancel) {}
            Button("Move to Trash", role: .destructive, action: model.moveCollectionToTrash)
        } message: { Text("You can restore these items from Trash in Finder. Orbit Disk never empties the Trash.") }
        .dismissSheetOnOutsideClick(isPresented: confirmPermanentDeletion || model.confirmTrash,
                                   enabled: !model.isRemoving) {
            if confirmPermanentDeletion { confirmPermanentDeletion = false }
            else { model.confirmTrash = false }
        }
    }

    private var permanentDeletionConfirmation: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Delete \(model.collection.count) \(model.collection.count == 1 ? "item" : "items") permanently?")
                .font(.system(size: 23, weight: .semibold))
            Text("These items and all contents of the listed folders will be deleted immediately. They will not go to Trash. This cannot be undone.")
                .font(.system(size: 13)).fixedSize(horizontal: false, vertical: true)
            Text("\(formatBytes(model.collectionSize)) in file sizes")
                .font(.system(size: 12)).foregroundStyle(OrbitStyle.muted)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    ForEach(model.collection) { item in
                        VStack(alignment: .leading, spacing: 4) {
                            Label(item.name, systemImage: item.isDirectory ? "folder" : "doc")
                                .font(.system(size: 13, weight: .medium))
                            Text(item.url.path).font(.system(size: 11)).foregroundStyle(OrbitStyle.muted)
                                .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }.frame(maxWidth: .infinity, alignment: .leading).padding(14)
            }
            .background(OrbitStyle.panel, in: RoundedRectangle(cornerRadius: 8))
            HStack {
                Spacer()
                Button("Cancel") { confirmPermanentDeletion = false }
                    .keyboardShortcut(.cancelAction).pointingCursor()
                Button(role: .destructive) {
                    confirmPermanentDeletion = false
                    model.deleteCollectionPermanently()
                } label: {
                    Text("Delete Permanently")
                }
                .pointingCursor()
                .disabled(model.collection.isEmpty || model.isRemoving || model.isValidatingCollection || model.isPartialMap || model.isScanning)
            }
        }
        .padding(26).frame(width: 640, height: 430)
        .background(OrbitStyle.canvas).foregroundStyle(.white)
        .buttonStyle(.bordered).controlSize(.large)
    }
}

struct QuickLookView: NSViewRepresentable {
    let url: URL
    func makeNSView(context: Context) -> QLPreviewView {
        let view = QLPreviewView(frame: .zero, style: .normal)!
        view.autostarts = true
        view.previewItem = url as NSURL
        return view
    }
    func updateNSView(_ nsView: QLPreviewView, context: Context) { nsView.previewItem = url as NSURL }
}
