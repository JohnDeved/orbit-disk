import AppKit
import Foundation
import Sparkle

@main struct UpdaterChecks {
    @MainActor static func main() async throws {
        let application = NSApplication.shared
        application.setActivationPolicy(.prohibited)
        let preferences = UserDefaults(suiteName: "OrbitUpdaterChecks.\(UUID().uuidString)")!
        let cache = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let model = DiskModel(cacheDirectory: cache, preferences: preferences)
        let updater = AppUpdater(model: model)
        let delegate = AppDelegate()
        delegate.model = model
        let sparkle = updater.controller.updater
        let item = SUAppcastItem.empty()
        let checks: [SPUUpdateCheck] = [.updates, .updatesInBackground, .updateInformation]

        for check in checks {
            try updater.updater(sparkle, mayPerform: check)
            try updater.updater(sparkle, shouldProceedWithUpdate: item, updateCheck: check)
        }
        precondition(delegate.applicationShouldTerminate(application) == .terminateNow)

        model.isRemoving = true
        precondition(!updater.canCheckForUpdates, "File removal must immediately disable the update menu")
        for check in checks {
            do {
                try updater.updater(sparkle, mayPerform: check)
                preconditionFailure("An update check started during file removal")
            } catch {
                precondition((error as NSError).domain == "OrbitDisk.Updates")
            }
            do {
                try updater.updater(sparkle, shouldProceedWithUpdate: item, updateCheck: check)
                preconditionFailure("An update found after removal started was allowed to proceed")
            } catch {
                precondition((error as NSError).domain == "OrbitDisk.Updates")
            }
        }
        precondition(delegate.applicationShouldTerminate(application) == .terminateCancel,
                     "A resumed update that bypasses the relaunch delegate must not terminate during removal")

        var installations = 0
        precondition(updater.updater(sparkle, shouldPostponeRelaunchForUpdate: item,
                                    untilInvokingBlock: { installations += 1 }))
        precondition(updater.isWaitingForRemoval && installations == 0)
        model.isRemoving = false
        model.isRemoving = true
        try await Task.sleep(for: .milliseconds(30))
        precondition(installations == 0, "A new removal must cancel the queued opportunity to restart")
        model.isRemoving = false
        try await Task.sleep(for: .milliseconds(30))
        precondition(installations == 1 && !updater.isWaitingForRemoval)
        model.isRemoving = true
        model.isRemoving = false
        try await Task.sleep(for: .milliseconds(30))
        precondition(installations == 1, "A postponed install callback must only run once")
        precondition(delegate.applicationShouldTerminate(application) == .terminateNow)
        precondition(!updater.updater(sparkle, shouldPostponeRelaunchForUpdate: item,
                                     untilInvokingBlock: { preconditionFailure("Unneeded postponement") }))
        model.isRemoving = true
        precondition(updater.updater(sparkle, shouldPostponeRelaunchForUpdate: item,
                                    untilInvokingBlock: { preconditionFailure("Aborted update restarted") }))
        updater.updater(sparkle, didAbortWithError: NSError(domain: "OrbitUpdaterChecks", code: 1))
        model.isRemoving = false
        try await Task.sleep(for: .milliseconds(30))
        precondition(!updater.isWaitingForRemoval)
        print("PASS: manual/background/probe checks, late discovery, quit protection, deferred restart, repeated removal, exactly-once continuation, and aborted update cleanup")
    }
}
