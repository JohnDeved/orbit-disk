import json
from pathlib import Path
import subprocess
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
        self.assertTrue(all(t["description"] and t["inputSchema"]["additionalProperties"] is False for t in tools))
        remove = next(t for t in tools if t["name"] == "orbit_remove")
        self.assertTrue(remove["annotations"]["destructiveHint"])
        self.assertEqual(remove["inputSchema"]["properties"]["confirmed"]["enum"], [True])

    def test_lifecycle_and_notifications(self):
        result = rpc([request(0, "tools/list")] + handshake() + [request(2, "ping"), {"jsonrpc": "2.0", "method": "notifications/cancelled"}, request(3, "unknown")])
        self.assertEqual(len(result), 4)
        self.assertEqual(result[0]["error"]["code"], -32002)
        self.assertEqual(result[2]["result"], {})
        self.assertEqual(result[3]["error"]["code"], -32601)

    def test_versions(self):
        for version in ("2025-06-18", "2025-11-25"):
            self.assertEqual(rpc(handshake(version))[0]["result"]["protocolVersion"], version)
        self.assertEqual(rpc(handshake("future"))[0]["result"]["protocolVersion"], "2025-11-25")

    def test_malformed_messages_recover(self):
        result = rpc([b"not json", b"[]", request(5, "ping")])
        self.assertEqual(result[0]["error"]["code"], -32700)
        self.assertEqual(result[1]["error"]["code"], -32600)
        self.assertEqual(result[2]["id"], 5)

    def test_invalid_ids_and_params(self):
        results = rpc([request(1.5, "ping"), request(True, "ping"), request(2, "ping", [])])
        self.assertEqual([r["error"]["code"] for r in results], [-32600, -32600, -32602])

    def test_oversize_recovery(self):
        result = rpc([b"x" * 1_048_577, request(5, "ping")])
        self.assertEqual(len(result), 2)
        self.assertEqual(result[0]["error"]["code"], -32600)
        self.assertEqual(result[1]["result"], {})

    def test_invalid_arguments_never_connect(self):
        bad = [
            ("orbit_scan", {"path": 4}),
            ("orbit_list", {"limit": True}),
            ("orbit_list", {"limit": 1.5}),
            ("orbit_list", {"limit": 201}),
            ("orbit_list", {"extra": "ignored?"}),
            ("orbit_collect", {"context": "x", "items": []}),
            ("orbit_remove", {"token": "x", "confirmed": False}),
            ("orbit_remove", {"token": "x", "confirmed": 1}),
            ("orbit_prepare_removal", {"context": "x", "mode": "delete"}),
            ("orbit_scan", {"path": "nul\0path"}),
        ]
        messages = handshake() + [request(i + 2, "tools/call", {"name": name, "arguments": args}) for i, (name, args) in enumerate(bad)]
        results = rpc(messages)[1:]
        for result in results:
            self.assertTrue(result["result"]["isError"])
            self.assertEqual(result["result"]["structuredContent"]["error"]["code"], "invalid_arguments")

    def test_unknown_tools(self):
        result = rpc(handshake() + [request(2, "tools/call", {"name": "orbit_nothing"}), request(3, "tools/list", {"cursor": "bad"})])
        self.assertEqual(result[1]["error"]["code"], -32602)
        self.assertEqual(result[2]["error"]["code"], -32602)

    def test_cli_metadata_and_errors(self):
        tools = subprocess.run([str(BINARY), "tools"], check=True, capture_output=True, text=True)
        self.assertGreater(len(json.loads(tools.stdout)["tools"]), 10)
        config = subprocess.run([str(BINARY), "mcp-config"], check=True, capture_output=True, text=True)
        self.assertEqual(json.loads(config.stdout)["mcpServers"]["orbit-disk"]["args"], ["mcp"])
        invalid = subprocess.run([str(BINARY), "unknown"], capture_output=True, text=True)
        self.assertEqual(invalid.returncode, 1)
        self.assertEqual(json.loads(invalid.stdout)["error"]["code"], "unknown_command")


if __name__ == "__main__":
    unittest.main()
