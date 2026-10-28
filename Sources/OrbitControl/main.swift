import Foundation
import Darwin
import CoreFoundation
import OrbitAutomation

func emit(_ value: [String: Any]) {
    FileHandle.standardOutput.write(AutomationCatalog.encode(value) + Data([10]))
}
func rpcError(_ id: Any, _ code: Int, _ message: String) -> [String: Any] {
    ["jsonrpc": "2.0", "id": id, "error": ["code": code, "message": message]]
}
func call(_ command: String, _ arguments: [String: Any]) -> [String: Any] {
    do {
        try AutomationCatalog.validate(command: command, arguments: arguments)
        let request = AutomationCatalog.encode(["command": command, "arguments": arguments])
        let response: Data
        do { response = try LocalAutomationClient().request(request) }
        catch LocalAutomationError.unavailable {
            // Launch only after a failed connection, never replay a request whose outcome is unknown.
            let executable = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath()
            let bundled = executable.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            let app = bundled.pathExtension == "app" ? bundled : URL(fileURLWithPath: "/Applications/Orbit Disk.app")
            guard FileManager.default.fileExists(atPath: app.appendingPathComponent("Contents/MacOS/OrbitDisk").path) else {
                throw AutomationCatalog.Failure("app_not_installed", "Install Orbit Disk in Applications or open the app before calling tools.")
            }
            let deadline = Date().addingTimeInterval(10)
            let launch = Process(), terminated = DispatchSemaphore(value: 0)
            launch.executableURL = URL(fileURLWithPath: "/usr/bin/open")
            launch.arguments = ["-g", app.path]
            launch.standardInput = FileHandle.nullDevice
            launch.standardOutput = FileHandle.standardError
            launch.terminationHandler = { _ in terminated.signal() }
            try launch.run()
            if terminated.wait(timeout: .now() + 10) != .success {
                if launch.isRunning { launch.terminate() }
                if terminated.wait(timeout: .now() + 1) != .success, launch.isRunning { _ = kill(launch.processIdentifier, SIGKILL) }
                throw AutomationCatalog.Failure("launch_timeout", "Opening Orbit Disk timed out. Check the app before retrying.")
            }
            guard launch.terminationStatus == 0 else { throw AutomationCatalog.Failure("launch_failed", "Could not open Orbit Disk.") }
            var received: Data?
            repeat {
                do { received = try LocalAutomationClient().request(request) }
                catch LocalAutomationError.unavailable { Thread.sleep(forTimeInterval: 0.1) }
            } while received == nil && Date() < deadline
            guard let received else { throw AutomationCatalog.Failure("not_ready", "Orbit Disk did not become ready within 10 seconds. Open the app and retry.") }
            response = received
        }
        guard let value = try JSONSerialization.jsonObject(with: response) as? [String: Any] else { throw AutomationCatalog.Failure("invalid_response", "Orbit Disk returned an invalid response.") }
        return value
    } catch { return AutomationCatalog.failure(error) }
}

func runMCP() {
    var initialized = false, ready = false
    func process(_ line: Data) {
        let request: [String: Any]
        do {
            guard let parsed = try JSONSerialization.jsonObject(with: line) as? [String: Any] else { emit(rpcError(NSNull(), -32600, "Expected one JSON-RPC object.")); return }
            request = parsed
        } catch { emit(rpcError(NSNull(), -32700, "Invalid JSON.")); return }
        let id = request["id"] ?? NSNull()
        guard request["jsonrpc"] as? String == "2.0", let method = request["method"] as? String else { emit(rpcError(id, -32600, "Invalid JSON-RPC request.")); return }
        if request["id"] == nil {
            if method == "notifications/initialized", initialized { ready = true }
            return
        }
        guard id is String || ((id as? NSNumber).map { CFGetTypeID($0) != CFBooleanGetTypeID() && $0.doubleValue.isFinite && $0.doubleValue.rounded() == $0.doubleValue } ?? false) else { emit(rpcError(NSNull(), -32600, "Invalid request ID.")); return }
        if let params = request["params"], !(params is [String: Any]) { emit(rpcError(id, -32602, "Parameters must be an object.")); return }
        let params = request["params"] as? [String: Any] ?? [:]
        var result: [String: Any]
        switch method {
        case "initialize":
            guard !initialized, let version = params["protocolVersion"] as? String,
                  params["capabilities"] is [String: Any], params["clientInfo"] is [String: Any] else { emit(rpcError(id, -32602, "Invalid initialization.")); return }
            initialized = true
            let supported = ["2025-06-18", "2025-11-25"]
            result = ["protocolVersion": supported.contains(version) ? version : "2025-11-25",
                      "capabilities": ["tools": [:]],
                      "serverInfo": ["name": "orbit-disk", "title": "Orbit Disk storage and file cleanup", "version": "1.0.0"],
                      "instructions": AutomationCatalog.instructions]
        case "ping": result = [:]
        case "tools/list":
            guard ready else { emit(rpcError(id, -32002, "Initialize first.")); return }
            guard params["cursor"] == nil else { emit(rpcError(id, -32602, "Unknown tool cursor.")); return }
            result = ["tools": AutomationCatalog.commands]
        case "tools/call":
            guard ready else { emit(rpcError(id, -32002, "Initialize first.")); return }
            guard let name = params["name"] as? String, name.hasPrefix("orbit_"),
                  AutomationCatalog.commands.contains(where: { $0["name"] as? String == name }),
                  params["arguments"] == nil || params["arguments"] is [String: Any] else { emit(rpcError(id, -32602, "Unknown tool or invalid arguments.")); return }
            let response = call(String(name.dropFirst(6)), params["arguments"] as? [String: Any] ?? [:])
            let text = String(decoding: AutomationCatalog.encode(response), as: UTF8.self)
            result = ["content": [["type": "text", "text": text]], "structuredContent": response, "isError": response["ok"] as? Bool != true]
        default: emit(rpcError(id, -32601, "Unknown method.")); return
        }
        emit(["jsonrpc": "2.0", "id": id, "result": result])
    }
    var buffer = Data(), skipping = false
    var bytes = [UInt8](repeating: 0, count: 8192)
    while true {
        let count = Darwin.read(STDIN_FILENO, &bytes, bytes.count)
        if count == 0 { break }
        if count < 0 { if errno == EINTR { continue }; break }
        for byte in bytes.prefix(count) {
            if byte == 10 {
                if !skipping && !buffer.isEmpty { process(buffer) }
                buffer.removeAll(keepingCapacity: true); skipping = false
            } else if !skipping {
                buffer.append(byte)
                if buffer.count > 1_048_576 {
                    emit(rpcError(NSNull(), -32600, "Message exceeds 1 MiB."))
                    buffer.removeAll(keepingCapacity: true); skipping = true
                }
            }
        }
    }
    if !buffer.isEmpty && !skipping { process(buffer) }
}

let args = Array(CommandLine.arguments.dropFirst())
if args.first == "mcp" { runMCP() }
else if args.isEmpty || ["--help", "help", "-h"].contains(args[0]) {
    print("""
    Orbit Disk: inspect disk usage, find large files, and manage cleanup using the running app.
    Usage: orbitdisk COMMAND [JSON_ARGUMENTS]
           orbitdisk tools          Print command schemas
           orbitdisk mcp            Start the discoverable MCP server on stdio
           orbitdisk mcp-config     Print a standard MCP client configuration
           orbitdisk connect-codex  Register automatic tool discovery in Codex
           orbitdisk install-cli    Install the ~/.local/bin/orbitdisk command
    Examples:
      orbitdisk status
      orbitdisk scan '{"path":"/Users/you/Downloads"}'
      orbitdisk list '{"limit":20}'
    Commands return JSON. Scan, search and collection validation are asynchronous.
    Use status to check progress. Removal requires prepare_removal followed by user-authorized remove.
    """)
} else if args[0] == "tools" { emit(["tools": AutomationCatalog.commands, "instructions": AutomationCatalog.instructions]) }
else if ["connect-codex", "install-cli"].contains(args[0]) {
    do {
        let installer = IntegrationInstaller(controlURL: URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath())
        let message = try args[0] == "connect-codex" ? installer.connectCodex() : installer.installCommandLineTool()
        emit(["ok": true, "message": message])
    } catch { emit(AutomationCatalog.failure(error)); exit(1) }
} else if args[0] == "mcp-config" {
    let executable = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath().path
    emit(["mcpServers": ["orbit-disk": ["command": executable, "args": ["mcp"]]]])
} else {
    do {
        guard args.count <= 2 else { throw AutomationCatalog.Failure("invalid_arguments", "Expected one command and optional JSON object. Use --help.") }
        let arguments: [String: Any]
        if args.count == 2 {
            guard let parsed = try JSONSerialization.jsonObject(with: Data(args[1].utf8)) as? [String: Any] else { throw AutomationCatalog.Failure("invalid_arguments", "Arguments must be a JSON object.") }
            arguments = parsed
        } else { arguments = [:] }
        let response = call(args[0], arguments)
        emit(response)
        if response["ok"] as? Bool != true { exit(1) }
    } catch { emit(AutomationCatalog.failure(error)); exit(2) }
}
