//! The interactive session driver.
//!
//! Two modes share one command surface:
//!
//! * **Full-screen** (TTY): a ratatui app renders the live conversation,
//!   dashboard overlay and input; the agent runtime runs as a background
//!   task broadcasting [`Event`]s that the UI drains without ever
//!   blocking it.
//! * **Plain** (piped / no TTY): a simple line loop that prints event
//!   summaries as they arrive — same commands, same runtime.
//!
//! Tool execution goes through a permission gate that follows the active
//! mode; the gate is shared, so mode switches propagate to in-flight
//! executor clones.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::clipboard;
use crate::commands as slash;
use crate::config::Config;
use requiem_core::agent::AgentId;
use requiem_core::events::{Event, NoticeLevel};
use requiem_core::mode::Mode;
use requiem_orchestration::agent_runtime::{
    run_agent_halted, AgentRun, CancelFlag, EventTx, ToolExecutor,
};
use requiem_orchestration::scheduler::{Scheduler, SchedulerConfig, SharedScheduler};
use requiem_permissions::PermissionGate;
use requiem_tools::{GrepSearchTool, ObsidianTool, Tool};
use requiem_tui::app::{App, CommandHints, UiAction};
use requiem_tui::theme::{theme_from_name, Theme, THEME_NAMES};

use crate::history::InputHistory;

/// Poll interval for the full-screen UI loop.
const UI_TICK: Duration = Duration::from_millis(50);

/// Two paste events carrying identical text within this window are treated
/// as one physical clipboard delivery (some terminals duplicate them).
const PASTE_DEDUPE_WINDOW: Duration = Duration::from_millis(80);

/// A registered tool bridge: shared tool map + shared permission gate.
struct ToolBridge {
    tools: Arc<std::sync::Mutex<HashMap<String, Arc<dyn Tool>>>>,
    gate: Arc<RwLock<PermissionGate>>,
}

impl ToolBridge {
    fn new(gate: PermissionGate) -> Self {
        let tools: HashMap<String, Arc<dyn Tool>> = [
            Arc::new(requiem_tools::FileReadTool) as Arc<dyn Tool>,
            Arc::new(requiem_tools::FileWriteTool),
            Arc::new(requiem_tools::FileEditTool),
            Arc::new(requiem_tools::FileDeleteTool),
            Arc::new(requiem_tools::TerminalTool),
            Arc::new(requiem_tools::GitTool),
            Arc::new(requiem_tools::GlobSearchTool),
            Arc::new(requiem_tools::GrepSearchTool),
        ]
        .into_iter()
        .map(|t| (t.name().to_string(), t))
        .collect();
        Self {
            tools: Arc::new(std::sync::Mutex::new(tools)),
            gate: Arc::new(RwLock::new(gate)),
        }
    }

    /// Switch the permission gate (mode change). Propagates to clones.
    async fn set_mode(&self, mode: Mode) {
        *self.gate.write().await = PermissionGate::for_mode(mode);
    }

    fn add_tool(&self, tool: Arc<dyn Tool>) {
        self.tools
            .lock()
            .unwrap()
            .insert(tool.name().to_string(), tool);
    }

    async fn is_tool_allowed(&self, name: &str) -> bool {
        let gate = self.gate.read().await;
        match name {
            "write_file" | "edit_file" | "delete_file" => gate.can_write_file().is_allowed(),
            "run_terminal" => gate.can_run_terminal(true).is_allowed(),
            "run_git" => gate.can_mutate_git().is_allowed(),
            "spawn_subagent" => gate.can_spawn_agent().is_allowed(),
            "obsidian" => gate.can_write_vault().is_allowed(),
            "read_file" | "glob_search" | "grep_search" => true,
            _ => true,
        }
    }

    /// The `ToolDef`s to advertise to the model under the current gate.
    async fn tool_defs(&self) -> Vec<requiem_model::request::ToolDef> {
        let names: Vec<String> = {
            let map = self.tools.lock().unwrap();
            map.keys().cloned().collect()
        };
        let mut defs = Vec::new();
        for name in names {
            if !self.is_tool_allowed(&name).await {
                continue;
            }
            let def = {
                let map = self.tools.lock().unwrap();
                map.get(&name).map(|t| t.tool_def())
            };
            if let Some(def) = def {
                defs.push(def);
            }
        }
        defs
    }

    /// A sharable async executor handle.
    fn share(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(BridgeExecutor {
            tools: self.tools.clone(),
            gate: self.gate.clone(),
        })
    }
}

/// The async executor the agent runtime calls.
struct BridgeExecutor {
    tools: Arc<std::sync::Mutex<HashMap<String, Arc<dyn Tool>>>>,
    gate: Arc<RwLock<PermissionGate>>,
}

#[async_trait::async_trait]
impl ToolExecutor for BridgeExecutor {
    async fn execute(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, requiem_core::error::RequiemError> {
        {
            let gate = self.gate.read().await;
            let allowed = match name {
                "write_file" | "edit_file" | "delete_file" => gate.can_write_file().is_allowed(),
                "run_terminal" => gate.can_run_terminal(true).is_allowed(),
                "run_git" => gate.can_mutate_git().is_allowed(),
                "spawn_subagent" => gate.can_spawn_agent().is_allowed(),
                "obsidian" => gate.can_write_vault().is_allowed(),
                _ => true,
            };
            if !allowed {
                return Err(requiem_core::error::RequiemError::PermissionDenied(
                    format!("tool '{name}' is denied in the current mode"),
                ));
            }
        }
        let tool: Arc<dyn Tool> = {
            let map = self.tools.lock().unwrap();
            match map.get(name).cloned() {
                Some(t) => t,
                None => {
                    return Err(requiem_core::error::RequiemError::Tool {
                        tool: name.to_string(),
                        message: "unknown tool".to_string(),
                    })
                }
            }
        };
        tool.execute(arguments)
            .await
            .map_err(|e| requiem_core::error::RequiemError::Tool {
                tool: name.to_string(),
                message: e.to_string(),
            })
    }
}

/// What a slash command asks the driver to do next.
enum Directive {
    Continue,
    Exit,
    /// Re-run setup: leave full-screen, run the wizard, come back.
    RunSetup,
}

/// Run an interactive session and return a summary line.
pub async fn run_session(
    resume: Option<Uuid>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let config = match Config::load() {
        Ok(c) => c,
        Err(crate::config::ConfigError::NotInitialized) => {
            return Err("no configuration found — run `requiem setup` first".into());
        }
        Err(e) => return Err(e.to_string().into()),
    };
    requiem_tui::init_color_mode();
    let theme = theme_from_name(&config.theme);
    let client = Arc::new(requiem_model::client::ModelClient::new(
        config.to_model_config(),
    )?);
    let hints = build_command_hints();

    let session_id = resume.unwrap_or_else(Uuid::new_v4);
    let session_label = session_id.to_string();

    // Persistence.
    let db_path = session_db_path()?;
    let store = requiem_memory::session::Session::open(&db_path).map_err(|e| e.to_string())?;

    // Runtime channels.
    let (event_tx, event_rx) = tokio::sync::broadcast::channel::<Event>(1024);

    let mode = Mode::from_slash(&format!("/{}", config.default_mode)).unwrap_or(Mode::Code);
    let bridge = ToolBridge::new(PermissionGate::for_mode(mode));

    // Vault tool from configured vaults (first = default).
    if !config.vaults.is_empty() {
        let roots: Vec<std::path::PathBuf> =
            config.vaults.iter().map(std::path::PathBuf::from).collect();
        bridge.add_tool(std::sync::Arc::new(ObsidianTool::with_vaults(roots)));
    }

    // Seed the full-screen app with prior records on resume.
    let prior = if resume.is_some() {
        store.records_for_session(&session_id).unwrap_or_default()
    } else {
        Vec::new()
    };

    let is_tty = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();

    let summary = if is_tty {
        run_fullscreen(
            &config,
            theme,
            client,
            bridge,
            hints,
            event_tx,
            event_rx,
            &store,
            session_id,
            &session_label,
            prior,
        )
        .await?
    } else {
        run_plain(
            &config, theme, client, bridge, event_tx, &store, session_id, prior,
        )
        .await?
    };

    Ok(summary)
}

/// Build the command hints injected into the TUI app.
fn build_command_hints() -> CommandHints {
    let mut hints = CommandHints::default();
    for (name, desc) in slash::list_commands() {
        hints.names.push(name.to_string());
        hints
            .descriptions
            .insert(name.to_string(), desc.to_string());
        let args = slash::suggest_args(name);
        if !args.is_empty() {
            hints.args.insert(
                name.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            );
        }
    }
    hints
}

fn session_db_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let home = dirs::home_dir().ok_or("cannot resolve home directory")?;
    let dir = home.join(".requiem");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("sessions.db"))
}

// ── Full-screen mode ────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn run_fullscreen(
    config: &Config,
    theme: Theme,
    client: Arc<requiem_model::client::ModelClient>,
    bridge: ToolBridge,
    hints: CommandHints,
    event_tx: EventTx,
    mut event_rx: tokio::sync::broadcast::Receiver<Event>,
    store: &requiem_memory::session::Session,
    session_id: Uuid,
    session_label: &str,
    prior: Vec<requiem_memory::session::SessionRecord>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    use crossterm::event::{Event as CEvent, EventStream};
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    // Enable bracketed paste so terminals deliver clipboard content as a
    // single CEvent::Paste instead of a flood of keypresses. Safe to enable
    // even if the terminal doesn't support it — old terminals just won't
    // send paste events.
    crossterm::execute!(stdout, crossterm::event::EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Restore the terminal even if we panic mid-frame.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
        previous_hook(info);
    }));

    let mut app = App::new(
        theme,
        hints,
        session_label.to_string(),
        client.model().to_string(),
    );
    app.workdir = std::env::current_dir().ok();
    app.set_mode(config.default_mode.as_str());
    if prior.is_empty() {
        // Fresh session: show the branded welcome screen with the ASCII banner.
        let proj_dir = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".into());
        app.push_welcome(&proj_dir);
    } else {
        // Resumed session: restore the conversation, not the welcome.
        for rec in &prior {
            let mark = if rec.success { "✓" } else { "✗" };
            app.push_system(
                &format!("{mark} (resumed) {}", rec.task_description),
                if rec.success {
                    NoticeLevel::Success
                } else {
                    NoticeLevel::Error
                },
            );
        }
    }

    let mut history = InputHistory::load();
    app.set_history(history.entries().to_vec());

    // Last physical paste (content + time), used only to drop duplicate
    // events for the *same* clipboard delivery. Never used to suppress a
    // deliberate second paste.
    let mut last_paste: Option<(String, std::time::Instant)> = None;

    // Last real (non-slash) request, so `/retry` can re-run it.
    let mut last_prompt: Option<String> = None;

    let mut tasks_total: usize = prior.len();
    let mut tasks_ok: usize = prior.iter().filter(|r| r.success).count();
    // One shared flag for the spawn_subagent tool's children (registered
    // once below); /stop and Ctrl-C cancel every in-flight run via the
    // per-run flags stored in active_runs.
    let subagent_cancel = CancelFlag::new();
    // The scheduler is the source of truth for task state. On resume,
    // restore the last checkpointed task graph so in-flight multi-agent
    // work continues rather than starting fresh.
    let scheduler: SharedScheduler = Arc::new(std::sync::Mutex::new({
        let cfg = SchedulerConfig {
            max_agents: config.max_concurrency as usize,
            ..Default::default()
        };
        if let Some(graph) = store.load_task_graph(&session_id).unwrap_or_default() {
            Scheduler::restore(cfg, graph)
        } else {
            Scheduler::new(cfg)
        }
    }));
    // Concurrent task runs (bounded by max_concurrency). Each run gets
    // its own cancel flag so /stop and Ctrl-C target only what is
    // actually in flight without cancelling the others' children.
    let mut active_runs: Vec<(requiem_core::Id, tokio::task::JoinHandle<()>, CancelFlag)> =
        Vec::new();
    // Finished runs are delivered here so the UI loop can persist their
    // records; the JoinHandle alone would lose the AgentRun state.
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<AgentRun>();

    let mut events: crossterm::event::EventStream = EventStream::new();

    // Register the subagent tools once; every run binds to the same
    // shared subagent_cancel flag.
    if bridge.is_tool_allowed("spawn_subagent").await {
        register_subagent_tools(
            &bridge,
            &client,
            &event_tx,
            &subagent_cancel,
            config.max_agent_depth,
        )
        .await;
    }

    let result: Result<String, String> = loop {
        // 1. Drain runtime events (never blocks the runtime). On a lagged
        //    broadcast, skip only the dropped events and keep draining —
        //    stopping at the first error would leave the rest of the queue
        //    unread until the next tick.
        loop {
            match event_rx.try_recv() {
                Ok(ev) => app.push_event(&ev),
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!("UI lagged; dropped {n} runtime events");
                }
                Err(_) => break,
            }
        }

        // 2. Reap completed runs. The JoinHandle catches panics; the done
        //    channel delivers the final AgentRun for persistence.
        if active_runs.iter().any(|(_, h, _)| h.is_finished()) {
            let mut still_active = Vec::new();
            let mut aborted: Vec<(requiem_core::Id, bool)> = Vec::new();
            for entry in active_runs.drain(..) {
                let (task_id, handle, flag) = entry;
                if handle.is_finished() {
                    // Reap — completion arrives via the done channel. A join
                    // error means the task panicked and therefore never sent
                    // its AgentRun or terminal event; record that so the
                    // scheduler and UI cannot keep a stale "running" state.
                    let panicked = handle.await.is_err();
                    aborted.push((task_id, panicked));
                } else {
                    still_active.push((task_id, handle, flag));
                }
            }
            active_runs = still_active;
            for (task_id, panicked) in aborted {
                if panicked {
                    app.push_event(&Event::RunFailed {
                        agent: "master".into(),
                        task: task_id.clone(),
                        error: "agent task panicked; its state was discarded".into(),
                        error_class: "internal".into(),
                    });
                    let mut sched = scheduler.lock().unwrap();
                    sched.fail_task(&task_id, "agent task panicked");
                    if let Some(a) = sched.registry.agents.get_mut(&AgentId("master".into())) {
                        a.fail();
                    }
                    let _ = store.save_task_graph(&session_id, &sched.graph);
                }
            }
        }
        while let Ok(run) = done_rx.try_recv() {
            let ok = run.final_response.is_some();
            persist_run(store, session_id, &run);
            tasks_ok += usize::from(ok);
            // Update the scheduler's task graph and agent registry.
            let task_id = run.task.id.clone();
            {
                let mut sched = scheduler.lock().unwrap();
                if ok {
                    let result = run
                        .final_response
                        .as_ref()
                        .and_then(|r| r.text())
                        .map(|s| serde_json::Value::String(s.to_string()))
                        .unwrap_or(serde_json::Value::Null);
                    sched.complete_task(&task_id, result);
                } else {
                    sched.fail_task(&task_id, "run did not complete");
                }
                // Mark the agent's terminal status so it frees its slot.
                let agent_id = AgentId("master".into());
                if let Some(a) = sched.registry.agents.get_mut(&agent_id) {
                    if ok {
                        a.finish();
                    } else {
                        a.fail();
                    }
                }
                // Checkpoint the updated graph so a crash or restart can
                // resume multi-agent state, not just finished records.
                let _ = store.save_task_graph(&session_id, &sched.graph);
            }
        }

        // Reconcile the UI's notion of in-flight work with the authoritative
        // list of running tasks. This is idempotent, so a terminal event that
        // arrives a tick later cannot flip the status the wrong way, and a
        // run that died without an event can never look permanently active.
        let running_ids: Vec<String> = active_runs
            .iter()
            .map(|(id, _, _)| id.as_str().to_string())
            .collect();
        app.sync_active_runs(&running_ids);

        // 3. Draw.
        terminal
            .draw(|f| app.render(f))
            .map_err(|e| format!("draw error: {e}"))?;

        // 4. Input with a tick so spinners and streams stay live.
        let Some(cev) = tokio::time::timeout(UI_TICK, events.next())
            .await
            .ok()
            .flatten()
        else {
            continue;
        };
        // Bracketed paste: the terminal delivers clipboard content as one
        // event instead of a keypress flood. Insert it into the input
        // buffer without triggering submit or per-char validation.
        //
        // Some terminals emit the same physical paste as two closely spaced
        // events. Deduplicate only exact repeats inside a very short window
        // — never by comparing against the existing prompt text (a
        // legitimate second paste of identical text must go through).
        if let Ok(CEvent::Paste(pasted)) = cev {
            let normalized = requiem_tui::composer::normalize_line_endings(&pasted);
            let duplicate = last_paste.as_ref().is_some_and(|(text, at)| {
                *text == normalized && at.elapsed() < PASTE_DEDUPE_WINDOW
            });
            if !duplicate {
                app.paste_input(&normalized);
            }
            last_paste = Some((normalized, std::time::Instant::now()));
            continue;
        }
        // Native clipboard fallback: when bracketed paste is unavailable,
        // Alt+V reads the OS clipboard directly.
        if let Ok(CEvent::Key(key)) = cev {
            if key.kind == crossterm::event::KeyEventKind::Press
                && key.code == crossterm::event::KeyCode::Char('v')
                && key.modifiers.contains(crossterm::event::KeyModifiers::ALT)
                && clipboard::is_supported()
            {
                match clipboard::read_clipboard() {
                    Some(text) if !text.is_empty() => {
                        let normalized = requiem_tui::composer::normalize_line_endings(&text);
                        let duplicate = last_paste.as_ref().is_some_and(|(prev, at)| {
                            *prev == normalized && at.elapsed() < PASTE_DEDUPE_WINDOW
                        });
                        if !duplicate {
                            app.paste_input(&normalized);
                        }
                        last_paste = Some((normalized, std::time::Instant::now()));
                    }
                    _ => {
                        app.push_system("clipboard is empty or unavailable", NoticeLevel::Warning);
                    }
                }
                continue;
            }
            let action = app.handle_key(key);
            let directive = handle_action(
                &mut app,
                action,
                config,
                &client,
                &bridge,
                store,
                session_id,
                &event_tx,
                &subagent_cancel,
                &mut tasks_total,
                &mut active_runs,
                &done_tx,
                &mut history,
                &scheduler,
                &mut last_prompt,
            )
            .await;
            match directive {
                Directive::Continue => {}
                Directive::Exit => {
                    break Ok(final_summary(store, session_id, tasks_total, tasks_ok));
                }
                Directive::RunSetup => {
                    // Leave full-screen, run the wizard, rebuild.
                    let _ = disable_raw_mode();
                    let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
                    match crate::setup::run_wizard() {
                        Ok(cfg) => {
                            let t = theme_from_name(&cfg.theme);
                            app = App::new(
                                t,
                                build_command_hints(),
                                session_label.to_string(),
                                client.model().to_string(),
                            );
                            app.workdir = std::env::current_dir().ok();
                            app.set_mode(cfg.default_mode.as_str());
                            app.set_history(history.entries().to_vec());
                            let proj_dir = std::env::current_dir()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|_| ".".into());
                            app.push_welcome(&proj_dir);
                            app.push_system(
                                "setup complete — new config is live",
                                NoticeLevel::Success,
                            );
                        }
                        Err(e) => {
                            app.push_system(&format!("setup cancelled: {e}"), NoticeLevel::Warning);
                        }
                    }
                    enable_raw_mode()?;
                    crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
                }
            }
        }
    };

    // Tear down the full-screen terminal.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
    history.save();

    match result {
        Ok(summary) => Ok(summary),
        Err(e) => Err(e.into()),
    }
}

/// Apply a UI action; returns the next driver directive.
#[allow(clippy::too_many_arguments)]
async fn handle_action(
    app: &mut App,
    action: UiAction,
    config: &Config,
    client: &Arc<requiem_model::client::ModelClient>,
    bridge: &ToolBridge,
    store: &requiem_memory::session::Session,
    session_id: Uuid,
    event_tx: &EventTx,
    cancel: &CancelFlag,
    tasks_total: &mut usize,
    active_runs: &mut Vec<(requiem_core::Id, tokio::task::JoinHandle<()>, CancelFlag)>,
    done_tx: &tokio::sync::mpsc::UnboundedSender<AgentRun>,
    history: &mut InputHistory,
    scheduler: &SharedScheduler,
    last_prompt: &mut Option<String>,
) -> Directive {
    match action {
        UiAction::None => Directive::Continue,
        UiAction::Quit => Directive::Exit,
        UiAction::CancelRun => {
            // Cancel every in-flight run (and the subagent tool's
            // children, which share the subagent flag bound to the
            // session).
            for (_, _, flag) in active_runs.iter() {
                flag.cancel();
            }
            cancel.cancel();
            app.push_system("stopping…", NoticeLevel::Warning);
            Directive::Continue
        }
        UiAction::Submit(line) => {
            // `/retry` re-runs the last real request (not a slash command),
            // so a transient provider failure is one keystroke from
            // recovery.
            if line.trim() == "/retry" {
                match last_prompt.clone() {
                    Some(prev) => {
                        app.push_system("retrying previous request…", NoticeLevel::Info);
                        let mode_str = app.mode.clone();
                        start_task(
                            app,
                            &prev,
                            config,
                            client,
                            bridge,
                            session_id,
                            event_tx,
                            cancel,
                            tasks_total,
                            active_runs,
                            done_tx,
                            history,
                            scheduler,
                            &mode_str,
                        )
                        .await;
                    }
                    None => app.push_system(
                        "nothing to retry — no previous request in this session",
                        NoticeLevel::Warning,
                    ),
                }
                return Directive::Continue;
            }
            if line.starts_with('/') {
                dispatch_slash(
                    app,
                    &line,
                    config,
                    client,
                    bridge,
                    store,
                    session_id,
                    event_tx,
                    cancel,
                    active_runs,
                )
                .await
            } else {
                let mode_str = app.mode.clone();
                *last_prompt = Some(line.clone());
                start_task(
                    app,
                    &line,
                    config,
                    client,
                    bridge,
                    session_id,
                    event_tx,
                    cancel,
                    tasks_total,
                    active_runs,
                    done_tx,
                    history,
                    scheduler,
                    &mode_str,
                )
                .await
            }
        }
    }
}

/// Dispatch a slash command inside the session.
#[allow(clippy::too_many_arguments)]
async fn dispatch_slash(
    app: &mut App,
    line: &str,
    config: &Config,
    client: &Arc<requiem_model::client::ModelClient>,
    bridge: &ToolBridge,
    _store: &requiem_memory::session::Session,
    session_id: Uuid,
    event_tx: &EventTx,
    cancel: &CancelFlag,
    active_runs: &[(requiem_core::Id, tokio::task::JoinHandle<()>, CancelFlag)],
) -> Directive {
    let trimmed = line.trim();
    let mut parts = trimmed[1..].split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let rest = parts.collect::<Vec<_>>().join(" ");

    // Validate arguments against the registry.
    if !rest.is_empty() {
        if slash::lookup(cmd).is_none() {
            unknown_command(app, cmd);
            return Directive::Continue;
        }
        if !slash::takes_value_arg(cmd) && !slash::accepts_arg(cmd, &rest) {
            let hints = slash::suggest_args(cmd);
            let hint = if hints.is_empty() {
                "/".to_string() + cmd + " takes no arguments"
            } else {
                format!("/{} accepts: {}", cmd, hints.join(" | "))
            };
            app.push_system(&hint, NoticeLevel::Warning);
            return Directive::Continue;
        }
    }

    match cmd {
        // Modes.
        "ask" | "plan" | "code" | "auto" | "debug" | "research" => {
            let mode = Mode::from_slash(&format!("/{cmd}")).unwrap_or(Mode::Code);
            bridge.set_mode(mode).await;
            app.set_mode(cmd);
            app.push_system(
                &format!("mode: {} — {}", cmd, mode.description()),
                NoticeLevel::Success,
            );
            Directive::Continue
        }
        "stop" => {
            if !active_runs.is_empty() {
                for (_, _, flag) in active_runs.iter() {
                    flag.cancel();
                }
                cancel.cancel();
                app.push_system("stopping…", NoticeLevel::Warning);
            } else {
                app.push_system("nothing is running", NoticeLevel::Info);
            }
            Directive::Continue
        }
        "retry" => {
            let client = client.clone();
            let tx = event_tx.clone();
            tokio::spawn(async move {
                match client.test_connection().await {
                    Ok(model) => {
                        let _ = tx.send(
                            Event::Notice {
                                text: format!("retry OK — model reachable: {model}"),
                                level: NoticeLevel::Success,
                            }
                            .stamped()
                            .event,
                        );
                    }
                    Err(e) => {
                        let _ = tx.send(
                            Event::Notice {
                                text: format!(
                                    "retry failed: [{}] {e} — check /diagnostics",
                                    e.class()
                                ),
                                level: NoticeLevel::Error,
                            }
                            .stamped()
                            .event,
                        );
                    }
                }
            });
            Directive::Continue
        }
        "diagnostics" => {
            let in_flight = client.in_flight();
            let can_admit = client.can_admit();
            let max_ctx = client
                .max_context_tokens()
                .map(|t| t.to_string())
                .unwrap_or_else(|| "?".into());
            let client_info = format!(
                "model {} @ {} — in-flight {in_flight} · admitting: {} · max-context {max_ctx}",
                client.model(),
                client.endpoint(),
                can_admit,
            );
            app.push_system(&client_info, NoticeLevel::Info);
            Directive::Continue
        }
        "dashboard" => {
            app.dash.open = true;
            Directive::Continue
        }
        "agents" => {
            let dash = app.dashboard_snapshot();
            if dash.agents.is_empty() {
                app.push_system("no agents yet — submit a task", NoticeLevel::Info);
            } else {
                let mut out = String::from("agents:");
                for a in &dash.agents {
                    out.push_str(&format!(
                        "\n  {} ({}) — {} · turns {}/{} · tok {}",
                        a.agent_id,
                        a.role,
                        a.status,
                        a.turns_used,
                        a.turns_max,
                        a.tokens_in + a.tokens_out
                    ));
                    if !a.current_task.is_empty() {
                        out.push_str(&format!(" · {}", a.current_task));
                    }
                }
                app.push_system(&out, NoticeLevel::Info);
            }
            Directive::Continue
        }
        "tasks" => {
            let dash = app.dashboard_snapshot();
            if dash.tasks.is_empty() {
                app.push_system("no tasks yet", NoticeLevel::Info);
            } else {
                let mut out = String::from("tasks:");
                for t in &dash.tasks {
                    out.push_str(&format!(
                        "\n  {} — {} — {}",
                        t.id,
                        t.status.label(),
                        t.description
                    ));
                }
                app.push_system(&out, NoticeLevel::Info);
            }
            Directive::Continue
        }
        "status" => {
            let dash = app.dashboard_snapshot();
            let (done, total) = dash.task_progress();
            let ep = client.endpoint();
            let key_state = if config.api_key.is_empty() {
                "(key from REQUIEM_API_KEY or none)"
            } else {
                "(key set, hidden)"
            };
            app.push_system(
                &format!(
                    "session {} — mode /{} — model {} @ {ep} {key_state} — tasks {done}/{total} — tok {} in / {} out",
                    session_id,
                    app.mode,
                    client.model(),
                    fmt_u32_tokens(dash.tokens_in_total),
                    fmt_u32_tokens(dash.tokens_out_total),
                ),
                NoticeLevel::Info,
            );
            Directive::Continue
        }
        "events" => {
            let dash = app.dashboard_snapshot();
            let skip = dash.events.len().saturating_sub(10);
            if dash.events.len() <= skip {
                app.push_system("no events yet", NoticeLevel::Info);
            } else {
                let mut out = String::from("recent events:");
                for le in &dash.events[skip..] {
                    let s = le.event.summary();
                    if !s.is_empty() {
                        out.push_str(&format!("\n  +{}s {}", le.rel_secs, s));
                    }
                }
                app.push_system(&out, NoticeLevel::Info);
            }
            Directive::Continue
        }
        "model" => {
            let client = client.clone();
            let tx = event_tx.clone();
            let test = rest.trim().eq_ignore_ascii_case("--test");
            tokio::spawn(async move {
                if test {
                    match client.test_connection().await {
                        Ok(model) => {
                            let _ = tx.send(
                                Event::Notice {
                                    text: format!("model reachable: {model}"),
                                    level: NoticeLevel::Success,
                                }
                                .stamped()
                                .event,
                            );
                        }
                        Err(e) => {
                            let _ = tx.send(
                                Event::Notice {
                                    text: format!("model unreachable: {e}"),
                                    level: NoticeLevel::Error,
                                }
                                .stamped()
                                .event,
                            );
                        }
                    }
                } else {
                    let _ = tx.send(
                        Event::Notice {
                            text: format!("model: {} @ {}", client.model(), client.endpoint()),
                            level: NoticeLevel::Info,
                        }
                        .stamped()
                        .event,
                    );
                }
            });
            Directive::Continue
        }
        "theme" => {
            if rest.is_empty() {
                let mut out = String::from("themes (use /theme <name>):");
                for name in THEME_NAMES {
                    let mark = if *name == config.theme {
                        " ✓"
                    } else {
                        ""
                    };
                    out.push_str(&format!("\n  {name}{mark}"));
                }
                app.push_system(&out, NoticeLevel::Info);
            } else if THEME_NAMES.contains(&rest.as_str()) {
                // Persist the choice so future sessions inherit it.
                let mut cfg = config.clone();
                cfg.theme = rest.clone();
                if let Err(e) = cfg.save() {
                    app.push_system(&format!("could not save theme: {e}"), NoticeLevel::Error);
                }
                app.theme = theme_from_name(&rest);
                app.push_system(&format!("theme → {rest} (saved)"), NoticeLevel::Success);
            } else {
                app.push_system(
                    &format!(
                        "unknown theme '{rest}' — options: {}",
                        THEME_NAMES.join(", ")
                    ),
                    NoticeLevel::Warning,
                );
            }
            Directive::Continue
        }
        "setup" => Directive::RunSetup,
        "vault" => {
            if config.vaults.is_empty() {
                app.push_system(
                    "no vaults configured — add one with `requiem setup`",
                    NoticeLevel::Info,
                );
            } else {
                let mut out = String::from("vaults (first = default):");
                for (i, v) in config.vaults.iter().enumerate() {
                    out.push_str(&format!(
                        "\n  {}. {v}{}",
                        i + 1,
                        if i == 0 { " ★" } else { "" }
                    ));
                }
                app.push_system(&out, NoticeLevel::Info);
            }
            Directive::Continue
        }
        "permissions" => {
            let mode = Mode::from_slash(&format!("/{}", app.mode)).unwrap_or(Mode::Code);
            let gate = PermissionGate::for_mode(mode);
            let p = gate.profile();
            app.push_system(
                &format!(
                    "mode /{} — file write: {:?} · terminal: {:?} · git: {:?} · vault: {:?} · agents: {:?}",
                    app.mode, p.file_write, p.terminal, p.git_mutation, p.vault_write, p.agent_spawn
                ),
                NoticeLevel::Info,
            );
            Directive::Continue
        }
        "budget" => {
            let dash = app.dashboard_snapshot();
            app.push_system(
                &format!(
                    "budget — token budget per task: {} · max tasks per session: {} · session tokens: {} in / {} out · tasks used: {}",
                    config.token_budget,
                    config.max_task_budget,
                    fmt_u32_tokens(dash.tokens_in_total),
                    fmt_u32_tokens(dash.tokens_out_total),
                    dash.tasks.len(),
                ),
                NoticeLevel::Info,
            );
            Directive::Continue
        }
        "sessions" => {
            // Reads go through the store the driver owns; open a fresh
            // connection for the query to avoid crossing awaits.
            let path = session_db_path();
            match path
                .ok()
                .and_then(|p| requiem_memory::session::Session::open(p).ok())
            {
                Some(s) => match s.list_sessions() {
                    Ok(list) if list.is_empty() => {
                        app.push_system("no past sessions", NoticeLevel::Info);
                    }
                    Ok(list) => {
                        let mut out = String::from("recent sessions (resume with --resume <id>):");
                        for (id, count, last) in list.iter().take(8) {
                            out.push_str(&format!("\n  {id} — {count} tasks — {last}"));
                        }
                        app.push_system(&out, NoticeLevel::Info);
                    }
                    Err(e) => app
                        .push_system(&format!("could not list sessions: {e}"), NoticeLevel::Error),
                },
                None => app.push_system("could not open the session database", NoticeLevel::Error),
            }
            Directive::Continue
        }
        "resume" => {
            app.push_system(
                "to resume a session, exit and run: requiem session --resume <id>",
                NoticeLevel::Info,
            );
            Directive::Continue
        }
        "brain" => {
            if rest.is_empty() {
                app.push_system("usage: /brain <query>", NoticeLevel::Warning);
                return Directive::Continue;
            }
            search_brain(app, config, &rest).await;
            Directive::Continue
        }
        "help" => {
            let mut out = String::from("commands:");
            for (group, cmds) in slash::list_commands_grouped() {
                out.push_str(&format!("\n  {group}"));
                for (name, desc) in cmds {
                    out.push_str(&format!("\n    /{name:<11} {desc}"));
                }
            }
            out.push_str("\n  (any other text runs as a task)");
            app.push_system(&out, NoticeLevel::Info);
            Directive::Continue
        }
        "version" => {
            let mode = requiem_tui::color::color_mode();
            let mode_str = match mode {
                requiem_tui::color::ColorMode::NoColor => "no-color",
                requiem_tui::color::ColorMode::Ansi16 => "ansi16",
                requiem_tui::color::ColorMode::Ansi256 => "ansi256",
                requiem_tui::color::ColorMode::TrueColor => "truecolor",
            };
            let out = format!(
                "REQUIEM v0.1.0 (commit {})\n\
                 binary: {}\n\
                 theme: {}  ·  colour-mode: {}\n\
                 model: {}",
                env!("CARGO_PKG_VERSION"),
                std::env::args().next().unwrap_or_default(),
                config.theme,
                mode_str,
                client.model(),
            );
            app.push_system(&out, NoticeLevel::Info);
            Directive::Continue
        }
        "exit" | "quit" => Directive::Exit,
        other => {
            unknown_command(app, other);
            Directive::Continue
        }
    }
}

fn unknown_command(app: &mut App, cmd: &str) {
    let suggestions = slash::suggest_commands(cmd);
    if suggestions.is_empty() {
        app.push_system(
            &format!("unknown command /{cmd} — type /help for the list"),
            NoticeLevel::Warning,
        );
    } else {
        let list: Vec<String> = suggestions.iter().map(|s| format!("/{s}")).collect();
        app.push_system(
            &format!("unknown command /{cmd} — did you mean {}?", list.join(", ")),
            NoticeLevel::Warning,
        );
    }
}

/// Search the default vault with the grep tool and surface top hits.
async fn search_brain(app: &mut App, config: &Config, query: &str) {
    let Some(vault) = config.vaults.first() else {
        app.push_system(
            "no vault configured — add one with `requiem setup`",
            NoticeLevel::Warning,
        );
        return;
    };
    let tool = GrepSearchTool;
    let args = serde_json::json!({
        "pattern": query,
        "directory": vault,
    });
    match tool.execute(&args).await {
        Ok(result) => {
            let count = result["count"].as_u64().unwrap_or(0);
            if count == 0 {
                app.push_system(
                    &format!("no notes match '{query}' in {vault}"),
                    NoticeLevel::Info,
                );
            } else {
                let mut out = format!("brain hits for '{query}' ({count}):");
                if let Some(matches) = result["matches"].as_array() {
                    for m in matches.iter().take(5) {
                        let path = m["path"].as_str().unwrap_or("");
                        let line_no = m["line"].as_u64().unwrap_or(0);
                        let text = m["text"].as_str().unwrap_or("");
                        let text: String = text.trim().chars().take(60).collect();
                        out.push_str(&format!("\n  {path}:{line_no} — {text}"));
                    }
                }
                app.push_system(&out, NoticeLevel::Success);
            }
        }
        Err(e) => app.push_system(&format!("brain search failed: {e}"), NoticeLevel::Error),
    }
}

/// Register the subagent spawn tools on the shared bridge.
///
/// Every run reuses the same `spawn_subagent` / `spawn_subagents`
/// instances; they bind to the shared `subagent_cancel` flag so `/stop`
/// and Ctrl-C can cancel children across concurrent runs.
async fn register_subagent_tools(
    bridge: &ToolBridge,
    client: &Arc<requiem_model::client::ModelClient>,
    event_tx: &EventTx,
    subagent_cancel: &CancelFlag,
    max_depth: u32,
) {
    let parent = requiem_core::agent::Agent::master("master", requiem_core::Id::new());
    let child_executor = bridge.share();
    let child_tools: Vec<requiem_model::request::ToolDef> = {
        let names: Vec<String> = bridge
            .tools
            .lock()
            .unwrap()
            .keys()
            .filter(|n| n.as_str() != "spawn_subagent" && n.as_str() != "spawn_subagents")
            .cloned()
            .collect();
        names
            .iter()
            .filter_map(|n| bridge.tools.lock().unwrap().get(n).map(|t| t.tool_def()))
            .collect()
    };
    let sub_tool = requiem_tools::make_subagent_tool(
        client.clone(),
        child_executor.clone(),
        parent.clone(),
        parent.task_session.clone(),
        child_tools.clone(),
    )
    .with_max_depth(max_depth)
    .with_events(event_tx.clone())
    .with_cancel(subagent_cancel.clone());
    bridge.add_tool(std::sync::Arc::new(sub_tool));

    let parallel_tool = requiem_tools::make_parallel_subagent_tool(
        client.clone(),
        child_executor,
        parent.clone(),
        parent.task_session.clone(),
        child_tools,
    )
    .with_max_depth(max_depth)
    .with_events(event_tx.clone())
    .with_cancel(subagent_cancel.clone());
    bridge.add_tool(std::sync::Arc::new(parallel_tool));
}

/// Spawn a task run on the runtime.
#[allow(clippy::too_many_arguments)]
async fn start_task(
    app: &mut App,
    line: &str,
    config: &Config,
    client: &Arc<requiem_model::client::ModelClient>,
    bridge: &ToolBridge,
    session_id: Uuid,
    event_tx: &EventTx,
    _cancel: &CancelFlag,
    tasks_total: &mut usize,
    active_runs: &mut Vec<(requiem_core::Id, tokio::task::JoinHandle<()>, CancelFlag)>,
    done_tx: &tokio::sync::mpsc::UnboundedSender<AgentRun>,
    _history: &mut InputHistory,
    scheduler: &SharedScheduler,
    mode: &str,
) -> Directive {
    // Use the scheduler's running task count as the concurrency gate.
    {
        let sched = scheduler.lock().unwrap();
        let max = sched.max_agents();
        if sched.running_task_count() >= max {
            app.push_system(
                &format!("max concurrency reached ({max}) — wait for a task to finish",),
                NoticeLevel::Warning,
            );
            return Directive::Continue;
        }
    }
    if *tasks_total >= config.max_task_budget as usize {
        app.push_system(
            &format!(
                "task budget reached ({}) — raise it with /budget or `requiem setup`",
                config.max_task_budget
            ),
            NoticeLevel::Error,
        );
        return Directive::Continue;
    }

    app.push_user_line(line);
    *tasks_total += 1;

    let tools = bridge.tool_defs().await;
    let agent = requiem_core::agent::Agent::master(
        "master",
        requiem_core::Id::from(session_id.to_string()),
    );
    let task = requiem_core::task::Task::new(line);
    let task_id = task.id.clone();

    // Register the task in the scheduler's graph, claim it for the
    // stable master agent, and mark it running. The agent descriptor is
    // re-registered on every run so poll_available treats its task as
    // held while it's in flight.
    {
        let mut sched = scheduler.lock().unwrap();
        sched.add_task(task.clone());
        let agent_id = AgentId("master".into());
        let mut agent = requiem_core::agent::Agent::master_with_id(
            agent_id.clone(),
            "master",
            requiem_core::Id::from(session_id.to_string()),
        );
        agent.assign_task(task_id.clone());
        agent.start();
        sched.registry.register(agent);
        let _ = sched.claim(&task_id, &agent_id);
        sched.start_task(&task_id);
    }

    let mut run = AgentRun::with_mode(agent, task, Some(mode));
    let client = client.clone();
    let executor = bridge.share();
    let tx = event_tx.clone();
    let flag = CancelFlag::new();
    let flag_for_run = flag.clone();
    let done_tx_owned = done_tx.clone();

    let handle = tokio::spawn(async move {
        let _ = run_agent_halted(
            &client,
            &executor,
            &mut run,
            &tools,
            Some(&tx),
            Some(&flag_for_run),
        )
        .await;
        let _ = done_tx_owned.send(run);
    });
    active_runs.push((task_id, handle, flag));
    Directive::Continue
}

/// Persist a finished run's outcome as a session record. Mirrors the
/// plain-mode record layout so `/resume` sees both paths identically.
fn persist_run(store: &requiem_memory::session::Session, session_id: Uuid, run: &AgentRun) {
    let resp_text = run
        .final_response
        .as_ref()
        .and_then(|r| r.text())
        .map(|s| s.to_string());
    let rec = requiem_memory::session::SessionRecord {
        session_id,
        task_description: run.task.description.clone(),
        role: run.agent.role.to_string(),
        final_response_json: resp_text,
        tokens_input: run.token_totals.input as u32,
        tokens_output: run.token_totals.output as u32,
        success: run.final_response.is_some(),
        timestamp_rfc3339: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(e) = store.insert_record(&rec) {
        // Persistence is best-effort; the session must not die over it. Log
        // it — printing to stderr would corrupt the alternate screen.
        tracing::warn!("session record save failed: {e}");
    }
}

/// Session exit summary.
fn final_summary(
    store: &requiem_memory::session::Session,
    session_id: Uuid,
    tasks_total: usize,
    tasks_ok: usize,
) -> String {
    let (tin, tout) = store.session_token_totals(&session_id).unwrap_or((0, 0));
    format!(
        "session {session_id} closed — {tasks_ok}/{tasks_total} tasks succeeded — {tin} in / {tout} out tokens"
    )
}

/// Human token count for plain text output.
fn fmt_u32_tokens(n: u32) -> String {
    requiem_tui::app::fmt_tokens(n)
}

// ── Plain (non-TTY) mode ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
#[allow(clippy::while_let_loop)]
async fn run_plain(
    config: &Config,
    theme: Theme,
    client: Arc<requiem_model::client::ModelClient>,
    bridge: ToolBridge,
    event_tx: EventTx,
    store: &requiem_memory::session::Session,
    session_id: Uuid,
    prior: Vec<requiem_memory::session::SessionRecord>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let _renderer = requiem_tui::ThemedRenderer::new(&theme);
    println!("{}", requiem_tui::banner::render_themed(&theme));
    println!(
        "{}",
        theme.secondary(format!(
            "session {session_id} — mode /{} — model {} — plain input mode (no TTY)",
            config.default_mode,
            client.model()
        ))
    );
    for rec in &prior {
        let mark = if rec.success { "✓" } else { "✗" };
        println!("  {mark} (resumed) {}", rec.task_description);
    }
    println!(
        "{}",
        theme.muted("commands: /help · submit any other line as a task")
    );

    let mut history = InputHistory::load_with_theme(theme);
    let cancel = CancelFlag::new();
    let mut tasks_total: usize = prior.len();
    let mut tasks_ok: usize = prior.iter().filter(|r| r.success).count();
    let scheduler: SharedScheduler =
        Arc::new(std::sync::Mutex::new(Scheduler::new(SchedulerConfig {
            max_agents: config.max_concurrency as usize,
            ..Default::default()
        })));

    loop {
        let Some(line) = history.read_line() else {
            break;
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('/') {
            let rest_stripped = line.strip_prefix('/').unwrap();
            let mut parts = rest_stripped.split_whitespace();
            let cmd = parts.next().unwrap_or("").to_string();
            match cmd.as_str() {
                "exit" | "quit" => break,
                "help" => {
                    for (group, cmds) in slash::list_commands_grouped() {
                        println!("  {group}");
                        for (name, desc) in cmds {
                            println!("    /{name:<11} {desc}");
                        }
                    }
                }
                "stop" => {
                    cancel.cancel();
                    println!("  stop requested");
                }
                "status" => {
                    println!(
                        "  mode /{} · model {} @ {} · tasks {tasks_total}",
                        config.default_mode,
                        client.model(),
                        client.endpoint()
                    );
                }
                "vault" => {
                    for (i, v) in config.vaults.iter().enumerate() {
                        println!("  {}. {v}", i + 1);
                    }
                    if config.vaults.is_empty() {
                        println!("  (no vaults configured)");
                    }
                }
                "model" => match client.test_connection().await {
                    Ok(m) => println!("  model reachable: {m}"),
                    Err(e) => println!("  model unreachable: {e}"),
                },
                "retry" => match client.test_connection().await {
                    Ok(m) => println!("  retry OK — model reachable: {m}"),
                    Err(e) => println!("  retry failed: [{}] {e}", e.class()),
                },
                "diagnostics" => {
                    println!(
                        "  model {} @ {} — in-flight {} · max-context {}",
                        client.model(),
                        client.endpoint(),
                        client.in_flight(),
                        client.max_context_tokens().unwrap_or(0)
                    );
                }
                other => {
                    println!("  /{other} needs a terminal UI — run `requiem session` in a TTY");
                }
            }
            continue;
        }

        // Task run in plain mode: spawn, drain events as text lines.
        if tasks_total >= config.max_task_budget as usize {
            println!(
                "  task budget reached ({}) — not starting a new task",
                config.max_task_budget
            );
            continue;
        }
        cancel.reset();
        tasks_total += 1;
        println!(
            "  ▸ task #{tasks_total} — {} — running…",
            Mode::from_slash(&format!("/{}", config.default_mode))
                .unwrap_or(Mode::Code)
                .slash()
        );

        // Subagent tool for this run: give the child the full tool set
        // (minus spawn_subagent) so it can actually do work.
        if bridge.is_tool_allowed("spawn_subagent").await {
            let parent = requiem_core::agent::Agent::master(
                "master",
                requiem_core::Id::from(session_id.to_string()),
            );
            let child_tools: Vec<requiem_model::request::ToolDef> = {
                let names: Vec<String> = bridge
                    .tools
                    .lock()
                    .unwrap()
                    .keys()
                    .filter(|n| n.as_str() != "spawn_subagent" && n.as_str() != "spawn_subagents")
                    .cloned()
                    .collect();
                names
                    .iter()
                    .filter_map(|n| bridge.tools.lock().unwrap().get(n).map(|t| t.tool_def()))
                    .collect()
            };
            let sub_tool = requiem_tools::make_subagent_tool(
                client.clone(),
                bridge.share(),
                parent.clone(),
                requiem_core::Id::from(session_id.to_string()),
                child_tools.clone(),
            )
            .with_max_depth(config.max_agent_depth)
            .with_events(event_tx.clone())
            .with_cancel(cancel.clone());
            bridge.add_tool(std::sync::Arc::new(sub_tool));

            let parallel_tool = requiem_tools::make_parallel_subagent_tool(
                client.clone(),
                bridge.share(),
                parent,
                requiem_core::Id::from(session_id.to_string()),
                child_tools,
            )
            .with_max_depth(config.max_agent_depth)
            .with_events(event_tx.clone())
            .with_cancel(cancel.clone());
            bridge.add_tool(std::sync::Arc::new(parallel_tool));
        }

        let tools = bridge.tool_defs().await;
        let agent = requiem_core::agent::Agent::master(
            "master",
            requiem_core::Id::from(session_id.to_string()),
        );
        let task = requiem_core::task::Task::new(&line);
        let task_id = task.id.clone();
        {
            let mut sched = scheduler.lock().unwrap();
            sched.add_task(task.clone());
            let agent_id = requiem_core::agent::AgentId("master".into());
            let _ = sched.claim(&task_id, &agent_id);
            sched.start_task(&task_id);
        }
        let mut run = AgentRun::with_mode(agent, task, Some(config.default_mode.as_str()));

        let mut event_rx = event_tx.subscribe();
        let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<(
            AgentRun,
            Result<(), requiem_core::error::RequiemError>,
        )>();
        {
            let client = client.clone();
            let executor = bridge.share();
            let tx = event_tx.clone();
            let flag = cancel.clone();
            tokio::spawn(async move {
                let result =
                    run_agent_halted(&client, &executor, &mut run, &tools, Some(&tx), Some(&flag))
                        .await;
                let _ = done_tx.send((run, result));
            });
        }

        let mut final_run: Option<AgentRun> = None;
        let mut final_err: Option<requiem_core::error::RequiemError> = None;
        loop {
            tokio::select! {
                ev = event_rx.recv() => {
                    match ev {
                        Ok(e) => {
                            let s = e.summary();
                            if !s.is_empty() {
                                println!("  · {s}");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            println!("  … (UI lagged, {n} events dropped)");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                done = done_rx.recv() => {
                    match done {
                        Some((run, result)) => {
                            // Drain any trailing events.
                            while let Ok(e) = event_rx.try_recv() {
                                let s = e.summary();
                                if !s.is_empty() {
                                    println!("  · {s}");
                                }
                            }
                            final_run = Some(run);
                            match result {
                                Ok(()) => {
                                    tasks_ok += 1;
                                    let mut sched = scheduler.lock().unwrap();
                                    sched.complete_task(&task_id, serde_json::Value::Null);
                                }
                                Err(e) => {
                                    final_err = Some(e);
                                    let mut sched = scheduler.lock().unwrap();
                                    sched.fail_task(&task_id, "run failed");
                                }
                            }
                            break;
                        }
                        None => {
                            // The worker vanished without reporting (panic):
                            // never leave the scheduler task stuck in Running.
                            let mut sched = scheduler.lock().unwrap();
                            sched.fail_task(&task_id, "worker task panicked");
                            println!("  ✗ failed: internal worker error");
                            break;
                        }
                    }
                }
            }
        }

        // Record + print the outcome.
        if let Some(run) = final_run {
            let usage = run.token_totals;
            let resp_text = run
                .final_response
                .as_ref()
                .and_then(|r| r.text())
                .map(|s| s.to_string());
            let rec = requiem_memory::session::SessionRecord {
                session_id,
                task_description: line.clone(),
                role: "master".into(),
                final_response_json: resp_text.clone(),
                tokens_input: usage.input as u32,
                tokens_output: usage.output as u32,
                success: final_err.is_none(),
                timestamp_rfc3339: chrono::Utc::now().to_rfc3339(),
            };
            let _ = store.insert_record(&rec);
            match (&final_err, resp_text) {
                (None, Some(text)) => println!("{}", theme.primary(&text)),
                (None, None) => println!("  ✓ completed (no text response)"),
                (Some(e), _) => println!("  ✗ failed: {e}"),
            }
            println!(
                "  tokens: {} in / {} out",
                fmt_u32_tokens(usage.input as u32),
                fmt_u32_tokens(usage.output as u32)
            );
        }
    }

    history.save();
    Ok(final_summary(store, session_id, tasks_total, tasks_ok))
}
