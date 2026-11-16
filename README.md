# REQUIEM

High-performance, cross-platform multi-agent CLI coding and knowledge-management harness written in Rust.

All agents share a single globally-configured model (the "9router" route). What differs between agents is their role, the context they receive, and the permission profile they run under — never the model.

## Architecture

```
requiem
├── main.rs          entry point, REPL loop, subcommands
├── config.rs        TOML config at ~/.requiem/config.toml
└── commands.rs      slash-command dispatch
    │
    ├── requiem-core           Id, Task, Agent, Mode, errors
    ├── requiem-model          9router ModelClient (HTTP, streaming, retry)
    ├── requiem-orchestration  agent runtime, scheduler, subagent loop
    ├── requiem-context        context compression (ContextBudget, Compressor)
    ├── requiem-permissions    PermissionGate (mode → profile → Decision)
    ├── requiem-tools          file, terminal, search tool implementations
    ├── requiem-project        project detection (cargo, node, python, git)
    ├── requiem-obsidian       vault operations (read, write, audit)
    ├── requiem-memory         SQLite session persistence (~/.requiem/sessions.db)
    └── requiem-tui            banner + dashboard renderer (plain text)
```

### Key design decisions

- **Single model, many agents.** One `ModelClient` behind an `Arc` serves every agent. Concurrency is bounded by `max_concurrency`; token accounting is centralized.
- **Permissions gate every tool call.** Each `Mode` (Ask/Plan/Code/Auto/Debug/Research) carries a `PermissionProfile`. The `SyncToolExecutor` checks the gate before executing and before advertising tools to the model.
- **Context compression in the agent loop.** When the accumulated message history exceeds the `ContextBudget` (4 bytes/token heuristic), the `Compressor` collapses the oldest tool results to placeholders.
- **Verification loop.** `run_agent_verified` re-checks a completed task with a `Verifier` before marking it done. Failed verifications retry up to a bounded attempt count, feeding the failure reason back into the agent's history.
- **Git tooling.** `GitTool` exposes status/diff/log/add/commit/branch/remote as a single `run_git` action, gated by the mode's `git_mutation` permission.
- **Multi-agent delegation.** `SubagentTool` lets the model call `spawn_subagent` to delegate a subtask to a child agent. The child runs with the same model, a focused context slice, and the parent's permission profile. Depth is bounded to 4. The subagent's final response is returned to the parent as a tool result.
- **Concurrent dispatch.** `run_agents_concurrent` runs multiple agent tasks in parallel via `futures::join_all`, returning results in input order.
- **Obsidian vault tooling.** `ObsidianTool` exposes read/write/list/audit on the user's vault at `~/.requiem/vault`. Writes are atomic with a JSON change log. Gated by `can_write_file()`.
- **Session persistence.** Every completed task writes a `SessionRecord` to SQLite. Resume with `requiem session --resume <uuid>`.

## Building

```bash
cargo build --release
```

The binary is `target/release/requiem`.

## Configuration

First run writes `~/.requiem/config.toml`:

```toml
model = "gpt-4o"
endpoint = "http://localhost:11434/v1"
api_key = ""
max_concurrency = 4
timeout_ms = 120000
max_retries = 3
max_context_tokens = 128000
```

The API key can also come from the `REQUIEM_API_KEY` environment variable (takes precedence over the TOML value).

## Usage

```bash
# Show banner and help
requiem

# Show the ASCII banner
requiem banner

# Show the configured model
requiem model

# Test the model connection
requiem model --test

# List slash commands
requiem help

# Start an interactive session
requiem session

# Resume a previous session
requiem session --resume <uuid>
```

### Slash commands (in-session)

| Command | Description |
|---------|-------------|
| `/ask` | Switch to Ask mode — read-only, no writes |
| `/plan` | Switch to Plan mode — plan-sandbox writes only |
| `/code` | Switch to Code mode — approved writes and builds |
| `/auto` | Switch to Auto mode — fully autonomous |
| `/debug` | Switch to Debug mode — reproduce, fix, verify |
| `/research` | Switch to Research mode — parallel findings, vault writes |
| `/model` | Test the configured model connection |
| `/help` | Show available commands |
| `/exit` | Leave the session |

Any non-slash line submits a task to the agent.

## Testing

```bash
cargo test --workspace
```

Includes unit tests across all crates and integration tests with a mock HTTP server in `crates/model/tests/mock_server.rs`.

## License

MIT
