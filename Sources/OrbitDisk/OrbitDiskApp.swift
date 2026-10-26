import SwiftUI
import AppKit

@main
struct OrbitDiskApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model: DiskModel
    @StateObject private var updater: AppUpdater

    init() {
        let model = DiskModel()
        _model = StateObject(wrappedValue: model)
        _updater = StateObject(wrappedValue: AppUpdater(model: model))
    }

    var body: some Scene {
        WindowGroup("Orbit Disk") {
            ContentView(model: model)
                .preferredColorScheme(.dark)
                .frame(minWidth: 1060, minHeight: 680)
                .onAppear {
                    delegate.model = model
                    updater.start()
                    guard !model.didLaunch else { return }
                    model.didLaunch = true
                    if let index = CommandLine.arguments.firstIndex(of: "--scan"),
                       CommandLine.arguments.indices.contains(index + 1) {
                        model.scan(URL(fileURLWithPath: CommandLine.arguments[index + 1]))
                    } else {
                        model.restoreLastLocation()
                    }
                }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1280, height: 810)
        .commands {
            CommandGroup(after: .appInfo) {
                CheckForUpdatesButton(updater: updater)
            }
            CommandGroup(replacing: .newItem) {
                Button("Open Folder…", action: model.chooseFolder).keyboardShortcut("o")
                    .disabled(model.isRemoving)
                Button("Refresh Full Map", action: model.rescan).keyboardShortcut("r")
                    .disabled(model.root == nil || model.isScanning || model.isRefreshing || model.isRemoving)
            }
            CommandMenu("Explore") {
                Button("Go Up", action: model.goBack).keyboardShortcut("[", modifiers: .command)
                    .disabled(model.navigation.count < 2)
                Button("Open Selected Folder", action: model.openSelection).keyboardShortcut(.downArrow, modifiers: .command)
                    .disabled(model.selected?.isDirectory != true)
                Button("Reveal in Finder", action: model.revealSelection).keyboardShortcut("r", modifiers: [.command, .shift])
                    .disabled(model.selected == nil)
                Button("Quick Look", action: model.previewSelection).keyboardShortcut(.space, modifiers: [])
                    .disabled(model.selected == nil || model.selected?.isDirectory == true)
                Button("Add to Collection", action: model.collectSelection).keyboardShortcut("k")
                    .disabled(!model.canCollectSelection)
            }
        }

        Settings {
            UpdateSettingsView(updater: updater)
                .preferredColorScheme(.dark)
        }
    }
}
