import Foundation
import Darwin

public enum IntegrationInstallError: Error, LocalizedError {
    case missingControl
    case codexNotFound
    case commandFailed(String)
    case invalidConfiguration
    case conflictingServer
    case conflictingLink(String)
    case timeout

    public var errorDescription: String? {
        switch self {
        case .missingControl: return "OrbitControl is missing from this app. Reinstall Orbit Disk and try again."
        case .codexNotFound: return "The Codex command line tool wasn’t found. Install it, or use Copy MCP Settings to connect another client."
        case .commandFailed(let message): return message
        case .invalidConfiguration: return "Codex returned an unreadable server configuration. No settings were changed."
        case .conflictingServer: return "Codex already has a different or disabled server named orbit-disk. Review that server in Codex settings before connecting."
        case .conflictingLink(let path): return "\(path) already exists and points somewhere else. No files were replaced."
        case .timeout: return "Codex setup timed out. Check its MCP settings before trying again."
        }
    }
}

public struct IntegrationInstaller: Sendable {
    public let controlURL: URL
    private let home: URL
    private let environment: [String: String]

    public init(controlURL: URL, home: URL = FileManager.default.homeDirectoryForCurrentUser,
                environment: [String: String] = ProcessInfo.processInfo.environment) {
        self.controlURL = controlURL
        self.home = home
        self.environment = environment
    }

    public func mcpSettings() throws -> String {
        try validateControl()
        let object: [String: Any] = ["mcpServers": ["orbit-disk": ["command": controlURL.path, "args": ["mcp"]]]]
        let data = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
        return String(decoding: data, as: UTF8.self)
    }

    public func installCommandLineTool() throws -> String {
        try validateControl()
        let directory = home.appendingPathComponent(".local/bin", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let link = directory.appendingPathComponent("orbitdisk")
        var info = stat()
        if lstat(link.path, &info) == 0 {
            guard info.st_mode & S_IFMT == S_IFLNK, sameExecutable(link, controlURL) else {
                throw IntegrationInstallError.conflictingLink(link.path)
            }
        } else {
            guard errno == ENOENT else { throw IntegrationInstallError.conflictingLink(link.path) }
            do { try FileManager.default.createSymbolicLink(at: link, withDestinationURL: controlURL) }
            catch { throw IntegrationInstallError.conflictingLink(link.path) }
        }
        let onPath = (environment["PATH"] ?? "").split(separator: ":").contains {
            URL(fileURLWithPath: String($0)).standardizedFileURL.resolvingSymlinksInPath()
                == directory.standardizedFileURL.resolvingSymlinksInPath()
        }
        return onPath ? "Installed \(link.path). Run orbitdisk --help in Terminal."
            : "Installed \(link.path). Add \(directory.path) to your shell’s PATH to run orbitdisk from any folder. Shell settings were not changed."
    }

    public func connectCodex() throws -> String {
        try validateControl()
        guard let codex = findCodex() else { throw IntegrationInstallError.codexNotFound }
        var childEnvironment = environment
        childEnvironment["PATH"] = codex.deletingLastPathComponent().path + ":"
            + (environment["PATH"] ?? "/usr/bin:/bin:/usr/sbin:/sbin")
        func configuration() throws -> [[String: Any]] {
            let result = try Self.run(codex, ["mcp", "list", "--json"], environment: childEnvironment)
            guard result.status == 0 else { throw IntegrationInstallError.commandFailed("Codex couldn’t read its MCP settings. \(result.errorSummary)") }
            guard let list = try? JSONSerialization.jsonObject(with: result.output) as? [[String: Any]],
                  list.allSatisfy({ $0["name"] is String && $0["transport"] is [String: Any] }) else {
                throw IntegrationInstallError.invalidConfiguration
            }
            return list
        }
        let servers = try configuration()
        if let existing = servers.first(where: { $0["name"] as? String == "orbit-disk" }) {
            guard matches(existing) else { throw IntegrationInstallError.conflictingServer }
            return "Orbit Disk is already connected to Codex. Its tools are available in new Codex conversations."
        }
        let result = try Self.run(codex, ["mcp", "add", "orbit-disk", "--", controlURL.path, "mcp"], environment: childEnvironment)
        guard result.status == 0 else {
            throw IntegrationInstallError.commandFailed("Codex couldn’t add Orbit Disk. \(result.errorSummary)")
        }
        let updated = try configuration()
        guard let entry = updated.first(where: { $0["name"] as? String == "orbit-disk" }), matches(entry) else {
            throw IntegrationInstallError.commandFailed("Codex setup finished, but the Orbit Disk connection could not be verified. Check its MCP settings.")
        }
        return "Connected to Codex. Start a new Codex conversation to use Orbit Disk’s tools. You don’t need to mention this integration by name."
    }

    private func matches(_ server: [String: Any]) -> Bool {
        guard server["enabled"] as? Bool == true,
              let transport = server["transport"] as? [String: Any], transport["type"] as? String == "stdio",
              let command = transport["command"] as? String, command.hasPrefix("/"),
              transport["args"] as? [String] == ["mcp"] else { return false }
        return sameExecutable(URL(fileURLWithPath: command), controlURL)
    }

    private func sameExecutable(_ lhs: URL, _ rhs: URL) -> Bool {
        lhs.standardizedFileURL.resolvingSymlinksInPath().path.utf8.elementsEqual(
            rhs.standardizedFileURL.resolvingSymlinksInPath().path.utf8)
    }

    private func validateControl() throws {
        guard controlURL.isFileURL, FileManager.default.isExecutableFile(atPath: controlURL.path) else {
            throw IntegrationInstallError.missingControl
        }
    }

    private func findCodex() -> URL? {
        var candidates = (environment["PATH"] ?? "").split(separator: ":").filter { $0.hasPrefix("/") }
            .map { URL(fileURLWithPath: String($0), isDirectory: true).appendingPathComponent("codex") }
        candidates += [home.appendingPathComponent(".local/bin/codex"), home.appendingPathComponent(".npm-global/bin/codex"),
                       URL(fileURLWithPath: "/opt/homebrew/bin/codex"), URL(fileURLWithPath: "/usr/local/bin/codex")]
        let nvm = home.appendingPathComponent(".nvm/versions/node", isDirectory: true)
        if let versions = try? FileManager.default.contentsOfDirectory(at: nvm, includingPropertiesForKeys: nil) {
            candidates += versions.sorted { $0.lastPathComponent.localizedStandardCompare($1.lastPathComponent) == .orderedDescending }
                .map { $0.appendingPathComponent("bin/codex") }
        }
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    private struct Output {
        let status: Int32
        let output: Data
        let errors: Data
        var errorSummary: String {
            String(decoding: errors.prefix(500), as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        }
    }

    private final class Capture: @unchecked Sendable {
        let ended = DispatchSemaphore(value: 0)
        private let lock = NSLock()
        private var data = Data()
        private var exceeded = false
        func append(_ value: Data) -> Bool {
            lock.lock(); defer { lock.unlock() }
            guard data.count + value.count <= 131_072 else { exceeded = true; return false }
            data.append(value); return true
        }
        func result() -> (Data, Bool) { lock.lock(); defer { lock.unlock() }; return (data, exceeded) }
    }

    private static func run(_ executable: URL, _ arguments: [String], environment: [String: String]) throws -> Output {
        let process = Process(), output = Pipe(), errors = Pipe()
        let capturedOutput = Capture(), capturedErrors = Capture(), terminated = DispatchSemaphore(value: 0)
        process.executableURL = executable; process.arguments = arguments; process.environment = environment
        process.standardInput = FileHandle.nullDevice; process.standardOutput = output; process.standardError = errors
        process.terminationHandler = { _ in terminated.signal() }
        for (pipe, capture) in [(output, capturedOutput), (errors, capturedErrors)] {
            pipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                if data.isEmpty { handle.readabilityHandler = nil; capture.ended.signal() }
                else if !capture.append(data) { if process.isRunning { process.terminate() } }
            }
        }
        defer {
            output.fileHandleForReading.readabilityHandler = nil
            errors.fileHandleForReading.readabilityHandler = nil
            try? output.fileHandleForReading.close(); try? errors.fileHandleForReading.close()
        }
        do { try process.run() } catch {
            throw IntegrationInstallError.commandFailed("Codex couldn’t start: \(error.localizedDescription)")
        }
        try? output.fileHandleForWriting.close(); try? errors.fileHandleForWriting.close()
        if terminated.wait(timeout: .now() + 10) != .success {
            if process.isRunning { process.terminate() }
            if terminated.wait(timeout: .now() + 1) != .success {
                if process.isRunning { _ = kill(process.processIdentifier, SIGKILL) }
                _ = terminated.wait(timeout: .now() + 1)
            }
            throw IntegrationInstallError.timeout
        }
        let outputEnded = capturedOutput.ended.wait(timeout: .now() + 1) == .success
        let errorsEnded = capturedErrors.ended.wait(timeout: .now() + 1) == .success
        let (data, oversizedOutput) = capturedOutput.result(), (errorData, oversizedErrors) = capturedErrors.result()
        guard outputEnded, errorsEnded, !oversizedOutput, !oversizedErrors else {
            throw IntegrationInstallError.commandFailed("Codex returned too much output or left its response open. No further setup was attempted.")
        }
        return Output(status: process.terminationStatus, output: data, errors: errorData)
    }
}
