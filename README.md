# REQUIEM

```
   .   .     ...
    :~"8888x :"%888x
   8    8888Xf  8888>
  X88x. ?8888k  8888X
 '8888L'8888X  '%88X
  "888X 8888X:xnHH(``
     ?8~ 8888X X8888
  -~`   8888> X8888
  :H8x  8888  X8888
   8888> 888~  X8888
   48"` '8*~   `8888!`
    ^-==""      `""
```

A high-performance, cross-platform multi-agent CLI coding and knowledge-management harness written in Rust.

All agents share a single globally-configured model. What differs between agents is their role, the context they receive, and the permission profile they run under — never the model.

---

## Features

- **Single model, many agents.** One `ModelClient` behind an `Arc` serves every agent. Concurrency is bounded by `max_concurrency`; token accounting is centralized.
- **Six operating modes.** `ask`, `plan`, `code`, `auto`, `debug`, `research` — each carries a distinct `PermissionProfile` that gates every tool call before execution and before advertising tools to the model.
- **Context compression.** When accumulated message history exceeds the `ContextBudget` (4 bytes/token heuristic), the `Compressor` collapses the oldest tool results to placeholders, keeping the window within model limits.
- **Verification loop.** `run_agent_verified` re-checks a completed task with a `Verifier` before marking it done. Failed verifications retry up to a bounded attempt count, feeding the failure reason back into the agent's history.
- **Multi-agent delegation.** `SubagentTool` lets the model call `spawn_subagent` to delegate a subtask to a child agent. The child runs with the same model, a focused context slice, and the parent's permission profile. Depth is bounded to 4.
- **Concurrent dispatch.** `run_agents_concurrent` runs multiple agent tasks in parallel via `futures::join_all`, returning results in input order.
- **Obsidian vault tooling.** `ObsidianTool` exposes read/write/list/audit on the user's vault. Writes are atomic with a JSON change log. The audit pass detects broken wikilinks, orphan notes, and duplicate content (SHA-256).
- **Session persistence.** Every completed task writes a `SessionRecord` to SQLite. Resume with `requiem session --resume <uuid>`.
- **Theme system.** All colors draw from a single `Theme` struct. Three variants: `default`, `high_contrast`, `muted`. The banner, dashboard, and REPL chrome all consume the same token set.

---

## Architecture

```
requiem (crates/cli)
├── main.rs          CLI entry point, REPL loop, subcommands
├── config.rs        TOML config at ~/.requiem/config.toml
├── setup.rs         interactive 7-step first-run wizard
└── commands.rs      slash-command dispatch
    │
    ├── requiem-core           Id, Task, Agent, Mode, Role, errors
    ├── requiem-model          9router ModelClient (HTTP, streaming, retry)
    ├── requiem-orchestration  agent runtime, scheduler, subagent loop, verifier
    ├── requiem-context        context compression (ContextBudget, Compressor)
    ├── requiem-permissions    PermissionGate (mode → profile → Decision)
    ├── requiem-tools          file, terminal, search, git, obsidian, subagent tools
    ├── requiem-project        project detection (cargo, node, python, git)
    ├── requiem-obsidian       vault operations (read, write, audit, wikilinks)
    ├── requiem-memory         SQLite session persistence (~/.requiem/sessions.db)
    └── requiem-tui            banner + dashboard renderer + theme system
```

### Agent roles

The `Role` enum defines the specialist a subagent can be spawned with:

| Role | Use |
|------|-----|
| `master` | Top-level coordinator for a user task |
| `researcher` | Investigates files, docs, and sources; produces structured findings |
| `architect` | Designs architecture and decomposition before implementation |
| `planner` | Produces a structured plan of subtasks |
| `implementer` | Writes or modifies code/files |
| `debugger` | Investigates and fixes failures |
| `tester` | Writes and runs tests |
| `reviewer` | Independently inspects results |
| `security` | Security-focused review |
| `documenter` | Writes documentation and notes |
| `refactorer` | Restructures code without changing behavior |

### Permission model

Each `Mode` maps to a `PermissionProfile` with five axes:

| Axis | Values |
|------|--------|
| `file_write` | `Denied` / `PlanOnly` / `Approved` / `Full` |
| `terminal` | `ReadOnly` / `Approved` / `Full` |
| `git_mutation` | `Denied` / `StatusOnly` / `Approved` / `Full` |
| `vault_write` | `Denied` / `Read` / `Write` |
| `agent_spawn` | `Denied` / `Read` / `Approved` / `Full` |

The `SyncToolExecutor` checks the gate before executing a tool and before advertising tools to the model. Tools that are denied under the current mode are simply invisible to the model.

### Available tools

| Tool | Name | Gated by |
|------|------|----------|
| `read_file` | `FileReadTool` | always allowed |
| `write_file` | `FileWriteTool` | `can_write_file()` |
| `edit_file` | `FileEditTool` | `can_write_file()` |
| `delete_file` | `FileDeleteTool` | `can_write_file()` |
| `run_terminal` | `TerminalTool` | `can_run_terminal()` |
| `run_git` | `GitTool` | `can_mutate_git()` |
| `glob_search` | `GlobSearchTool` | always allowed |
| `grep_search` | `GrepSearchTool` | always allowed |
| `spawn_subagent` | `SubagentTool` | `can_spawn_agent()` |
| `obsidian` | `ObsidianTool` | `can_write_file()` |

---

## Installation

### Prerequisites

- Rust toolchain (`cargo`, Rust 1.70+ recommended)
- An OpenAI-compatible model endpoint (e.g. Ollama, LM Studio, OpenAI, etc.)

### Windows

```powershell
# From the project root
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
```

The script builds the release binary and installs it to `%LOCALAPPDATA%\bin\requiem.exe`, adding that directory to your user PATH. Open a new terminal afterwards.

Alternatively, install manually:

```powershell
cargo build --release
copy target\release\requiem.exe $env:LOCALAPPDATA\bin\
# add %LOCALAPPDATA%\bin to PATH if not already there
```

### Linux / macOS

```bash
# From the project root
bash scripts/install.sh
```

The script builds the release binary and installs it to `~/.local/bin/requiem`, adding that directory to your shell rc file.

### Build from source

```bash
cargo build --release
```

The binary lands at `target/release/requiem.exe` (Windows) or `target/release/requiem` (Linux/macOS).

---

## First Run

On first run with no config, REQUIEM prints the banner and help. Run the interactive 7-step setup wizard:

```bash
requiem setup
```

The wizard walks through:

1. **Welcome** — confirm readiness to begin
2. **9router connection** — endpoint, API key, model name (optional connection test)
3. **Agent limits** — max depth, token budget, task budget, dynamic scaling
4. **Permissions** — choose the default operating mode
5. **Obsidian vaults** — register one or more vault paths
6. **Appearance** — pick a theme variant (live preview shown)
7. **Summary** — review and save

The config is written to `~/.requiem/config.toml`. Nothing is saved until you confirm on the final step.

### Config reference

```toml
# ~/.requiem/config.toml

# ── Model ───────────────────────────────────────────────────────────
model = "gpt-4o"
endpoint = "http://localhost:11434/v1"
api_key = ""              # or set REQUIEM_API_KEY env var (takes precedence)
max_concurrency = 4
timeout_ms = 120000
max_retries = 3
max_context_tokens = 128000

# ── Agent limits ────────────────────────────────────────────────────
max_agent_depth = 4       # master → worker → subagent… (1–16)
token_budget = 50000      # per-agent task token budget
max_task_budget = 20      # max tasks per session
dynamic_scaling = false

# ── UI ─────────────────────────────────────────────────────────────
theme = "default"          # default | high_contrast | muted
default_mode = "code"     # ask | plan | code | auto | debug | research

# ── Obsidian vaults ────────────────────────────────────────────────
vaults = []               # paths to vault directories; first is the default
```

---

## Usage

```bash
# Show banner and help (launches setup wizard if not yet configured)
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

# Run the interactive setup wizard
requiem setup

# Install into PATH
requiem install
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
| `/setup` | Re-run the interactive setup wizard |
| `/theme` | List available theme variants with live previews |
| `/help` | Show available commands (grouped by category) |
| `/exit` | Leave the session |

Any non-slash line submits a task to the agent.

### Themes

Three theme variants are available via `config.theme` or `/theme` in-session:

| Variant | Description |
|---------|-------------|
| `default` | Crimson accent, high-contrast text, standard muted colors |
| `high_contrast` | Brighter secondary and border text for accessibility |
| `muted` | Soft, low-stimulation palette with dimmed accent |

---

## Project detection

`requiem-project` auto-detects the language and toolchain of the current working directory:

| Project type | Detection signal |
|--------------|-----------------|
| Cargo | `Cargo.toml` present |
| Node | `package.json` present |
| Python | `pyproject.toml` or `requirements.txt` present |
| Git | `.git/` directory present |

The detected project type informs tool descriptions and permission defaults.

---

## Session persistence

Sessions are stored in `~/.requiem/sessions.db` (SQLite). Each completed task writes a `SessionRecord`:

```
session_id, task_description, role, final_response_json,
tokens_input, tokens_output, success, timestamp_rfc3339
```

Resume a previous session:

```bash
requiem session --resume <session-uuid>
```

---

## Testing

```bash
cargo test --workspace
```

Includes unit tests across all crates and integration tests with a mock HTTP server in `crates/model/tests/mock_server.rs`.

---

## License

MIT
