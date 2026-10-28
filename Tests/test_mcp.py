import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import sys
import unittest

BINARY = Path(sys.argv.pop(1)).resolve()


def request(ident, method, params=None):
    value = {"jsonrpc": "2.0", "id": ident, "method": method}
    if params is not None:
        value["params"] = params
    return value


def handshake(version="2025-11-25"):
    return [request(1, "initialize", {"protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
            {"jsonrpc": "2.0", "method": "notifications/initialized"}]


def rpc(messages):
    data = b"".join((m if isinstance(m, bytes) else json.dumps(m).encode()) + b"\n" for m in messages)
    result = subprocess.run([str(BINARY), "mcp"], input=data, capture_output=True, timeout=10, check=True)
    return [json.loads(line) for line in result.stdout.splitlines()]


class MCPChecks(unittest.TestCase):
    def test_discovery_without_app_connection(self):
        result = rpc(handshake() + [request(2, "tools/list")])
        self.assertEqual(result[0]["result"]["protocolVersion"], "2025-11-25")
        tools = result[1]["result"]["tools"]
        names = {t["name"] for t in tools}
        self.assertEqual(len(names), len(tools))
        self.assertTrue({"orbit_status", "orbit_scan", "orbit_list", "orbit_collect", "orbit_prepare_removal", "orbit_remove"} <= names)
        self.assertTrue(all(t["title"] and t["description"] and t["inputSchema"]["additionalProperties"] is False for t in tools))
        self.assertTrue(all(t["outputSchema"]["type"] == "object" and t["outputSchema"]["required"] == ["ok"] for t in tools))
        self.assertIn("without waiting for scanning to finish", result[0]["result"]["instructions"])
        self.assertIn("never automatically retry a mutation", result[0]["result"]["instructions"])
        remove = next(t for t in tools if t["name"] == "orbit_remove")
        self.assertTrue(remove["annotations"]["destructiveHint"])
        self.assertEqual(remove["inputSchema"]["properties"]["confirmed"]["enum"], [True])

    def test_lifecycle_and_notifications(self):
        result = rpc([request(0, "tools/list")] + handshake() + [request(2, "ping"), {"jsonrpc": "2.0", "method": "notifications/cancelled"}, request(3, "unknown")])
        self.assertEqual(len(result), 4)
        self.assertEqual(result[0]["error"]["code"], -32002)
        self.assertEqual(result[2]["result"], {})
        self.assertEqual(result[3]["error"]["code"], -32601)

    def test_initialization_validation_and_recovery(self):
        bad = request(0, "initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {}})
        initialized = {"jsonrpc": "2.0", "method": "notifications/initialized"}
        malformed = {**initialized, "params": []}
        result = rpc([bad, handshake()[0], malformed, request(2, "tools/list"), initialized, request(3, "tools/list")])
        self.assertEqual(result[0]["error"]["code"], -32602)
        self.assertIn("serverInfo", result[1]["result"])
        self.assertEqual(result[2]["error"]["code"], -32002)
        self.assertIn("tools", result[3]["result"])

    def test_versions(self):
        for version in ("2025-06-18", "2025-11-25"):
            self.assertEqual(rpc(handshake(version))[0]["result"]["protocolVersion"], version)
        self.assertEqual(rpc(handshake("future"))[0]["result"]["protocolVersion"], "2025-11-25")

    def test_malformed_messages_recover(self):
        result = rpc([b"not json", b"[]", b"null", b"42", b'"text"', request(5, "ping")])
        self.assertEqual(result[0]["error"]["code"], -32700)
        self.assertTrue(all(r["error"]["code"] == -32600 for r in result[1:5]))
        self.assertEqual(result[5]["id"], 5)

    def test_invalid_ids_and_params(self):
        results = rpc([request(1.5, "ping"), request(True, "ping"), request(2, "ping", [])])
        self.assertEqual([r["error"]["code"] for r in results], [-32600, -32600, -32602])
        malformed = rpc([{"jsonrpc": "2.0", "id": {"nested": "id"}}, {"jsonrpc": "wrong", "id": True, "method": "ping"}])
        self.assertTrue(all(r["id"] is None and r["error"]["code"] == -32600 for r in malformed))

    def test_oversize_recovery(self):
        result = rpc([b"x" * 1_048_577, request(5, "ping")])
        self.assertEqual(len(result), 2)
        self.assertEqual(result[0]["error"]["code"], -32600)
        self.assertEqual(result[1]["result"], {})

    def test_invalid_arguments_never_connect(self):
        bad = [
            ("orbit_scan", {"path": 4}),
            ("orbit_scan", {"path": ""}),
            ("orbit_search", {"context": "", "query": "large"}),
            ("orbit_search", {"context": "x", "query": "é" * 4097}),
            ("orbit_list", {"limit": True}),
            ("orbit_list", {"limit": 1.5}),
            ("orbit_list", {"limit": 201}),
            ("orbit_list", {"extra": "ignored?"}),
            ("orbit_collect", {"context": "x", "items": []}),
            ("orbit_collect", {"context": "x", "items": [{"path": "/tmp/file", "identity": ""}]}),
            ("orbit_remove", {"token": "x", "confirmed": False}),
            ("orbit_remove", {"token": "x", "confirmed": 1}),
            ("orbit_prepare_removal", {"context": "x", "mode": "delete"}),
            ("orbit_scan", {"path": "nul\0path"}),
        ]
        messages = handshake() + [request(i + 2, "tools/call", {"name": name, "arguments": args}) for i, (name, args) in enumerate(bad)]
        results = rpc(messages)[1:]
        for result in results:
            self.assertTrue(result["result"]["isError"])
            structured = result["result"]["structuredContent"]
            self.assertEqual(structured["error"]["code"], "invalid_arguments")
            self.assertFalse(structured["ok"])
            self.assertEqual(json.loads(result["result"]["content"][0]["text"]), structured)

    def test_unknown_tools(self):
        result = rpc(handshake() + [request(2, "tools/call", {"name": "orbit_nothing"}), request(3, "tools/list", {"cursor": "bad"})])
        self.assertEqual(result[1]["error"]["code"], -32602)
        self.assertEqual(result[2]["error"]["code"], -32602)

    def test_unsupported_task_mode_is_rejected_without_connecting(self):
        result = rpc(handshake() + [request(2, "tools/call", {"name": "orbit_status", "task": {"ttl": 60000}})])
        self.assertEqual(result[1]["error"]["code"], -32602)
        self.assertIn("unsupported", result[1]["error"]["message"])

    def test_cli_rejects_bad_json_and_extra_setup_arguments(self):
        for command in ("mcp", "tools", "mcp-config", "connect-codex", "install-cli", "--version", "--help"):
            result = subprocess.run([str(BINARY), command, "unexpected"], capture_output=True, text=True, timeout=3)
            self.assertEqual(result.returncode, 2, command)
            self.assertEqual(json.loads(result.stdout)["error"]["code"], "invalid_arguments")
        for arguments in ("{broken", "[]", "null"):
            result = subprocess.run([str(BINARY), "scan", arguments], capture_output=True, text=True, timeout=3)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(json.loads(result.stdout)["error"]["code"], "invalid_arguments")

    def test_bundled_version_is_reported_without_launching_app(self):
        with tempfile.TemporaryDirectory(prefix="OrbitMCPMetadata-") as fixture:
            contents = Path(fixture) / "Orbit Disk.app" / "Contents"
            executable = contents / "MacOS" / "OrbitControl"
            executable.parent.mkdir(parents=True)
            shutil.copy2(BINARY, executable)
            (contents / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": "test.orbit.metadata", "CFBundleShortVersionString": "9.8.7-test"}))
            result = subprocess.run([str(executable), "--version"], check=True, capture_output=True, text=True, timeout=3)
            self.assertEqual(json.loads(result.stdout)["serverInfo"]["version"], "9.8.7-test")
            result = subprocess.run([str(executable), "mcp"], input=json.dumps(handshake()[0]) + "\n", check=True, capture_output=True, text=True, timeout=3)
            self.assertEqual(json.loads(result.stdout)["result"]["serverInfo"]["version"], "9.8.7-test")

    def test_cli_metadata_and_errors(self):
        version = subprocess.run([str(BINARY), "--version"], check=True, capture_output=True, text=True)
        metadata = json.loads(version.stdout)
        self.assertTrue(metadata["serverInfo"]["version"])
        self.assertEqual(metadata["protocolVersions"], ["2025-06-18", "2025-11-25"])
        tools = subprocess.run([str(BINARY), "tools"], check=True, capture_output=True, text=True)
        self.assertGreater(len(json.loads(tools.stdout)["tools"]), 10)
        config = subprocess.run([str(BINARY), "mcp-config"], check=True, capture_output=True, text=True)
        self.assertEqual(json.loads(config.stdout)["mcpServers"]["orbit-disk"]["args"], ["mcp"])
        invalid = subprocess.run([str(BINARY), "unknown"], capture_output=True, text=True)
        self.assertEqual(invalid.returncode, 1)
        self.assertEqual(json.loads(invalid.stdout)["error"]["code"], "unknown_command")


if __name__ == "__main__":
    unittest.main()
