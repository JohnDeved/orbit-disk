import AppKit
import Combine
import Foundation
import OrbitCore

@main struct Check {
    @MainActor static func main() async throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let folder = base.appendingPathComponent("root", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try Data([1]).write(to: folder.appendingPathComponent("file"))
        let second = base.appendingPathComponent("second", isDirectory: true)
        try FileManager.default.createDirectory(at: second, withIntermediateDirectories: true)
        try Data([2]).write(to: second.appendingPathComponent("second-file"))
        let action = CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : "cancel"
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("index"),
                              preferences: UserDefaults(suiteName: UUID().uuidString)!)
        var armed = false, triggered = false
        var cancelledState: (root: String?, navigation: [String], partial: Bool)?
        if action.hasPrefix("begin-") {
            model.testBeforeLifecycleTransition = { name in if name == "beginPreview" { armed = true } }
        } else if action.hasPrefix("finish-") {
            model.testBeforeLifecycleTransition = { name in if name == "finishInstall" { armed = true } }
        } else { model.testBeforeRootInstall = { armed = true } }
        let subscription = model.objectWillChange.sink {
            MainActor.assumeIsolated {
                guard armed, !triggered else { return }
                armed = false; triggered = true
                print("during-notification root=\(model.root?.id ?? "nil") navigation=\(model.navigation.count) scanning=\(model.isScanning)")
                if action.hasSuffix("switch") { model.scan(second) }
                else if action.hasSuffix("selection") { model.selectedID = model.current?.children.first?.id ?? "new-selection" }
                else {
                    model.cancelScan()
                    cancelledState = (model.root?.id, model.navigation.map(\.id), model.isPartialMap)
                }
                print("after-cancel root=\(model.root?.id ?? "nil") navigation=\(model.navigation.count) scanning=\(model.isScanning)")
            }
        }
        model.scan(folder)
        for _ in 0..<500 {
            if triggered { break }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        guard triggered else { fatalError("root installation was not reached") }
        try await Task.sleep(nanoseconds: 100_000_000)
        print("after-install root=\(model.root?.id ?? "nil") navigation=\(model.navigation.count) scanning=\(model.isScanning) partial=\(model.isPartialMap)")
        if action.hasSuffix("switch") {
            for _ in 0..<500 {
                if model.root?.id == second.path && !model.isScanning { break }
                try await Task.sleep(nanoseconds: 20_000_000)
            }
            print("after-switch root=\(model.root?.id ?? "nil") navigation=\(model.navigation.count) scanning=\(model.isScanning)")
            precondition(model.root?.id == second.path && !model.isScanning, "A synchronous root switch must complete")
        }
        if action == "cancel" || action == "begin-cancel" {
            precondition(!model.isScanning && model.root == nil && model.navigation.isEmpty,
                         "A cancelled initial install must not publish its old folder")
        }
        if action == "finish-cancel" {
            guard let cancelledState else { fatalError("Cancellation did not run during the handoff") }
            precondition(!model.isScanning && model.root?.id == cancelledState.root &&
                         model.navigation.map(\.id) == cancelledState.navigation &&
                         model.isPartialMap == cancelledState.partial,
                         "A cancelled final handoff must not install its stale folder")
        }
        if action == "finish-selection" {
            for _ in 0..<500 {
                if !model.isScanning && !model.isPartialMap { break }
                try await Task.sleep(nanoseconds: 20_000_000)
            }
            precondition(!model.isScanning && !model.isPartialMap && model.current?.id == folder.path)
            print("selection change retried and handoff completed")
        }
        withExtendedLifetime(subscription) {}
    }
}
