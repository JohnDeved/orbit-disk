import SwiftUI
import AppKit

@main
struct OrbitDiskApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model: DiskModel
    @StateObject private var updater: AppUpdater
    @StateObject private var automation: AutomationController

    init() {
        MappingCacheReset.finishPendingReset()
        let model = DiskModel()
        _model = StateObject(wrappedValue: model)
        _updater = StateObject(wrappedValue: AppUpdater(model: model))
        _automation = StateObject(wrappedValue: AutomationController(model: model))
    }

    var body: some Scene {
        WindowGroup("Orbit Disk") {
            ContentView(model: model)
                .preferredColorScheme(.dark)
                .frame(minWidth: 1060, minHeight: 680)
                .onAppear {
                    delegate.model = model
                    delegate.stopAutomation = { automation.stop() }
                    updater.start()
                    guard !model.didLaunch else { return }
                    model.didLaunch = true
                    automation.start()
                    if let index = CommandLine.arguments.firstIndex(of: "--scan"),
                       CommandLine.arguments.indices.contains(index + 1) {
                        model.scan(URL(fileURLWithPath: CommandLine.arguments[index + 1]))
                    } else {
                        model.restoreLastLocation()
                    }
                }
        }
        .windowStyle(.hiddenTitleBar)
        .windowToolbarStyle(.unified(showsTitle: false))
        .defaultSize(width: 1280, height: 810)
        .commands {
            CommandGroup(after: .appInfo) {
                CheckForUpdatesButton(updater: updater)
                Button("AI Integration…", action: AutomationIntegration.show)
                Button("Clear Cache…", action: clearCache)
                    .disabled(model.isRemovingAcrossSessions)
            }
            CommandGroup(replacing: .newItem) {
                Button("Open Folder…", action: model.chooseFolder).keyboardShortcut("o")
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

    private func clearCache() {
        guard !model.hasActiveRemovals else { return }
        let alert = NSAlert()
        alert.messageText = "Clear cached maps?"
        alert.informativeText = "Orbit Disk will restart. Saved maps and the current collection will be cleared. Your files and Places will stay unchanged."
        alert.addButton(withTitle: "Clear Cache and Restart")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn, !model.hasActiveRemovals else { return }
        do { try MappingCacheReset.restart() }
        catch { model.errorMessage = "Couldn’t restart Orbit Disk: \(error.localizedDescription)" }
    }
}
