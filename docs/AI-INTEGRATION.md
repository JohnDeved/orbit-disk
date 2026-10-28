# AI integration

Available in Orbit Disk 0.9.11 and later. Orbit Disk exposes its running app through MCP and a JSON command-line client. Both use the same live inventory, collection, navigation and removal checks as the interface. They can use discovered files before mapping finishes.

## Automatic tool discovery

Choose **Orbit Disk → AI Integration… → Connect to Codex** once. Orbit Disk registers its bundled MCP server in Codex's configuration. New Codex sessions discover the tools and their descriptions automatically; you do not need to mention Orbit Disk or explain its commands in each prompt. Existing sessions may need to reload their MCP connections.

For other MCP clients, choose **Copy MCP Settings** and add those settings to the client. MCP clients require this initial connection; installing an application alone does not grant it access to every AI client.

The equivalent Codex command is:

```sh
codex mcp add orbit-disk -- '/Applications/Orbit Disk.app/Contents/MacOS/OrbitControl' mcp
```

The server responds to initialization and tool discovery without launching the app. A tool call opens Orbit Disk if it is not already running. No API key or AI model is bundled into Orbit Disk.

## Tools

| Task | Tools |
| --- | --- |
| Inspect mapping and progress | `orbit_status`, `orbit_list` |
| Open, resume or refresh maps | `orbit_scan`, `orbit_open`, `orbit_back`, `orbit_refresh`, `orbit_stop` |
| Find and inspect items | `orbit_search`, `orbit_select`, `orbit_reveal`, `orbit_preview` |
| Build a collection | `orbit_collect`, `orbit_collection`, `orbit_uncollect`, `orbit_clear_collection` |
| Remove reviewed items | `orbit_prepare_removal`, `orbit_remove` |

Tool descriptions include input and output schemas, asynchronous behavior and mutation hints. The server also provides workflow instructions during MCP initialization so an agent can choose tools without a separate prompt or skill installation.

Scanning, search, collection validation and removal return an accepted response when work starts. Agents must poll status and collection to observe completion and errors. Once `opening` is false, listed files can be used while `scanning` remains true. Directory sizes may still be incomplete, and every item includes a completeness flag. Search uses the app's current indexed-name search and reports when its result limit truncates matches. A failed search returns `search_failed`; submitting the same query again retries it.

Cloud storage is excluded from mapping and collection by default: the standard macOS `CloudStorage` and iCloud Drive locations, plus Desktop and Documents when macOS identifies them as iCloud-backed. These exclusions apply to the native app and AI tools. Directory discovery disables automatic downloads of cloud-only content. Custom third-party sync locations outside the standard roots may not be identifiable; arbitrary folder names are not treated as proof of cloud storage. Older cached inventories are rebuilt once to apply the exclusions.

`list` and `collection` return at most 200 items per page. A cursor refers to a fixed snapshot for 30 seconds. A new listing replaces the prior snapshot; changing the folder or query invalidates its cursor. Omit the cursor to get current results. Action requests carry the map context and exact item identity from the response; outdated references are rejected.

## Removal

`prepare_removal` returns the complete proposed set of up to 200 collected items, a mode (`trash` or `permanent`), and a five-minute token. It opens the app's collection review but removes nothing. The agent must present that plan and obtain user authorization before calling `remove` with the token and `confirmed: true`.

A token is valid once and is rejected after the collection, map or review changes. After an accepted removal, `status.lastRemoval` reports that operation's completion and errors even if the user navigates to another map. The existing removal pipeline rechecks file identities, parent folders and protected paths, and records the removal durably before changing files. Permanent deletion cannot be undone.

## Command line

Choose **Install Command Line Tool** in AI Integration to create `~/.local/bin/orbitdisk`. The installer never replaces an unrelated file. If that directory is not on your PATH, use the full path or add it to your shell configuration.

```sh
orbitdisk --help
orbitdisk --version
orbitdisk tools
orbitdisk status
orbitdisk scan '{"path":"/Users/you/Downloads"}'
orbitdisk list '{"limit":20}'
```

Commands take one optional JSON object and return JSON. `orbitdisk mcp` uses newline-delimited JSON-RPC on standard input/output. Diagnostics never mix with MCP output.

## Responses and interrupted requests

Successful commands return `{"ok":true,"result":{...}}`; failures return `{"ok":false,"error":{"code":"...","message":"..."}}`. MCP exposes the same envelope as structured content and text. The command-line client exits with a nonzero status on failure.

A transport timeout or lost connection can occur after the app accepts a command. In that case, `error.outcomeUnknown` is true. Read status and collection before deciding what to do; do not automatically repeat a mutation. If the app restarted and the removal receipt is unavailable, inspect the current state and obtain a new review before attempting another removal.

## Local access and privacy

The app listens on a Unix socket in a private per-user temporary directory, with owner-only permissions and peer-user verification. There is no TCP listener or remote endpoint. Only processes running as the same macOS user can connect. Frames, concurrent clients and timeouts are bounded.

The interface returns file metadata, not file contents. Finder and Quick Look actions use the same previews as the app. Connected AI clients receive the returned metadata and may send it to their model provider according to their own settings. macOS filesystem permissions still apply.

To disconnect Codex, run `codex mcp remove orbit-disk`. Remove the `~/.local/bin/orbitdisk` symlink to uninstall the command-line entry point. Quit Orbit Disk to close its local automation socket.

The protocol follows the [MCP stdio transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports) and [tool discovery](https://modelcontextprotocol.io/specification/2025-11-25/server/tools). Codex registration uses its [documented MCP configuration](https://developers.openai.com/codex/mcp).
