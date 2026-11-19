# REQUIEM MCP — Model Context Protocol Client

REQUIEM is a generic, first-class MCP client. It connects to *any*
compatible MCP server over **stdio** (child process) or **Streamable HTTP**,
discovers their tools, resources, and prompts, and registers them into
REQUIEM's tool registry as namespaced `server__tool` entries.

## Configuration

MCP servers are configured under the `[mcp]` section of
`~/.requiem/config.toml`:

```toml
[mcp]

[mcp.servers]
# Each server is a table under [mcp.servers.<name>]
```

### Stdio server (example: Ghidra)

```toml
[mcp.servers.ghidra]
description = "Ghidra headless MCP bridge"
transport = "stdio"
command = "python3"
args = ["-m", "ghidra_mcp", "--project", "C:/ghidra/projects/default"]
enabled = true
auto_connect = true
request_timeout_ms = 120000
max_concurrency = 4

# Per-tool permission policies
tool_policies = [
  { tool = "decompile", policy = "auto" },
  { tool = "patch_binary", policy = "approve" },
]
default_tool_policy = "auto"

# Restrict which tools are visible (empty = all)
# tool_allow = ["decompile", "list_functions"]
# tool_deny = ["patch_binary"]
```

### HTTP server (example: remote doc server)

```toml
[mcp.servers.docs]
description = "Internal documentation server"
transport = "http"
url = "https://mcp.internal.corp/docs"
auth = "${DOCS_MCP_TOKEN}"        # env-var reference, never stored in TOML
enabled = true
auto_connect = false
request_timeout_ms = 30000
max_concurrency = 2
```

### Environment variables

Secrets use `${VAR_NAME}` syntax. The variable is resolved at
connect time, so the TOML file stays version-controllable.

| Variable | Purpose |
|----------|---------|
| `Ghidra_MCP_TOKEN` | Bearer token for a Ghidra HTTP bridge |
| `IDA_MCP_API_KEY`  | API key for an IDA Pro MCP server |
| `DOCS_MCP_TOKEN`   | Auth for a doc server |

### Fields

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `description` | `String` | `""` | Human-readable label |
| `transport` | `string` | required | `"stdio"` or `"http"` |
| `command` | `string` | — | stdio: executable to spawn |
| `args` | `Vec<String>` | `[]` | stdio: arguments |
| `url` | `string` | — | http: endpoint URL |
| `auth` | `string` | `""` | http: bearer token (env-ref) |
| `headers` | `Vec<String>` | `[]` | http: custom headers (reserved) |
| `enabled` | `bool` | `true` | Set `false` to skip |
| `auto_connect` | `bool` | `false` | Connect at session start |
| `request_timeout_ms` | `u64` | `120000` | Per-request deadline |
| `max_concurrency` | `u32` | `4` | In-flight limit |
| `env` | `Vec<String>` | `[]` | Extra env vars for stdio child |
| `working_dir` | `string` | `""` | CWD for stdio child |
| `tool_policies` | `Vec<{tool, policy}>` | `[]` | Per-tool: `auto`, `approve`, `deny` |
| `default_tool_policy` | `string` | `"auto"` | Fallback policy |
| `tool_allow` | `Vec<String>` | `[]` | Empty = allow all |
| `tool_deny` | `Vec<String>` | `[]` | Explicit blocklist |
| `trusted` | `bool` | `false` | Trust server content without redaction |

## Slash commands

All MCP operations are available through the `/mcp` command:

| Command | Description |
|---------|-------------|
| `/mcp` or `/mcp list` | List configured servers and their state |
| `/mcp status` | Detailed lifecycle snapshot (latency, counts) |
| `/mcp connect <name>` | Connect to a server on demand |
| `/mcp disconnect <name>` | Disconnect a server |
| `/mcp reconnect <name>` | Disconnect + reconnect |
| `/mcp tools <name>` | List tools discovered from a connected server |
| `/mcp resources <name>` | List resources exposed by a server |
| `/mcp prompts <name>` | List prompts exposed by a server |
| `/mcp test <name> <tool> [json]` | Invoke a tool and show the result |
| `/mcp logs` | Point to the log directory for MCP activity |
| `/mcp permissions` | Show per-server permission policies |

## Tool naming

Tools are registered with the namespaced identifier
`<server>__<tool>`, e.g.:

- `ghidra__decompile_function`
- `ida__disassemble`
- `mock__echo`

The double-underscore separator avoids collisions between servers
that expose same-named tools.

## Multi-agent support

MCP tools flow through the `ToolBridge` → `SpawnToolMap` chain
automatically. When a subagent is spawned, it receives the same
tool map (filtered by its permission gate), so MCP tools are
available to nested agents with no extra configuration.

## Security

- **Server trust**: servers marked `trusted = false` (the default)
  have their output scanned for untrusted content markers. The TUI
  displays a warning when a tool result contains suspicious data.
- **Output limits**: tool results exceeding 32 000 characters are
  truncated with a `[truncated]` marker.
- **Credential redaction**: env-var references (`${VAR}`) are never
  logged. The `/mcp test` output redacts known secret patterns.
- **TLS**: HTTP transports always verify certificates. `insecure`
  mode is not supported.

## Ghidra setup (example)

```toml
[mcp.servers.ghidra]
description = "Ghidra 12.x headless MCP bridge"
transport = "stdio"
command = "python3"
args = [
  "-m", "ghidra_mcp",
  "--project", "C:/ghidra/projects/default",
  "--ghidra-path", "C:/ghidra",
]
enabled = true
auto_connect = false
request_timeout_ms = 180000
max_concurrency = 2
trusted = true
```

## IDA Pro setup (example)

```toml
[mcp.servers.ida]
description = "IDA Pro 9.5 MCP bridge"
transport = "stdio"
command = "python3"
args = [
  "-m", "ida_mcp",
  "--ida-path", "C:/Program Files/IDA Professional 9.5",
]
env = [
  "IDA_95_DIR=C:/Program Files/IDA Professional 9.5",
]
enabled = true
auto_connect = false
request_timeout_ms = 120000
max_concurrency = 2
trusted = true
```

## Testing

Unit tests: `cargo test -p requiem-mcp`

Integration tests (mock stdio server, requires `python3` in PATH):
`cargo test -p requiem-mcp --test mock_server`

The mock server speaks minimal MCP JSON-RPC over stdio and validates
the full connect → discover → call_tool chain without network access.
