import AppKit
import Combine
import Sparkle
import SwiftUI

@MainActor
final class AppUpdater: NSObject, ObservableObject, SPUUpdaterDelegate {
    @Published private(set) var canCheckForUpdates = false
    @Published private(set) var automaticallyChecksForUpdates = false
    @Published private(set) var isWaitingForRemoval = false

    private weak var model: DiskModel?
    private(set) var controller: SPUStandardUpdaterController!
    private var removalObservation: AnyCancellable?
    private var pendingRelaunch: (() -> Void)?
    private var started = false

    init(model: DiskModel) {
        self.model = model
        super.init()
        controller = SPUStandardUpdaterController(startingUpdater: false, updaterDelegate: self, userDriverDelegate: nil)
        controller.updater.publisher(for: \.canCheckForUpdates)
            .combineLatest(model.$isRemovingAcrossSessions)
            .map { $0 && !$1 }
            .assign(to: &$canCheckForUpdates)
        controller.updater.publisher(for: \.automaticallyChecksForUpdates)
            .assign(to: &$automaticallyChecksForUpdates)
        removalObservation = model.$isRemovingAcrossSessions.removeDuplicates().sink { [weak self] removing in
            guard !removing else { return }
            // Published emits before the model changes; recheck on the next main-loop turn.
            DispatchQueue.main.async { self?.resumePendingRelaunch() }
        }
    }

    func start() {
        guard !started else { return }
        started = true
        controller.startUpdater()
    }

    func checkForUpdates() {
        guard canCheckForUpdates, model?.hasActiveRemovals != true else { return }
        controller.checkForUpdates(nil)
    }

    func setAutomaticallyChecksForUpdates(_ enabled: Bool) {
        controller.updater.automaticallyChecksForUpdates = enabled
    }

    func updater(_ updater: SPUUpdater, mayPerform updateCheck: SPUUpdateCheck) throws {
        try requireRemovalFinished()
    }

    func updater(_ updater: SPUUpdater, shouldProceedWithUpdate updateItem: SUAppcastItem,
                 updateCheck: SPUUpdateCheck) throws {
        try requireRemovalFinished()
    }

    func updater(_ updater: SPUUpdater, shouldPostponeRelaunchForUpdate item: SUAppcastItem,
                 untilInvokingBlock installHandler: @escaping () -> Void) -> Bool {
        guard model?.hasActiveRemovals == true else { return false }
        pendingRelaunch = installHandler
        isWaitingForRemoval = true
        return true
    }

    func updater(_ updater: SPUUpdater, didAbortWithError error: Error) {
        pendingRelaunch = nil
        isWaitingForRemoval = false
    }

    private func requireRemovalFinished() throws {
        guard model?.hasActiveRemovals == true else { return }
        throw NSError(domain: "OrbitDisk.Updates", code: 1, userInfo: [
            NSLocalizedDescriptionKey: "Files are still being removed.",
            NSLocalizedRecoverySuggestionErrorKey: "Check for updates after file removal finishes."
        ])
    }

    private func resumePendingRelaunch() {
        guard model?.hasActiveRemovals != true, let pendingRelaunch else { return }
        self.pendingRelaunch = nil
        isWaitingForRemoval = false
        pendingRelaunch()
    }
}

struct CheckForUpdatesButton: View {
    @ObservedObject var updater: AppUpdater

    var body: some View {
        Button("Check for Updates…", action: updater.checkForUpdates)
            .pointingCursor()
            .disabled(!updater.canCheckForUpdates)
    }
}

struct UpdateSettingsView: View {
    @ObservedObject var updater: AppUpdater

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Updates").font(.headline)
            Toggle("Automatically check for updates", isOn: Binding(
                get: { updater.automaticallyChecksForUpdates },
                set: updater.setAutomaticallyChecksForUpdates
            ))
            Text("Checks for updates daily while Orbit Disk is open.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Text(updater.isWaitingForRemoval
                 ? "The update will restart Orbit Disk after file removal finishes."
                 : "Restarting clears the current collection.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(height: 36, alignment: .topLeading)
            CheckForUpdatesButton(updater: updater)
        }
        .padding(24)
        .frame(width: 420, alignment: .leading)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    weak var model: DiskModel?
    var stopAutomation: (() -> Void)?

    func applicationWillTerminate(_ notification: Notification) { stopAutomation?() }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        // Sparkle can resume a downloaded update without asking its relaunch delegate again.
        guard model?.hasActiveRemovals != true else {
            model?.statusMessage = "Files are still being removed. Quit after this finishes."
            return .terminateCancel
        }
        return .terminateNow
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}
