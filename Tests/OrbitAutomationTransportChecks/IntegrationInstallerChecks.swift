import Foundation
import Darwin
import OrbitAutomation

func checkIntegrationInstaller() throws {
    func require(_ condition: @autoclosure () -> Bool, _ message: String) throws {
        if !condition() { throw NSError(domain: "IntegrationInstallerChecks", code: 1, userInfo: [NSLocalizedDescriptionKey: message]) }
    }
    let directory = URL(fileURLWithPath: "/tmp/oi-\(UUID().uuidString.prefix(8))", isDirectory: true)
    let bin = directory.appendingPathComponent("bin"), home = directory.appendingPathComponent("home")
    try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: directory) }
    let control = directory.appendingPathComponent("My App.app/Contents/MacOS/OrbitControl")
    try FileManager.default.createDirectory(at: control.deletingLastPathComponent(), withIntermediateDirectories: true)
    try Data("#!/bin/sh\nexit 0\n".utf8).write(to: control)
    chmod(control.path, 0o700)
    let codex = bin.appendingPathComponent("codex")
    let source = #"""
    #!/usr/bin/python3
    import json, os, pathlib, sys
    root = pathlib.Path(os.environ['FAKE_CODEX_ROOT'])
    args = sys.argv[1:]
    with (root / 'calls').open('a') as calls:
        calls.write(json.dumps(args) + '\n')
    mode = os.environ.get('FAKE_CODEX_MODE', '')
    if mode == 'empty': sys.exit(0)
    if mode == 'invalid': print('not json'); sys.exit(0)
    if mode == 'large': print('x' * 300000); sys.exit(0)
    state = root / 'servers.json'
    servers = json.loads(state.read_text())
    if args == ['mcp', 'list', '--json']:
        print(json.dumps(servers))
    elif args[:4] == ['mcp', 'add', 'orbit-disk', '--']:
        assert len(args) == 6 and args[5] == 'mcp'
        assert not any(s['name'] == 'orbit-disk' for s in servers)
        servers.append({'name': 'orbit-disk', 'enabled': True, 'transport': {'type': 'stdio', 'command': args[4], 'args': ['mcp']}})
        state.write_text(json.dumps(servers))
        print('Added')
    else:
        sys.exit(2)
    """#
    try Data(source.utf8).write(to: codex); chmod(codex.path, 0o700)
    let state = directory.appendingPathComponent("servers.json")
    let initial = "[{\"name\":\"another-server\",\"transport\":{\"type\":\"stdio\",\"command\":\"keep\",\"args\":[]},\"enabled\":true}]"
    try Data(initial.utf8).write(to: state)
    let environment = ["PATH": bin.path + ":/usr/bin:/bin", "FAKE_CODEX_ROOT": directory.path]
    let installer = IntegrationInstaller(controlURL: control, home: home, environment: environment)
    let settings = try installer.mcpSettings()
    let parsed = try JSONSerialization.jsonObject(with: Data(settings.utf8)) as! [String: Any]
    let server = (parsed["mcpServers"] as! [String: [String: Any]])["orbit-disk"]!
    try require(server["command"] as? String == control.path && server["args"] as? [String] == ["mcp"], "Copied MCP settings preserve spaced paths")
    let installResult = try installer.installCommandLineTool()
    try require(installResult.contains("PATH"), "PATH installation guidance")
    let link = home.appendingPathComponent(".local/bin/orbitdisk")
    try require(link.resolvingSymlinksInPath() == control, "CLI symlink points to bundled control")
    _ = try installer.installCommandLineTool()
    try FileManager.default.removeItem(at: link)
    try Data("keep".utf8).write(to: link)
    do { _ = try installer.installCommandLineTool(); throw NSError(domain: "Expected link conflict", code: 1) }
    catch IntegrationInstallError.conflictingLink {}
    try require((try? Data(contentsOf: link)) == Data("keep".utf8), "Existing CLI file is untouched")
    print("PASS MCP settings, CLI install/idempotency/conflict, PATH guidance")

    _ = try installer.connectCodex()
    _ = try installer.connectCodex()
    let calls = try String(contentsOf: directory.appendingPathComponent("calls"), encoding: .utf8)
    try require(calls.components(separatedBy: "\"add\"").count == 2, "Repeated connection adds exactly once")
    var servers = try JSONSerialization.jsonObject(with: Data(contentsOf: state)) as! [[String: Any]]
    try require(servers.count == 2 && servers[0]["name"] as? String == "another-server", "Other MCP server remains unchanged")
    var transport = servers[1]["transport"] as! [String: Any]
    let alias = directory.appendingPathComponent("control-alias")
    try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: control)
    transport["command"] = alias.path; servers[1]["transport"] = transport
    try JSONSerialization.data(withJSONObject: servers).write(to: state)
    _ = try installer.connectCodex()
    transport["command"] = "/different/app/OrbitControl"; servers[1]["transport"] = transport
    try JSONSerialization.data(withJSONObject: servers).write(to: state)
    let beforeConflict = try Data(contentsOf: state)
    do { _ = try installer.connectCodex(); throw NSError(domain: "Expected MCP conflict", code: 1) }
    catch IntegrationInstallError.conflictingServer {}
    try require((try? Data(contentsOf: state)) == beforeConflict, "Conflicting MCP configuration unchanged")
    print("PASS Codex add, idempotency, equivalent symlink, conflict preservation")

    for mode in ["empty", "invalid", "large"] {
        var configured = environment; configured["FAKE_CODEX_MODE"] = mode
        let broken = IntegrationInstaller(controlURL: control, home: home, environment: configured)
        do { _ = try broken.connectCodex(); throw NSError(domain: "Expected unreadable config failure", code: 1) }
        catch IntegrationInstallError.invalidConfiguration {}
        catch IntegrationInstallError.commandFailed {}
        try require((try? Data(contentsOf: state)) == beforeConflict, "Unreadable config never triggers an add")
    }
    print("PASS empty/invalid/oversized Codex output fails without config changes")
}
