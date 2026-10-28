import AppKit
import Foundation

enum MappingCacheReset {
    static let pendingKey = "clearMappingCacheOnLaunch"

    static func performIfRequested(preferences: UserDefaults, indexes: URL, mapping: URL) throws -> Bool {
        guard preferences.bool(forKey: pendingKey) else { return false }
        let directories = [indexes, mapping]
        for directory in directories {
            try validate(directory.deletingLastPathComponent())
            try validate(directory)
        }
        for directory in directories {
            do { try FileManager.default.removeItem(at: directory) }
            catch let error as CocoaError where error.code == .fileNoSuchFile { }
        }
        preferences.removeObject(forKey: "lastIndexedRoot")
        preferences.removeObject(forKey: pendingKey)
        return true
    }

    private static func validate(_ directory: URL) throws {
        var info = stat()
        guard lstat(directory.path, &info) == 0 else {
            if errno == ENOENT { return }
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        guard info.st_mode & S_IFMT == S_IFDIR, info.st_uid == geteuid() else {
            throw NSError(domain: "OrbitDisk.Cache", code: 1, userInfo: [
                NSLocalizedDescriptionKey: "The cache path is not an owned folder: \(directory.path)"
            ])
        }
    }

    @MainActor
    static func finishPendingReset() {
        let preferences = UserDefaults.standard
        let identifier = Bundle.main.bundleIdentifier ?? "local.orbitdisk.app"
        let manager = FileManager.default
        let indexes = manager.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(identifier).appendingPathComponent("Indexes", isDirectory: true)
        let mapping = manager.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(identifier).appendingPathComponent("Mapping", isDirectory: true)
        while preferences.bool(forKey: pendingKey) {
            do {
                guard NSRunningApplication.runningApplications(withBundleIdentifier: identifier)
                    .allSatisfy({ $0.processIdentifier == getpid() }) else {
                    throw NSError(domain: "OrbitDisk.Cache", code: 2, userInfo: [
                        NSLocalizedDescriptionKey: "Quit the other running copy of Orbit Disk, then try again."
                    ])
                }
                _ = try performIfRequested(preferences: preferences, indexes: indexes, mapping: mapping)
            } catch {
                // Do not construct a model that could reopen a partially cleared map.
                NSApplication.shared.setActivationPolicy(.regular)
                NSApplication.shared.activate(ignoringOtherApps: true)
                let alert = NSAlert()
                alert.messageText = "Couldn’t clear the cache"
                alert.informativeText = error.localizedDescription
                alert.addButton(withTitle: "Try Again")
                alert.addButton(withTitle: "Quit")
                if alert.runModal() != .alertFirstButtonReturn { exit(EXIT_FAILURE) }
            }
        }
    }

    @MainActor
    static func restart() throws {
        let preferences = UserDefaults.standard
        let helper = Process()
        helper.executableURL = URL(fileURLWithPath: "/bin/sh")
        // Only relaunch after every worker in this process has exited. Paths are positional arguments.
        helper.arguments = ["-c", "while kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; exec /usr/bin/open -n \"$2\"",
                            "orbit-cache-reset", String(getpid()), Bundle.main.bundleURL.path]
        helper.standardInput = FileHandle.nullDevice
        helper.standardOutput = FileHandle.nullDevice
        helper.standardError = FileHandle.nullDevice
        preferences.set(true, forKey: pendingKey)
        guard preferences.synchronize() else {
            preferences.removeObject(forKey: pendingKey)
            throw NSError(domain: "OrbitDisk.Cache", code: 3, userInfo: [
                NSLocalizedDescriptionKey: "The cache reset request could not be saved. Try again."
            ])
        }
        do { try helper.run() }
        catch {
            preferences.removeObject(forKey: pendingKey)
            throw error
        }
        NSApp.terminate(nil)
        // A refused termination must not leave a pending reset or relaunch behind.
        helper.terminate()
        preferences.removeObject(forKey: pendingKey)
    }
}
