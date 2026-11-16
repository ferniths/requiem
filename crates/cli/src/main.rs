//! REQUIEM CLI entry point.
//!
//! Parses arguments, loads config, initializes the model client, and
//! dispatches subcommands. The `session` subcommand runs an interactive
//! REPL loop with permissions gating and context compression.

use clap::{Parser, Subcommand};
use colored::Colorize;
use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use uuid::Uuid;

use requiem_cli::config::Config;
use requiem_core::mode::Mode;
use requiem_permissions::PermissionGate;
use requiem_tools::Tool;
use requiem_tui::DashboardRenderer;

#[derive(Parser)]
#[command(name = "requiem", version, about = "REQUIEM multi-agent CLI harness")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show the startup banner and version info.
    Banner,
    /// Show or test the configured model.
    Model {
        /// Test the model connection with a simple probe.
        #[arg(long)]
        test: bool,
    },
    /// Show available slash commands.
    Help,
    /// Start an interactive session.
    Session {
        /// A specific session ID to resume.
        #[arg(long)]
        resume: Option<Uuid>,
    },
    /// Interactive first-run setup: configure endpoint, API key, model, vault.
    Setup,
    /// Install the binary into PATH (creates a symlink/junction or copies).
    Install {
        /// Target directory (default: user's AppData\Local\Microsoft\WindowsApps or ~\bin).
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
    },
}

/// A tool executor that bridges the sync `Tool` trait into the async
/// `ToolExecutor` trait required by the agent runtime, with a
/// permissions gate that enforces the current mode.
struct SyncToolExecutor {
    tools: Arc<std::sync::Mutex<HashMap<String, Box<dyn requiem_tools::Tool>>>>,
    gate: Arc<PermissionGate>,
}

impl SyncToolExecutor {
    fn new(gate: PermissionGate) -> Self {
        let tools = std::sync::Mutex::new(
            [
                Box::new(requiem_tools::FileReadTool) as Box<dyn requiem_tools::Tool>,
                Box::new(requiem_tools::FileWriteTool),
                Box::new(requiem_tools::FileEditTool),
                Box::new(requiem_tools::FileDeleteTool),
                Box::new(requiem_tools::TerminalTool),
                Box::new(requiem_tools::GitTool),
                Box::new(requiem_tools::GlobSearchTool),
                Box::new(requiem_tools::GrepSearchTool),
            ]
            .into_iter()
            .map(|t| (t.name().to_string(), t))
            .collect::<HashMap<String, Box<dyn requiem_tools::Tool>>>(),
        );
        let tools = Arc::new(tools);
        Self {
            tools,
            gate: Arc::new(gate),
        }
    }

    /// Replace the permission gate (called on mode switch).
    fn set_gate(&mut self, gate: PermissionGate) {
        self.gate = Arc::new(gate);
    }

    /// Register an additional tool in the shared map.
    fn add_tool(&mut self, tool: Box<dyn requiem_tools::Tool>) {
        self.tools
            .lock()
            .unwrap()
            .insert(tool.name().to_string(), tool);
    }

    /// A clone that shares the same tools map (cheap `Arc` clone).
    fn share(&self) -> Self {
        Self {
            tools: self.tools.clone(),
            gate: self.gate.clone(),
        }
    }

    /// Build the list of `ToolDef`s to advertise to the model.
    /// Only includes tools that the current permission gate allows.
    fn tool_defs(&self) -> Vec<requiem_model::request::ToolDef> {
        self.tools
            .lock()
            .unwrap()
            .values()
            .filter(|t| self.is_tool_allowed(t.name()))
            .map(|t| t.tool_def())
            .collect()
    }

    /// Check whether a tool is allowed under the current mode.
    fn is_tool_allowed(&self, name: &str) -> bool {
        let gate = &*self.gate;
        match name {
            "write_file" | "edit_file" | "delete_file" => gate.can_write_file().is_allowed(),
            "run_terminal" => gate.can_run_terminal(true).is_allowed(),
            "run_git" => gate.can_mutate_git().is_allowed(),
            "spawn_subagent" => gate.can_spawn_agent().is_allowed(),
            "obsidian" => gate.can_write_file().is_allowed(),
            "read_file" | "glob_search" | "grep_search" => true,
            _ => true,
        }
    }
}

#[async_trait::async_trait]
impl requiem_orchestration::agent_runtime::ToolExecutor for SyncToolExecutor {
    async fn execute(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, requiem_core::error::RequiemError> {
        // Permission check before execution.
        if !self.is_tool_allowed(name) {
            return Err(requiem_core::error::RequiemError::PermissionDenied(
                format!("tool '{name}' is denied in the current mode"),
            ));
        }
        match self.tools.lock().unwrap().get(name) {
            Some(tool) => {
                tool.execute(arguments)
                    .map_err(|e| requiem_core::error::RequiemError::Tool {
                        tool: name.to_string(),
                        message: e.to_string(),
                    })
            }
            None => Err(requiem_core::error::RequiemError::Tool {
                tool: name.to_string(),
                message: "unknown tool".to_string(),
            }),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        None => {
            show_banner_and_help();
            ExitCode::SUCCESS
        }
        Some(Commands::Banner) => {
            let banner = std::fs::read_to_string("REQUIEM-ASCII-BANNER.txt").unwrap_or_default();
            println!("{banner}");
            ExitCode::SUCCESS
        }
        Some(Commands::Model { test }) => match run_model_command(test).await {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{}", e.to_string().red());
                ExitCode::FAILURE
            }
        },
        Some(Commands::Help) => {
            for (name, desc) in requiem_cli::commands::list_commands() {
                println!("{name:<12} {desc}");
            }
            ExitCode::SUCCESS
        }
        Some(Commands::Session { resume }) => match run_session(resume).await {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{}", e.to_string().red());
                ExitCode::FAILURE
            }
        },
        Some(Commands::Setup) => match run_setup() {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{}", e.to_string().red());
                ExitCode::FAILURE
            }
        },
        Some(Commands::Install { dir }) => match run_install(dir) {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{}", e.to_string().red());
                ExitCode::FAILURE
            }
        },
    }
}

/// Run an interactive session with a REPL loop.
async fn run_session(
    resume: Option<Uuid>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::load_or_init().map_err(|e| e.to_string())?;
    let client = reqwest_client(&config)?;

    let mut mode = Mode::Code;
    let gate = PermissionGate::for_mode(mode);
    let mut executor = SyncToolExecutor::new(gate);

    let db_path = session_db_path()?;
    let session = std::rc::Rc::new(
        requiem_memory::session::Session::open(&db_path).map_err(|e| e.to_string())?,
    );
    let session_id = resume.unwrap_or_else(Uuid::new_v4);

    let mut total_tasks: usize = 0;
    let mut completed_tasks: usize = 0;
    let task_session_id = requiem_core_id();

    println!("{}", requiem_tui_banner());
    println!(
        "{} session {session_id} — mode: {} — type a task or a slash command\n",
        "REQUIEM".yellow().bold(),
        mode.slash()
    );

    let stdin = std::io::stdin();
    loop {
        print!("{} ", "›".cyan().bold());
        std::io::Write::flush(&mut std::io::stdout()).ok();

        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => {
                // EOF (Ctrl-D) — clean exit.
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        }

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        // --- Slash commands ---
        if line.starts_with('/') {
            let cmd = line.trim();

            match cmd {
                "/exit" | "/quit" => {
                    println!("exiting session {session_id}");
                    break;
                }
                "/help" => {
                    show_repl_help();
                    continue;
                }
                "/model" => {
                    match client.test_connection().await {
                        Ok(model) => println!("model reachable: {model}"),
                        Err(e) => println!("model unreachable: {e}"),
                    }
                    continue;
                }
                "/ask" => {
                    mode = Mode::Ask;
                    executor.set_gate(PermissionGate::for_mode(mode));
                    println!("mode: ask — read-only, no writes");
                    continue;
                }
                "/plan" => {
                    mode = Mode::Plan;
                    executor.set_gate(PermissionGate::for_mode(mode));
                    println!("mode: plan — plan-sandbox writes only");
                    continue;
                }
                "/code" => {
                    mode = Mode::Code;
                    executor.set_gate(PermissionGate::for_mode(mode));
                    println!("mode: code — approved writes and builds");
                    continue;
                }
                "/auto" => {
                    mode = Mode::Auto;
                    executor.set_gate(PermissionGate::for_mode(mode));
                    println!("mode: auto — fully autonomous");
                    continue;
                }
                "/debug" => {
                    mode = Mode::Debug;
                    executor.set_gate(PermissionGate::for_mode(mode));
                    println!("mode: debug — reproduce, fix, verify");
                    continue;
                }
                "/research" => {
                    mode = Mode::Research;
                    executor.set_gate(PermissionGate::for_mode(mode));
                    println!("mode: research — parallel findings, vault writes");
                    continue;
                }
                _ => {
                    println!("unknown command: {cmd} — type /help for available commands");
                    continue;
                }
            }
        }

        // --- Task submission (any non-slash line) ---
        let task = requiem_core::task::Task::new(&line);
        total_tasks += 1;

        let agent = requiem_core::agent::Agent::master(
            "master",
            requiem_core::Id::from(task_session_id.as_str().to_string()),
        );
        let mut run =
            requiem_orchestration::agent_runtime::AgentRun::new(agent.clone(), task.clone());

        println!(
            "{} task #{} — mode: {} — running…\n",
            "▸".cyan(),
            total_tasks,
            mode.slash()
        );

        let mut tools = executor.tool_defs();

        // Add the subagent tool when the current mode allows spawning.
        if executor.is_tool_allowed("spawn_subagent") {
            let client_arc: Arc<requiem_model::client::ModelClient> = Arc::new(client.clone());
            let child_executor: Arc<dyn requiem_orchestration::agent_runtime::ToolExecutor> =
                Arc::new(executor.share());
            let sub_tool = requiem_tools::make_subagent_tool(
                client_arc,
                child_executor,
                agent.clone(),
                task_session_id.clone(),
                tools.clone(),
            );
            tools.push(sub_tool.tool_def());
            executor.add_tool(Box::new(sub_tool));
        }

        // Add the obsidian vault tool when the mode allows writes.
        if executor.is_tool_allowed("obsidian") {
            let vault_root = dirs::home_dir()
                .map(|h| h.join(".requiem").join("vault"))
                .unwrap_or_default();
            std::fs::create_dir_all(&vault_root).ok();
            let obs_tool = requiem_tools::ObsidianTool::new(vault_root);
            tools.push(obs_tool.tool_def());
            executor.add_tool(Box::new(obs_tool));
        }

        let executor_arc: Arc<dyn requiem_orchestration::agent_runtime::ToolExecutor> =
            Arc::new(executor.share());

        let result = requiem_orchestration::agent_runtime::run_agent(
            &client,
            &executor_arc,
            &mut run,
            &tools,
        )
        .await;

        // Render dashboard after the run.
        let dash =
            requiem_tui_dashboard(&run, total_tasks, completed_tasks, &session_id.to_string());
        println!("{dash}");

        let task_success = match result {
            Ok(_) => {
                completed_tasks += 1;
                if let Some(resp) = &run.final_response {
                    println!("{}", render_final(resp));
                }
                true
            }
            Err(e) => {
                println!("task #{} failed: {e}", total_tasks);
                false
            }
        };

        // Persist the session record.
        let usage = run
            .final_response
            .as_ref()
            .and_then(|r| r.usage)
            .map_or((0, 0), |u| (u.input, u.output));
        let rec = requiem_memory::session::SessionRecord {
            session_id,
            task_description: line.clone(),
            role: "master".into(),
            final_response_json: run.final_response.as_ref().and_then(|r| {
                r.message
                    .content
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            }),
            tokens_input: usage.0,
            tokens_output: usage.1,
            success: task_success,
            timestamp_rfc3339: chrono::Utc::now().to_rfc3339(),
        };
        let _ = session.insert_record(&rec);
    }

    let (tin, tout) = session.session_token_totals(&session_id).unwrap_or((0, 0));
    Ok(format!(
        "session {session_id} complete — {completed_tasks}/{total_tasks} tasks succeeded, {tin} in / {tout} out tokens"
    ))
}

fn requiem_tui_banner() -> String {
    requiem_tui::banner::BANNER.to_string()
}

fn requiem_tui_dashboard(
    run: &requiem_orchestration::agent_runtime::AgentRun,
    total: usize,
    completed: usize,
    session_id: &str,
) -> String {
    let usage = run
        .final_response
        .as_ref()
        .and_then(|r| r.usage)
        .map_or((0, 0), |u| (u.input, u.output));
    let mut state = requiem_tui::DashboardState {
        agents: vec![requiem_tui::AgentRow::new("master", "master")],
        total_tasks: total,
        completed_tasks: completed,
        session_id: session_id.to_string(),
    };
    if let Some(row) = state.agents.first_mut() {
        row.status = if run.final_response.is_some() {
            requiem_tui::AgentStatus::Done
        } else {
            requiem_tui::AgentStatus::Failed
        };
        row.current_task = run.task.description.clone();
        row.turns_used = run.turns;
        row.tokens_in = usage.0;
        row.tokens_out = usage.1;
    }
    requiem_tui::PlainRenderer.render(&state)
}

fn render_final(resp: &requiem_model::response::ModelResponse) -> String {
    resp.message
        .content
        .as_ref()
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(no text content)".into())
}

fn show_repl_help() {
    println!("\n{} commands:", "available".yellow().bold());
    for (name, desc) in requiem_cli::commands::list_commands() {
        println!("  {name:<12} {desc}");
    }
    println!("  {exit:<12} leave the session", exit = "/exit");
    println!(
        "  {any:<12} submit a task to the agent\n",
        any = "(any text)"
    );
}

fn session_db_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let home = dirs::home_dir().ok_or("cannot resolve home directory")?;
    let dir = home.join(".requiem");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("sessions.db"))
}

fn requiem_core_id() -> requiem_core::Id {
    requiem_core::Id::new()
}

fn reqwest_client(
    config: &Config,
) -> Result<requiem_model::client::ModelClient, Box<dyn std::error::Error + Send + Sync>> {
    Ok(requiem_model::client::ModelClient::new(
        config.to_model_config(),
    )?)
}

fn show_banner_and_help() {
    println!("{}", requiem_tui_banner());
    println!(
        "\n{} — multi-agent CLI harness\n",
        "REQUIEM".yellow().bold()
    );
    println!("{}", "usage:".cyan().bold());
    println!("  requiem banner          show this banner");
    println!("  requiem setup            interactive first-run configuration");
    println!("  requiem model [--test]  show or test the configured model");
    println!("  requiem help            list slash commands");
    println!("  requiem session [--resume <uuid>]   start an interactive session");
    println!("  requiem install [--dir <path>]     install into PATH\n");
    println!("{}", "slash commands (in session):".cyan().bold());
    for (name, desc) in requiem_cli::commands::list_commands() {
        println!("  {name:<12} {desc}");
    }
}

async fn run_model_command(test: bool) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::load_or_init().map_err(|e| e.to_string())?;
    let client = requiem_model::client::ModelClient::new(config.to_model_config())?;
    if test {
        let model = client.test_connection().await.map_err(|e| e.to_string())?;
        Ok(format!(
            "model reachable: {model} — endpoint {}",
            client.endpoint()
        ))
    } else {
        Ok(format!(
            "model: {} — endpoint {}",
            client.model(),
            client.endpoint()
        ))
    }
}

/// Interactive first-run setup: walk through endpoint, API key, model,
/// and vault path. Writes `~/.requiem/config.toml` and optionally
/// creates the vault directory.
fn run_setup() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let home = dirs::home_dir().ok_or("cannot resolve home directory")?;
    let req_dir = home.join(".requiem");
    std::fs::create_dir_all(&req_dir)?;
    let config_path = req_dir.join("config.toml");

    println!("{} REQUIEM setup\n", "▸".cyan().bold());

    let existing = if config_path.is_file() {
        let raw = std::fs::read_to_string(&config_path)?;
        Some(
            toml::from_str::<Config>(&raw)
                .map_err(|e| format!("failed to parse existing config: {e}"))?,
        )
    } else {
        None
    };

    let mut config = existing.unwrap_or_default();

    println!("endpoint [{}]: ", config.endpoint);
    prompt_override(&mut config.endpoint);
    println!("model [{}]: ", config.model);
    prompt_override(&mut config.model);
    let api_key_display = if config.api_key.is_empty() {
        "(empty — use REQUIEM_API_KEY env var)"
    } else {
        "(set)"
    };
    println!("api_key [{api_key_display}]: ");
    prompt_override(&mut config.api_key);

    let raw = toml::to_string_pretty(&config)?;
    std::fs::write(&config_path, raw)?;
    println!(
        "\n{} config written to {}",
        "✓".green().bold(),
        config_path.display()
    );

    // Optionally create the vault directory.
    let vault_root = req_dir.join("vault");
    if !vault_root.is_dir() {
        println!(
            "vault directory {} does not exist — create it? [y/N]: ",
            vault_root.display()
        );
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim().eq_ignore_ascii_case("y") {
            std::fs::create_dir_all(&vault_root)?;
            println!("✓ vault created at {}", vault_root.display());
        }
    }

    println!(
        "\nrun `requiem model --test` to verify the model endpoint, then `requiem session` to start."
    );
    Ok("setup complete".into())
}

/// Prompt for a value; if the user just presses Enter, keep the existing value.
fn prompt_override(current: &mut String) {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    let trimmed = input.trim().to_string();
    if !trimmed.is_empty() {
        *current = trimmed;
    }
}

/// Install the current binary into a PATH-visible location so `requiem`
/// can be invoked from any directory.
fn run_install(
    dir: Option<std::path::PathBuf>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot resolve current executable path: {e}"))?;

    let target_dir = match dir {
        Some(d) => d,
        None => {
            // On Windows, use %LOCALAPPDATA%\bin (or create it).
            let local = std::env::var("LOCALAPPDATA")
                .ok()
                .map(|p| std::path::PathBuf::from(p).join("bin"))
                .filter(|p| p.is_dir() || p.parent().map(|pp| pp.is_dir()).unwrap_or(false));
            local.ok_or("cannot resolve install directory; use --dir <path>")?
        }
    };

    std::fs::create_dir_all(&target_dir)?;

    // Always install as "requiem" (with the platform exe suffix).
    let target_name = if cfg!(windows) {
        "requiem.exe".to_string()
    } else {
        "requiem".to_string()
    };
    let target = target_dir.join(&target_name);
    std::fs::copy(&exe, &target)?;

    println!("{} installed to {}", "✓".green().bold(), target.display());

    // Check if the target dir is on PATH.
    let path_env = std::env::var("PATH").unwrap_or_default();
    let target_canonical = target_dir.canonicalize().ok();
    let is_on_path = path_env
        .split(';')
        .filter_map(|p| std::path::Path::new(p).canonicalize().ok())
        .any(|p| Some(p) == target_canonical);

    if !is_on_path {
        println!(
            "\n{} {} is not on PATH.",
            "⚠".yellow().bold(),
            target_dir.display()
        );
        println!(
            "Add it to your PATH environment variable, or run via full path: {}",
            target.display()
        );
    }

    Ok(format!(
        "installed to {target_display}",
        target_display = target.display()
    ))
}
