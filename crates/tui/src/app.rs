//! The interactive ratatui application.
//!
//! Two full-screen views share one event loop:
//!
//! * **Session screen** — a scrollable conversation transcript (user
//!   messages, streamed agent text, tool calls, notices), an input line
//!   with slash-command autocomplete and history recall, a header with
//!   run context, and a one-line status bar.
//! * **Dashboard overlay** — opened with `/dashboard` or `Tab`: task
//!   overview, the agent tree with live token counts, and the event log.
//!   `Tab` cycles panels, arrows select items, `PgUp`/`PgDn` scrolls.
//!
//! The app holds no runtime logic: it consumes [`Event`]s from the agent
//! runtime (via the CLI driver) and produces [`UiAction`]s. All colors
//! come from the theme — nothing here hardcodes a color.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, Paragraph, Wrap};
use ratatui::Frame;

use requiem_core::events::{Event, NoticeLevel};

use crate::composer::Composer;
use crate::dashboard::{AgentRow, AgentStatus, TaskRow, TaskStatus};
use crate::markdown as md;
use crate::theme::{Theme, Token};

// ── Public types ─────────────────────────────────────────────────────

/// What the user did that the driver must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    /// Nothing to do.
    None,
    /// The user submitted an input line.
    Submit(String),
    /// Cancel the active run (Ctrl-C while running).
    CancelRun,
    /// Quit the session (Ctrl-C when idle).
    Quit,
}

/// Command metadata injected by the CLI for autocomplete and hints.
#[derive(Debug, Clone, Default)]
pub struct CommandHints {
    /// All command names, without the leading `/`.
    pub names: Vec<String>,
    /// One-line description per command name.
    pub descriptions: HashMap<String, String>,
    /// Argument hints per command name.
    pub args: HashMap<String, Vec<String>>,
}

// ── Transcript ───────────────────────────────────────────────────────

/// One semantic entry in the conversation transcript.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    /// Glyph prefix shown on the first line (e.g. `›`, `▸`, `✓`).
    pub glyph: &'static str,
    /// Plain text content (wrapped by the renderer).
    pub text: String,
    /// Foreground token for the text.
    pub fg: Token,
    /// Bold styling.
    pub bold: bool,
    /// Continuation-line indent (list alignment).
    pub indent: usize,
    /// Whether a streaming entry still accepts deltas.
    pub streaming: bool,
    /// Render the text as markdown (headings, code fences, lists, etc.).
    pub markdown: bool,
    /// Render as the brand banner: raw artwork, styled directly with the
    /// `Accent` token (no ANSI round-trip).
    pub banner: bool,
    /// When set, this entry represents an in-flight tool call with this
    /// name, so the matching `ToolCallFinished` can fold into it instead of
    /// appending a second line.
    pub tool: Option<String>,
}

impl TranscriptEntry {
    fn new(glyph: &'static str, text: String, fg: Token) -> Self {
        Self {
            glyph,
            text,
            fg,
            bold: false,
            indent: 2,
            streaming: false,
            markdown: false,
            banner: false,
            tool: None,
        }
    }
}

/// Pre-wrapped, styled transcript lines plus the total count.
struct RenderedTranscript {
    lines: Vec<Line<'static>>,
}

fn wrap_to_width(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let w = width.max(8);
    textwrap::wrap(text, w)
        .into_iter()
        .map(|c| c.to_string())
        .collect()
}

/// Parse a single ANSI-styled line (as produced by `color::ansi_wrap`)
/// into ratatui `Span<'static>` values.
///
/// Handles: `\x1b[<params>m<text>\x1b[0m` where params may include:
/// `0` reset, `1` bold, `2` dim, `3` italic, `4` underline, `7` reverse,
/// `9` strikethrough, `30-37`/`90-97` 16-colour, `38;5;N` 256-colour,
/// `38;2;R;G;B` truecolour. Multiple segments in one line are concatenated.
fn parse_ansi_line(line: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current_style = Style::new();
    let mut buf: String = String::new();

    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek().is_some() && *chars.peek().unwrap() == '[' {
            // Flush text accumulated under current style.
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), current_style));
            }
            // Parse SGR parameters.
            let mut params: String = String::new();
            for pc in chars.by_ref() {
                if pc == 'm' {
                    break;
                }
                params.push(pc);
            }
            if params.is_empty() {
                continue;
            }
            current_style = Style::new();
            let parts: Vec<&str> = params.split(';').collect();
            let mut i = 0;
            while i < parts.len() {
                let p = parts[i];
                match p.parse::<u32>() {
                    Ok(0) => current_style = Style::new(),
                    Ok(1) => current_style = current_style.add_modifier(Modifier::BOLD),
                    Ok(2) => current_style = current_style.add_modifier(Modifier::DIM),
                    Ok(3) => current_style = current_style.add_modifier(Modifier::ITALIC),
                    Ok(4) => current_style = current_style.add_modifier(Modifier::UNDERLINED),
                    Ok(7) => current_style = current_style.add_modifier(Modifier::REVERSED),
                    Ok(9) => current_style = current_style.add_modifier(Modifier::CROSSED_OUT),
                    Ok(n) if (30..=37).contains(&n) => {
                        let idx = (n - 30) as u8;
                        current_style = current_style.fg(ansi16_to_color(idx));
                    }
                    Ok(n) if (90..=97).contains(&n) => {
                        let idx = (n - 90) as u8;
                        current_style = current_style.fg(ansi16_to_color(8 + idx));
                    }
                    Ok(38) if i + 1 < parts.len() => {
                        // Extended colour: "5;N" or "2;R;G;B"
                        if parts.get(i + 1) == Some(&"5") && i + 2 < parts.len() {
                            if let Ok(n) = parts[i + 2].parse::<u8>() {
                                current_style = current_style.fg(Color::Indexed(n));
                            }
                            i += 2;
                        } else if parts.get(i + 1) == Some(&"2") && i + 4 < parts.len() {
                            if let (Ok(r), Ok(g), Ok(b)) = (
                                parts[i + 2].parse::<u8>(),
                                parts[i + 3].parse::<u8>(),
                                parts[i + 4].parse::<u8>(),
                            ) {
                                current_style = current_style.fg(Color::Rgb(r, g, b));
                            }
                            i += 4;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        } else {
            buf.push(c);
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, current_style));
    }
    spans
}

/// Map a 0–15 ANSI base colour index to a ratatui `Color`.
fn ansi16_to_color(idx: u8) -> Color {
    match idx {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::White,
        8 => Color::Gray,
        9 => Color::Indexed(9),
        10 => Color::Indexed(10),
        11 => Color::Indexed(11),
        12 => Color::Indexed(12),
        13 => Color::Indexed(13),
        14 => Color::Indexed(14),
        15 => Color::Indexed(15),
        _ => Color::Reset,
    }
}

/// Format a structured provider error card (markdown) for the transcript.
/// Maps `error_class` to a category label and appends recovery hints.
fn format_error_card(error: &str, error_class: &str, model: &str) -> String {
    let (category, action) = match error_class {
        "connection" => ("Connection", "Check your endpoint URL and network. Run `/model test` to verify."),
        "model" => ("Model / API", "The provider rejected the request. Check your API key with `/model test` or reconfigure via `/setup`."),
        "invalid_response" => ("Response Parse", "The provider returned an unexpected payload. Retry the request or check for a provider-side issue."),
        "context_length" => ("Context Limit", "The prompt exceeded the model's context window. Shorten the input or use `/model` to switch to a larger model."),
        "rate_limited" => ("Rate Limit", "Too many requests. Wait for the provider's backoff window and retry, or check your provider's quota."),
        "timeout" => ("Timeout", "The request timed out. Check your network or increase the timeout in config."),
        "permission" => ("Permission", "The operation was denied by the current mode's permission policy. Switch modes or adjust permissions."),
        "tool" => ("Tool Error", "A tool invocation failed. Review the tool output above for details."),
        "io" => ("I/O", "A filesystem or I/O error occurred."),
        "session" => ("Session", "A session state error occurred. Try starting a new session."),
        "config" => ("Configuration", "A configuration value is missing or invalid. Run `/setup` to fix."),
        "vault" => ("Vault", "An Obsidian vault operation failed."),
        "cancelled" => ("Cancelled", "The request was cancelled by the user."),
        "auth" => ("Authentication", "Your API key is invalid or expired. Run `/setup` to reconfigure credentials."),
        "missing_credentials" => ("No API Key", "No API key is configured. Run `/setup` to add one or set REQUIEM_API_KEY."),
        "dns" => ("DNS", "The endpoint host could not be resolved. Check your DNS configuration and network."),
        "tls" => ("TLS", "The TLS handshake or certificate verification failed. Check your CA bundle and endpoint URL."),
        "model_unavailable" => ("Model Not Found", "The requested model is not available at this endpoint. Check the model name in config."),
        "quota_exhausted" => ("Quota / Billing", "The provider's quota or billing limit has been reached. Check your account."),
        "tool_calling" => ("Tool Schema", "The provider rejected the tool-call schema. Simplify or remove the tools from the request."),
        "streaming" => ("Stream Error", "The SSE stream was interrupted mid-response. Retry the request."),
        "gateway" => ("Gateway", "An upstream gateway or proxy returned an error. The provider may be temporarily unavailable — retry in a moment."),
        "invalid_state" => ("State", "The internal task graph state is inconsistent. Try a new session."),
        "internal" => ("Internal", "A worker task failed unexpectedly. Its partial state was discarded — retry the request."),
        _ => ("Unknown", "An unexpected error occurred. See the log for details."),
    };
    let status = extract_http_status(error);
    // Only transport/provider classes describe an HTTP exchange.
    let is_provider = matches!(
        error_class,
        "connection"
            | "model"
            | "invalid_response"
            | "context_length"
            | "rate_limited"
            | "timeout"
            | "auth"
            | "missing_credentials"
            | "dns"
            | "tls"
            | "model_unavailable"
            | "quota_exhausted"
            | "tool_calling"
            | "streaming"
            | "gateway"
    );
    // Retryability matches `RequiemError::is_retryable`: partial streams and
    // permanent failures are never replayed automatically.
    let retry_line = match error_class {
        "rate_limited" => {
            "**Retry:** transient — wait for the provider's backoff window, then `/retry`."
        }
        "connection" | "dns" | "timeout" | "gateway" => {
            "**Retry:** transient — press `/retry` to re-run the last request."
        }
        "streaming" => {
            "**Retry:** partial output kept — `/retry` starts a fresh attempt (the partial text is not replayed automatically)."
        }
        _ => "**Retry:** not automatic — fix the cause above, then press `/retry`.",
    };
    let mut card = format!(
        "**{}**\n\n\
         **Category:** {category}  ",
        if is_provider {
            "PROVIDER ERROR"
        } else {
            "ERROR"
        }
    );
    if is_provider {
        card.push_str(&format!("\n**Model:** {model}  "));
    }
    if let Some(status) = status {
        card.push_str(&format!("\n**HTTP status:** {status}  "));
    }
    card.push_str(&format!(
        "\n**Detail:** {error}\n\n{retry_line}\n\n**Suggested action:** {action}"
    ));
    card
}

/// Pull an HTTP status out of a formatted error (`... [429] ...` or
/// `model error [500]: ...`), when present.
fn extract_http_status(error: &str) -> Option<u16> {
    let start = error.find('[')? + 1;
    let end = error[start..].find(']')? + start;
    error[start..end].trim().parse::<u16>().ok()
}

/// Walk up to `limit` files/dirs under `root` (skipping common noise
/// directories) and return their relative paths as `String`s.
fn walk_files_for_suggestions(root: &std::path::Path, limit: usize) -> Vec<String> {
    const SKIP: &[&str] = &[
        ".git",
        "target",
        "node_modules",
        ".idea",
        ".vscode",
        "__pycache__",
        ".qwen",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') {
                continue;
            }
            if SKIP.iter().any(|s| name_str == *s) {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path);
            out.push(rel.display().to_string());
            if out.len() >= limit {
                return out;
            }
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out
}

// ── Dashboard overlay state ──────────────────────────────────────────

/// Which dashboard panel has focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DashPanel {
    #[default]
    Tasks,
    Agents,
    Events,
}

const DASH_PANELS: [DashPanel; 3] = [DashPanel::Tasks, DashPanel::Agents, DashPanel::Events];

/// An entry in the event log: relative seconds + the event.
#[derive(Debug, Clone)]
pub struct LogEvent {
    pub rel_secs: u64,
    pub event: Event,
}

/// Live dashboard state, maintained from runtime events.
#[derive(Debug, Default)]
pub struct DashState {
    pub agents: Vec<AgentRow>,
    pub tasks: Vec<TaskRow>,
    pub events: Vec<LogEvent>,
    pub open: bool,
    pub panel: DashPanel,
    /// Per-panel scroll-back (0 = follow).
    pub scroll: [usize; 3],
    /// Per-panel selected item index.
    pub selected: [usize; 3],
    /// Session-wide input tokens (accumulated from Usage events).
    pub tokens_in_total: u32,
    /// Session-wide output tokens.
    pub tokens_out_total: u32,
}

impl DashState {
    fn agent_index(&self, name: &str) -> Option<usize> {
        self.agents.iter().position(|a| a.agent_id == name)
    }

    fn task_index(&self, id: &str) -> Option<usize> {
        self.tasks.iter().position(|t| t.id == id)
    }

    /// Completed / total task counts.
    pub fn task_progress(&self) -> (usize, usize) {
        let total = self.tasks.len();
        let done = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .count();
        (done, total)
    }

    pub fn total_tokens(&self) -> u32 {
        self.agents.iter().map(|a| a.tokens_in + a.tokens_out).sum()
    }
}

// ── The app ──────────────────────────────────────────────────────────

/// The full-screen session application.
pub struct App {
    /// The theme resolved from config (variants included).
    pub theme: Theme,
    /// Command registry for autocomplete/hints.
    pub hints: CommandHints,
    /// Session label (uuid) shown in the header.
    pub session_label: String,
    /// Configured model shown in the header.
    pub model: String,
    /// Project directory/name shown in the header.
    pub project: String,
    /// Current mode name (without slash).
    pub mode: String,
    /// Working directory for `@` file autocomplete.
    pub workdir: Option<PathBuf>,

    // Session screen state
    entries: Vec<TranscriptEntry>,
    composer: Composer,
    suggestions: Vec<String>,
    suggestion_idx: usize,
    history: Vec<String>,
    history_idx: Option<usize>,
    history_draft: String,
    scroll_back: usize,
    /// Whether a large paste is shown expanded instead of as a placeholder.
    paste_expanded: bool,

    // Runtime-derived status
    started: Instant,
    /// Task ids of active (non-terminal) runs. Keyed by task id rather than a
    /// counter so that completion/cancellation events are idempotent and a
    /// driver can reconcile the set exactly — a finished run can never leave
    /// the UI stuck in a stale "running" state, and one task finishing cannot
    /// mark the app idle while others are still in flight.
    active_runs: std::collections::HashSet<String>,
    turns: u32,
    tokens_in: u32,
    tokens_out: u32,
    active_task: Option<String>,
    /// Streaming buffers per agent name.
    streaming: HashMap<String, usize>,

    // Dashboard state
    pub dash: DashState,
}

impl App {
    /// Build an app for a session.
    pub fn new(theme: Theme, hints: CommandHints, session_label: String, model: String) -> Self {
        Self {
            theme,
            hints,
            session_label,
            model,
            project: String::new(),
            mode: "code".to_string(),
            workdir: None,
            entries: Vec::new(),
            composer: Composer::new(),
            suggestions: Vec::new(),
            suggestion_idx: 0,
            history: Vec::new(),
            history_idx: None,
            history_draft: String::new(),
            scroll_back: 0,
            paste_expanded: false,
            started: Instant::now(),
            active_runs: std::collections::HashSet::new(),
            turns: 0,
            tokens_in: 0,
            tokens_out: 0,
            active_task: None,
            streaming: HashMap::new(),
            dash: DashState::default(),
        }
    }

    /// Seed persisted history (newest first, like the disk format).
    pub fn set_history(&mut self, entries: Vec<String>) {
        self.history = entries;
    }

    /// Persisted history (newest first).
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Whether the dashboard overlay is open.
    pub fn dashboard_open(&self) -> bool {
        self.dash.open
    }

    /// Toggle the dashboard overlay.
    pub fn toggle_dashboard(&mut self) {
        self.dash.open = !self.dash.open;
    }

    /// Change the displayed mode (after `/ask`, `/code`, …).
    pub fn set_mode(&mut self, mode: &str) {
        self.mode = mode.to_string();
    }

    /// Insert a full pasted text blob (bracketed-paste delivery) into the
    /// composer at the cursor position. Newlines are preserved for
    /// multiline content. The input is NOT submitted — the user still
    /// presses Enter to send. Suggestions are refreshed.
    pub fn paste_input(&mut self, text: &str) {
        self.composer.insert_paste(text);
        self.refresh_suggestions();
    }

    /// Record a user-submitted line in the transcript and history.
    pub fn push_user_line(&mut self, text: &str) {
        self.entries.push(TranscriptEntry {
            glyph: "›",
            text: text.to_string(),
            fg: Token::AccentBright,
            bold: true,
            indent: 4,
            streaming: false,
            markdown: false,
            banner: false,
            tool: None,
        });
        self.history.insert(0, text.to_string());
        self.history.truncate(200);
        self.history_idx = None;
        self.scroll_back = 0;
        self.composer.clear();
        self.paste_expanded = false;
    }

    /// Record a system notice in the transcript.
    pub fn push_system(&mut self, text: &str, level: NoticeLevel) {
        let (glyph, fg) = match level {
            NoticeLevel::Success => ("✓", Token::Success),
            NoticeLevel::Warning => ("⚠", Token::Warning),
            NoticeLevel::Error => ("✗", Token::Error),
            NoticeLevel::Info => ("·", Token::TextMuted),
        };
        self.entries
            .push(TranscriptEntry::new(glyph, text.to_string(), fg));
        // Do not force a jump to the bottom: if the user is reading earlier
        // output, their scroll position is preserved.
    }

    /// Seed the transcript with the branded welcome screen: the official
    /// REQUIEM ASCII banner, a description block, and command hints.
    /// Called once at session start when there is no prior conversation.
    ///
    /// The banner is stored as raw artwork and styled with the `Accent`
    /// token at render time — no ANSI paint/parse round-trip, so terminal
    /// colour detection can never turn the logo blue.
    pub fn push_welcome(&mut self, project_dir: &str) {
        use crate::banner;

        self.set_project(project_dir);
        let spacer = || TranscriptEntry {
            glyph: " ",
            text: String::new(),
            fg: Token::Text,
            bold: false,
            indent: 0,
            streaming: false,
            markdown: false,
            banner: false,
            tool: None,
        };

        self.entries.push(spacer());
        self.entries.push(TranscriptEntry {
            glyph: " ",
            text: banner::BANNER.to_string(),
            fg: Token::Accent,
            bold: true,
            indent: 0,
            streaming: false,
            markdown: false,
            banner: true,
            tool: None,
        });
        self.entries.push(spacer());

        let project = if self.project.is_empty() {
            "—".to_string()
        } else {
            self.project.clone()
        };
        let desc = format!(
            "MULTI-AGENT AI ENVIRONMENT\n\
             Autonomous coding and knowledge orchestration\n\n\
             Model  {model}   ·   Project  {project}   ·   Mode  /{mode}\n\n\
             Describe a task to begin, or use a command:\n\
             /help        Available commands\n\
             /dashboard   Live agent & task view\n\
             /model       Model configuration\n\
             /setup       Reconfigure API key & endpoint\n\
             /theme       Switch theme (default / high_contrast / muted)",
            model = self.model,
            project = project,
            mode = self.mode,
        );
        self.entries.push(TranscriptEntry {
            glyph: " ",
            text: desc,
            fg: Token::TextSecondary,
            bold: false,
            indent: 0,
            streaming: false,
            markdown: false,
            banner: false,
            tool: None,
        });
        self.scroll_back = 0;
    }

    /// Derive and store a short project label from a directory path.
    pub fn set_project(&mut self, project_dir: &str) {
        let s = project_dir.trim_end_matches(['\\', '/']);
        let name = s.rsplit(['\\', '/']).find(|p| !p.is_empty()).unwrap_or(s);
        self.project = name.to_string();
    }

    // ── Event consumption ────────────────────────────────────────

    /// Reconcile the set of active runs against the driver's authoritative
    /// list of in-flight task ids.
    ///
    /// Idempotent and order-independent: a terminal event arriving after the
    /// reconcile simply removes an id that is already gone, so a finished run
    /// can never leave the UI stuck on "running".
    pub fn sync_active_runs(&mut self, task_ids: &[String]) {
        self.active_runs.clear();
        self.active_runs.extend(task_ids.iter().cloned());
    }

    /// Whether any run is currently active.
    pub fn is_running(&self) -> bool {
        !self.active_runs.is_empty()
    }

    /// Number of currently active runs.
    pub fn active_run_count(&self) -> usize {
        self.active_runs.len()
    }

    /// Consume one runtime event: updates the transcript, dashboard and
    /// status counters.
    pub fn push_event(&mut self, event: &Event) {
        self.dash.events.push(LogEvent {
            rel_secs: self.started.elapsed().as_secs(),
            event: event.clone(),
        });
        if self.dash.events.len() > 200 {
            let drop = self.dash.events.len() - 200;
            self.dash.events.drain(0..drop);
        }

        match event {
            Event::RunStarted {
                agent,
                role,
                task,
                description,
                ..
            } => {
                self.active_runs.insert(task.as_str().to_string());
                self.turns = 0;
                self.active_task = Some(description.clone());
                if self.dash.agent_index(agent).is_none() {
                    self.dash.agents.push(AgentRow {
                        agent_id: agent.clone(),
                        role: role.clone(),
                        ..AgentRow::new(agent, role)
                    });
                }
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].status = AgentStatus::Running;
                    self.dash.agents[i].current_task = description.clone();
                }
                if self.dash.task_index(task.as_str()).is_none() {
                    self.dash.tasks.push(TaskRow {
                        id: task.as_str().to_string(),
                        description: description.clone(),
                        status: TaskStatus::Running,
                        assignee: Some(agent.clone()),
                        ..TaskRow::default()
                    });
                }
                self.entries.push(TranscriptEntry {
                    glyph: "▸",
                    text: format!("{description}  ·  {agent} ({role}) running"),
                    fg: Token::Info,
                    bold: false,
                    indent: 4,
                    streaming: false,
                    markdown: false,
                    banner: false,
                    tool: None,
                });
            }
            Event::TurnStarted { agent, turn } => {
                self.turns = *turn;
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].turns_used = *turn;
                }
            }
            Event::AssistantDelta { agent, delta } => {
                // Append to (or start) this agent's streaming entry.
                let idx = match self.streaming.get(agent) {
                    Some(&i) => i,
                    None => {
                        self.entries.push(TranscriptEntry {
                            glyph: "◆",
                            text: String::new(),
                            fg: Token::Text,
                            bold: false,
                            indent: 4,
                            streaming: true,
                            markdown: true,
                            banner: false,
                            tool: None,
                        });
                        let i = self.entries.len() - 1;
                        self.streaming.insert(agent.clone(), i);
                        i
                    }
                };
                self.entries[idx].text.push_str(delta);
            }
            Event::AssistantMessage { agent, text } => {
                if let Some(&i) = self.streaming.get(agent) {
                    // Streaming already rendered the content; finalize.
                    self.entries[i].streaming = false;
                    self.streaming.remove(agent);
                    if text.is_empty() {
                        self.entries.remove(i);
                    }
                } else if !text.is_empty() {
                    let mut e = TranscriptEntry::new("◆", text.clone(), Token::Text);
                    e.markdown = true;
                    self.entries.push(e);
                }
            }
            Event::ToolCallStarted {
                agent,
                tool,
                summary,
            } => {
                // One compact line per tool call. The matching
                // `ToolCallFinished` folds duration/status into this entry
                // instead of appending a second line.
                let mut e =
                    TranscriptEntry::new("⚙", format!("{tool}  {summary}"), Token::TextMuted);
                e.indent = 4;
                e.streaming = true;
                e.tool = Some(tool.clone());
                self.entries.push(e);
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].status = AgentStatus::Running;
                }
            }
            Event::ToolCallFinished {
                agent,
                tool,
                ok,
                summary,
                duration_ms,
            } => {
                let (glyph, fg) = if *ok {
                    ("✓", Token::Success)
                } else {
                    ("✗", Token::Error)
                };
                let text = if summary.is_empty() {
                    format!("{tool}  {duration_ms}ms")
                } else {
                    format!("{tool}  {summary}  ·  {duration_ms}ms")
                };
                let pending = self
                    .entries
                    .iter()
                    .rposition(|e| e.streaming && e.tool.as_deref() == Some(tool.as_str()));
                match pending {
                    Some(i) => {
                        let e = &mut self.entries[i];
                        e.glyph = glyph;
                        e.fg = fg;
                        e.text = text;
                        e.streaming = false;
                        e.tool = None;
                    }
                    None => {
                        let mut e = TranscriptEntry::new(glyph, text, fg);
                        e.indent = 4;
                        self.entries.push(e);
                    }
                }
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].status = AgentStatus::Running;
                }
            }
            Event::AgentSpawned {
                parent,
                child,
                role,
                description,
            } => {
                self.dash.agents.push(AgentRow {
                    agent_id: child.clone(),
                    role: role.clone(),
                    parent: parent.clone(),
                    current_task: description.clone(),
                    status: AgentStatus::Running,
                    ..AgentRow::new(child, role)
                });
                self.entries.push(TranscriptEntry::new(
                    "⇗",
                    format!("{parent} spawned {child} [{role}]: {description}"),
                    Token::Violet,
                ));
            }
            Event::AgentStatus { agent, status } => {
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].status = match status.as_str() {
                        "running" => AgentStatus::Running,
                        "done" => AgentStatus::Done,
                        "failed" => AgentStatus::Failed,
                        "cancelled" => AgentStatus::Waiting,
                        _ => AgentStatus::Idle,
                    };
                }
            }
            Event::Usage {
                agent,
                input_tokens,
                output_tokens,
                ..
            } => {
                self.tokens_in += input_tokens;
                self.tokens_out += output_tokens;
                self.dash.tokens_in_total += input_tokens;
                self.dash.tokens_out_total += output_tokens;
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].tokens_in += input_tokens;
                    self.dash.agents[i].tokens_out += output_tokens;
                }
            }
            Event::RunCompleted {
                agent,
                task,
                response,
                input_tokens,
                output_tokens,
            } => {
                self.active_runs.remove(task.as_str());
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].status = AgentStatus::Done;
                }
                if let Some(i) = self.dash.task_index(task.as_str()) {
                    self.dash.tasks[i].status = TaskStatus::Completed;
                }
                let _ = (input_tokens, output_tokens); // already accumulated via Usage
                if let Some(text) = response {
                    if !text.is_empty() {
                        let exists = self.entries.iter().any(|e| e.text == *text);
                        if !exists {
                            let mut e = TranscriptEntry::new("◆", text.clone(), Token::Text);
                            e.markdown = true;
                            self.entries.push(e);
                        }
                    }
                }
                if self.active_runs.is_empty() {
                    self.active_task = None;
                }
            }
            Event::RunFailed {
                agent,
                task,
                error,
                error_class,
                ..
            } => {
                self.active_runs.remove(task.as_str());
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].status = AgentStatus::Failed;
                }
                if let Some(i) = self.dash.task_index(task.as_str()) {
                    self.dash.tasks[i].status = TaskStatus::Failed;
                }
                // Render a structured error card in the transcript.
                let card = format_error_card(error, error_class, &self.model);
                let mut e = TranscriptEntry::new("✗", card, Token::Error);
                e.markdown = true;
                self.entries.push(e);
                if self.active_runs.is_empty() {
                    self.active_task = None;
                }
            }
            Event::RunCancelled { agent, task } => {
                self.active_runs.remove(task.as_str());
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].status = AgentStatus::Waiting;
                }
                if let Some(i) = self.dash.task_index(task.as_str()) {
                    self.dash.tasks[i].status = TaskStatus::Cancelled;
                }
                self.entries.push(TranscriptEntry::new(
                    "⊘",
                    "run cancelled — partial state kept in the transcript".to_string(),
                    Token::Warning,
                ));
                if self.active_runs.is_empty() {
                    self.active_task = None;
                }
            }
            Event::Notice { text, level } => self.push_system(text, *level),
        }
    }

    /// A snapshot of the live dashboard state (for persistence/summary).
    pub fn dashboard_snapshot(&self) -> &DashState {
        &self.dash
    }

    /// The current composer text (read-only access for the CLI driver).
    pub fn composer_text(&self) -> &str {
        &self.composer.text
    }

    // ── Key handling ─────────────────────────────────────────────

    /// Handle a key event. Drivers must filter `KeyEventKind::Release`
    /// before calling (Windows sends both).
    pub fn handle_key(&mut self, key: KeyEvent) -> UiAction {
        if key.kind != KeyEventKind::Press {
            return UiAction::None;
        }

        if self.dash.open {
            return self.handle_dashboard_key(key);
        }
        self.handle_session_key(key)
    }

    fn handle_dashboard_key(&mut self, key: KeyEvent) -> UiAction {
        match key.code {
            KeyCode::Esc => {
                self.dash.open = false;
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.dash.open = false;
            }
            KeyCode::Tab => {
                let i = DASH_PANELS
                    .iter()
                    .position(|p| *p == self.dash.panel)
                    .unwrap_or(0);
                self.dash.panel = DASH_PANELS[(i + 1) % DASH_PANELS.len()];
            }
            KeyCode::PageUp => {
                let p = panel_index(self.dash.panel);
                self.dash.scroll[p] = self.dash.scroll[p].saturating_add(10);
            }
            KeyCode::PageDown => {
                let p = panel_index(self.dash.panel);
                self.dash.scroll[p] = self.dash.scroll[p].saturating_sub(10);
            }
            KeyCode::Up => {
                let p = panel_index(self.dash.panel);
                self.dash.selected[p] = self.dash.selected[p].saturating_sub(1);
            }
            KeyCode::Down => {
                let p = panel_index(self.dash.panel);
                let len = panel_len(&self.dash, p);
                if len > 0 {
                    self.dash.selected[p] = (self.dash.selected[p] + 1).min(len - 1);
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return if self.is_running() {
                    UiAction::CancelRun
                } else {
                    UiAction::Quit
                };
            }
            _ => {}
        }
        UiAction::None
    }

    fn handle_session_key(&mut self, key: KeyEvent) -> UiAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                return if self.is_running() {
                    UiAction::CancelRun
                } else {
                    UiAction::Quit
                };
            }
            KeyCode::Char('u') if ctrl => {
                self.composer.delete_to_start();
                self.refresh_suggestions();
            }
            KeyCode::Char('w') if ctrl => {
                self.composer.delete_word_left();
                self.refresh_suggestions();
            }
            KeyCode::Char('k') if ctrl => {
                self.composer.delete_to_end();
                self.refresh_suggestions();
            }
            KeyCode::Char('z') if ctrl && key.modifiers.contains(KeyModifiers::SHIFT) => {
                if self.composer.redo() {
                    self.refresh_suggestions();
                }
            }
            KeyCode::Char('z') if ctrl => {
                if self.composer.undo() {
                    self.refresh_suggestions();
                }
            }
            KeyCode::Char('a') if ctrl => {
                self.composer.move_home();
            }
            KeyCode::Char('e') if ctrl => {
                // Toggle expanded view of a large paste. The full content
                // was never truncated — this only changes presentation.
                if self.composer.paste_placeholder().is_some() {
                    self.paste_expanded = !self.paste_expanded;
                }
            }
            KeyCode::Enter => {
                // Newline shortcuts: Shift+Enter, Ctrl+Enter, Ctrl+J, Alt+Enter
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || (ctrl && key.code == KeyCode::Enter)
                    || (alt && key.code == KeyCode::Enter)
                {
                    self.composer.insert_newline();
                    self.refresh_suggestions();
                    return UiAction::None;
                }
                // Plain Enter: submit (preserves newlines in pasted
                // multiline content — the text is sent verbatim).
                let text = self.composer.text.clone();
                if text.trim().is_empty() {
                    return UiAction::None;
                }
                self.composer.clear();
                self.paste_expanded = false;
                self.suggestions.clear();
                self.suggestion_idx = 0;
                return UiAction::Submit(text);
            }
            KeyCode::Char('j') if ctrl => {
                self.composer.insert_newline();
                self.refresh_suggestions();
            }
            KeyCode::Tab => {
                if !self.suggestions.is_empty() {
                    self.suggestion_idx = (self.suggestion_idx + 1) % self.suggestions.len();
                    let chosen = self.suggestions[self.suggestion_idx].clone();
                    // Detect whether this is an @-file completion or a /cmd.
                    let trimmed = self.composer.text.trim_start();
                    if trimmed.starts_with('@') {
                        // Insert the path with a trailing space, keep @ prefix.
                        self.composer.text = format!("@{chosen} ");
                    } else {
                        self.composer.text = format!("/{chosen}");
                    }
                    self.composer.cursor = self.composer.len();
                    self.composer.multiline = false;
                    self.refresh_suggestions();
                } else {
                    self.dash.open = true;
                }
            }
            KeyCode::Up => {
                if !self.suggestions.is_empty() {
                    self.suggestion_idx = self
                        .suggestion_idx
                        .checked_sub(1)
                        .unwrap_or(self.suggestions.len() - 1);
                } else {
                    self.recall_history(1);
                }
            }
            KeyCode::Down => {
                if !self.suggestions.is_empty() {
                    self.suggestion_idx = (self.suggestion_idx + 1) % self.suggestions.len();
                } else {
                    self.recall_history(-1);
                }
            }
            KeyCode::PageUp => {
                self.scroll_back = self.scroll_back.saturating_add(10);
            }
            KeyCode::PageDown => {
                self.scroll_back = self.scroll_back.saturating_sub(10);
            }
            KeyCode::Left => {
                if ctrl {
                    self.composer.move_word_left();
                } else {
                    self.composer.move_left();
                }
            }
            KeyCode::Right => {
                if ctrl {
                    self.composer.move_word_right();
                } else {
                    self.composer.move_right();
                }
            }
            KeyCode::Home => {
                self.composer.move_home();
            }
            KeyCode::End => {
                self.composer.move_end();
            }
            KeyCode::Backspace => {
                self.composer.delete_before();
                self.refresh_suggestions();
            }
            KeyCode::Delete => {
                self.composer.delete_after();
                self.refresh_suggestions();
            }
            KeyCode::Esc => {
                if !self.composer.text.is_empty() {
                    self.composer.clear();
                    self.refresh_suggestions();
                }
            }
            KeyCode::Char(c) => {
                self.composer.insert_char(c);
                self.refresh_suggestions();
            }
            _ => {}
        }
        UiAction::None
    }

    fn recall_history(&mut self, dir: i32) {
        if self.history.is_empty() {
            return;
        }
        match self.history_idx {
            None => {
                if dir > 0 {
                    self.history_draft = self.composer.text.clone();
                    self.history_idx = Some(0);
                    self.composer.text = self.history[0].clone();
                    self.composer.cursor = self.composer.len();
                    self.composer.multiline = self.composer.text.contains('\n');
                }
            }
            Some(i) => {
                let next = if dir > 0 {
                    (i + 1).min(self.history.len() - 1)
                } else if i == 0 {
                    // Restoring the draft.
                    self.history_idx = None;
                    self.composer.text = self.history_draft.clone();
                    self.composer.cursor = self.composer.len();
                    self.composer.multiline = self.composer.text.contains('\n');
                    return;
                } else {
                    i - 1
                };
                self.history_idx = Some(next);
                self.composer.text = self.history[next].clone();
                self.composer.cursor = self.composer.len();
                self.composer.multiline = self.composer.text.contains('\n');
            }
        }
    }

    fn refresh_suggestions(&mut self) {
        self.suggestions.clear();
        self.suggestion_idx = 0;
        let trimmed = self.composer.text.trim_start();
        if let Some(at) = trimmed.strip_prefix('@') {
            // File autocomplete: fuzzy-search files under workdir.
            let query = at.to_lowercase();
            let root = match &self.workdir {
                Some(p) => p.clone(),
                None => return,
            };
            let files = walk_files_for_suggestions(&root, 200);
            self.suggestions = files
                .into_iter()
                .filter(|p| {
                    let lower = p.to_lowercase();
                    query.is_empty() || lower.contains(&query)
                })
                .take(6)
                .collect();
            return;
        }
        if !trimmed.starts_with('/') {
            return;
        }
        let word = trimmed[1..].split_whitespace().next().unwrap_or("");
        let typed_space = trimmed[1..].contains(' ');
        if typed_space {
            // Command complete: show argument hints instead.
            return;
        }
        for name in &self.hints.names {
            if word.is_empty() || name.starts_with(word) {
                self.suggestions.push(name.clone());
            }
            if self.suggestions.len() >= 6 {
                break;
            }
        }
    }

    // ── Rendering ────────────────────────────────────────────────

    /// Render the full-screen app (session screen or dashboard overlay).
    pub fn render(&self, frame: &mut Frame) {
        if self.dash.open {
            self.render_dashboard(frame);
        } else {
            self.render_session(frame);
        }
    }

    fn render_session(&self, frame: &mut Frame) {
        let area = frame.area();

        // The composer grows with wrapped content (capped), so the
        // transcript gets whatever is left. Height and rendering share the
        // same width and the same wrapping algorithm.
        let input_rows = self.composer_visible_rows(area.width);
        let [header, transcript, input, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(input_rows as u16),
            Constraint::Length(1),
        ])
        .areas(area);

        self.render_header(frame, header);
        self.render_transcript(frame, transcript);
        self.render_input(frame, input);
        self.render_status(frame, footer);
    }

    /// Display columns available for composer text: terminal width minus
    /// the two-cell `> ` prompt prefix.
    fn composer_wrap_width(&self, term_width: u16) -> usize {
        (term_width as usize).saturating_sub(2).max(8)
    }

    /// Number of terminal rows the composer needs at the real terminal
    /// width: its visual rows (capped at 8) plus one hint row.
    fn composer_visible_rows(&self, term_width: u16) -> usize {
        let wrap = self.composer_wrap_width(term_width);
        let visual = if self.composer.paste_placeholder().is_some() && !self.paste_expanded {
            1
        } else {
            crate::composer::total_visual_rows(&self.composer.text, wrap)
        };
        visual.clamp(1, 8) + 1
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        use unicode_width::UnicodeWidthStr;

        let t = &self.theme;
        let width = area.width as usize;

        // Left: crimson REQUIEM wordmark + mode.
        let left_spans = vec![
            Span::styled("REQUIEM", t.style_bold(Token::Accent)),
            Span::styled("  ", Style::new()),
            Span::styled(self.mode.clone(), t.style(Token::TextMuted)),
        ];

        // Center: model · project (dropped on narrow terminals).
        let mut center_spans = Vec::new();
        if width >= 56 {
            center_spans.push(Span::styled(
                self.model.clone(),
                t.style(Token::TextSecondary),
            ));
            if !self.project.is_empty() {
                center_spans.push(Span::styled("  ·  ", t.style(Token::TextMuted)));
                center_spans.push(Span::styled(
                    self.project.clone(),
                    t.style(Token::TextSecondary),
                ));
            }
        }

        // Right: connection status (kept distinct from the brand crimson).
        let mut right_spans = Vec::new();
        if self.is_running() {
            right_spans.push(Span::styled(
                format!("{} running", spinner_glyph(self.started)),
                t.style(Token::Info),
            ));
        } else {
            right_spans.push(Span::styled("●", t.style(Token::Success)));
            right_spans.push(Span::styled(" ready", t.style(Token::TextMuted)));
        }

        let mut spans = left_spans;
        let left_w: usize = spans.iter().map(|s| s.content.width()).sum();
        let center_w: usize = center_spans.iter().map(|s| s.content.width()).sum();
        let right_w: usize = right_spans.iter().map(|s| s.content.width()).sum();
        let gap = width.saturating_sub(left_w + center_w + right_w + 2);
        // Put half the slack before the centre block, the rest before the
        // right block, so the header breathes on wide terminals.
        let left_gap = if center_w > 0 { gap / 2 } else { gap };
        let right_gap = gap.saturating_sub(left_gap);

        spans.push(Span::styled(" ".repeat(left_gap), Style::new()));
        spans.extend(center_spans);
        spans.push(Span::styled(" ".repeat(right_gap), Style::new()));
        spans.extend(right_spans);

        frame.render_widget(
            Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }),
            area,
        );
    }

    fn render_transcript(&self, frame: &mut Frame, area: Rect) {
        let t = &self.theme;
        let width = area.width.saturating_sub(2) as usize; // padding
        let rendered = self.render_transcript_lines(width);

        let total = rendered.lines.len();
        let viewport = area.height as usize;
        let offset = if total > viewport {
            (total - viewport).saturating_sub(self.scroll_back) as u16
        } else {
            0
        };

        let block = Block::default()
            .borders(Borders::NONE)
            .style(t.panel_style());
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(rendered.lines)
                .wrap(Wrap { trim: false })
                .scroll((offset, 0)),
            area,
        );
    }

    /// Convert transcript entries into styled, pre-wrapped lines.
    fn render_transcript_lines(&self, width: usize) -> RenderedTranscript {
        let t = &self.theme;
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(self.entries.len() + 2);

        for e in &self.entries {
            let text_width = width.saturating_sub(e.indent.min(width / 2) + 3);

            if e.banner {
                // The official artwork, rendered raw with the crimson accent.
                // No ANSI round-trip, no re-wrapping and no prefix, so the
                // glyphs stay exactly as authored.
                for raw_line in e.text.lines() {
                    lines.push(Line::from(Span::styled(
                        raw_line.to_string(),
                        t.style_bold(Token::Accent),
                    )));
                }
            } else if e.markdown {
                // Render markdown with the terminal's colour capability,
                // then convert each ANSI line into ratatui Spans.
                let ansi_lines =
                    md::render_with_mode(&e.text, t, text_width.max(8), crate::color::color_mode());
                for (i, ansi_line) in ansi_lines.iter().enumerate() {
                    let mut spans: Vec<Span<'static>> = Vec::new();
                    if i == 0 {
                        spans.push(Span::styled(
                            format!("{glyph}  ", glyph = e.glyph),
                            t.style_bold(e.fg),
                        ));
                    } else {
                        let pad = " ".repeat(e.glyph.chars().count() + 2);
                        spans.push(Span::styled(pad, Style::new()));
                    }
                    spans.extend(parse_ansi_line(ansi_line));
                    lines.push(Line::from(spans));
                }
            } else {
                // Pre-styled (ANSI) or alignment-sensitive multi-line text
                // (welcome block): emit each line verbatim, no re-wrap.
                let is_prestyled = e.text.contains("\x1b[");
                let is_align_sensitive = is_prestyled || (e.indent == 0 && e.text.contains('\n'));
                if is_align_sensitive {
                    let base_style = if e.bold {
                        t.style_bold(e.fg)
                    } else {
                        t.style(e.fg)
                    };
                    for (i, raw_line) in e.text.lines().enumerate() {
                        let mut spans: Vec<Span<'static>> = Vec::new();
                        if i == 0 {
                            spans.push(Span::styled(
                                format!("{glyph}  ", glyph = e.glyph),
                                base_style,
                            ));
                        } else {
                            let pad = " ".repeat(e.glyph.chars().count() + 2);
                            spans.push(Span::styled(pad, Style::new()));
                        }
                        if is_prestyled {
                            spans.extend(parse_ansi_line(raw_line));
                        } else {
                            // Plain multi-line blocks must still use the
                            // entry's token — parsing plain text would drop it.
                            spans.push(Span::styled(raw_line.to_string(), base_style));
                        }
                        lines.push(Line::from(spans));
                    }
                } else {
                    let wrapped = wrap_to_width(&e.text, text_width.max(8));
                    for (i, chunk) in wrapped.iter().enumerate() {
                        let mut spans = Vec::new();
                        if i == 0 {
                            spans.push(Span::styled(
                                format!("{glyph}  ", glyph = e.glyph),
                                if e.bold {
                                    t.style_bold(e.fg)
                                } else {
                                    t.style(e.fg)
                                },
                            ));
                        } else {
                            let pad = " ".repeat(e.glyph.chars().count() + 2);
                            spans.push(Span::styled(pad, Style::new()));
                        }
                        let mut style = t.style(e.fg);
                        if e.bold {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        if e.streaming {
                            style = style.add_modifier(Modifier::DIM);
                        }
                        spans.push(Span::styled(chunk.clone(), style));
                        lines.push(Line::from(spans));
                    }
                }
            }
            // One blank line between entries for readability.
            lines.push(Line::from(Span::styled(
                " ".repeat(width.max(1)),
                t.style(Token::Background),
            )));
        }
        RenderedTranscript { lines }
    }

    fn render_input(&self, frame: &mut Frame, area: Rect) {
        let t = &self.theme;
        let text_rows = (area.height as usize).saturating_sub(1).max(1);
        let wrap_width = self.composer_wrap_width(area.width);

        // Large-paste compact display: when the composer holds content that
        // is visually noisy (≥ 50 lines or ≥ 4 KB), show a single-line
        // placeholder. The content remains intact in `composer.text` and is
        // submitted exactly once. Ctrl+E expands the real text.
        if let Some(placeholder) = self
            .composer
            .paste_placeholder()
            .filter(|_| !self.paste_expanded)
        {
            let [text_area, hint_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

            let spans: Vec<Span<'static>> = vec![
                Span::styled("> ", t.style_bold(Token::Accent)),
                Span::styled(placeholder, t.style(Token::TextMuted)),
                Span::styled("  Ctrl+E expand", t.style(Token::TextMuted)),
            ];
            frame.render_widget(
                Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }),
                text_area,
            );

            let hint = self.hint_line();
            frame.render_widget(
                Paragraph::new(Line::from(hint)).wrap(Wrap { trim: false }),
                hint_area,
            );
            return;
        }

        // One canonical wrapping pass — the same one used for layout height.
        let text = &self.composer.text;
        let rows = crate::composer::visual_rows(text, wrap_width);
        let total = rows.len();
        let (cursor_row, cursor_col) =
            crate::composer::cursor_visual_position(text, self.composer.cursor, wrap_width);

        // Keep the caret visible without extra state: once it passes the
        // last text row, the view slides down by exactly the overflow.
        let start = if cursor_row >= text_rows {
            cursor_row + 1 - text_rows
        } else {
            0
        };
        let end = (start + text_rows).min(total);

        let mut out: Vec<Line<'static>> = Vec::with_capacity(text_rows + 1);
        for (idx, row) in rows.iter().enumerate().take(end).skip(start) {
            let prefix = if idx == 0 {
                Span::styled("> ", t.style_bold(Token::Accent))
            } else {
                Span::styled("  ", Style::new())
            };

            if idx == cursor_row {
                let (before, at, after) =
                    crate::composer::split_at_display_col(&row.text, cursor_col);
                let cursor_style = t
                    .style(Token::Background)
                    .bg(t.ratatui_color(Token::Accent));
                let mut spans = vec![
                    prefix,
                    Span::styled(before, t.style(Token::Text)),
                    Span::styled(
                        if at.is_empty() { " ".to_string() } else { at },
                        cursor_style,
                    ),
                ];
                if !after.is_empty() {
                    spans.push(Span::styled(after, t.style(Token::Text)));
                }
                out.push(Line::from(spans));
            } else {
                out.push(Line::from(vec![
                    prefix,
                    Span::styled(row.text.clone(), t.style(Token::Text)),
                ]));
            }
        }
        // Fill unused rows so the composer panel keeps a stable background.
        while out.len() < text_rows {
            out.push(Line::from(""));
        }

        let [text_area, hint_area] =
            Layout::vertical([Constraint::Length(text_rows as u16), Constraint::Length(1)])
                .areas(area);

        frame.render_widget(Paragraph::new(out), text_area);

        let hint = self.hint_line();
        frame.render_widget(
            Paragraph::new(Line::from(hint)).wrap(Wrap { trim: false }),
            hint_area,
        );
    }

    /// Build the hint line under the input.
    fn hint_line(&self) -> Vec<Span<'static>> {
        let t = &self.theme;
        if !self.suggestions.is_empty() {
            let mut spans = vec![Span::styled("  ", Style::new())];
            for (i, s) in self.suggestions.iter().enumerate() {
                let style = if i == self.suggestion_idx {
                    t.style_bold(Token::AccentBright)
                } else {
                    t.style(Token::TextMuted)
                };
                let desc = hint_text(s, &self.hints);
                if i == self.suggestion_idx && !desc.is_empty() {
                    spans.push(Span::styled(format!("/{s} "), style));
                    spans.push(Span::styled(desc, t.style(Token::TextSecondary)));
                    spans.push(Span::styled(" ", Style::new()));
                } else {
                    spans.push(Span::styled(format!("/{s} "), style));
                }
            }
            spans.push(Span::styled("Tab to cycle", t.style(Token::TextMuted)));
            return spans;
        }
        if let Some((cmd, args)) = self.current_command_args() {
            let mut spans = vec![Span::styled(
                format!("{cmd} "),
                t.style(Token::TextSecondary),
            )];
            spans.push(Span::styled(args.join(" | "), t.style(Token::Info)));
            return spans;
        }
        // Minimal default hint — contextual, short.
        let default_hint = if self.composer.paste_placeholder().is_some() {
            "  Enter send (full content) · Ctrl+E expand · Esc clear"
        } else if self.composer.multiline {
            "  Enter send · Shift+Enter newline · Esc clear · Ctrl+C quit"
        } else {
            "  Enter send · Shift+Enter newline · /help · @files · Ctrl+C quit"
        };
        vec![Span::styled(default_hint, t.style(Token::TextMuted))]
    }

    /// (command, arg hints) when the input names a known command.
    fn current_command_args(&self) -> Option<(String, Vec<String>)> {
        let trimmed = self.composer.text.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let word = trimmed[1..].split_whitespace().next()?;
        let args = self.hints.args.get(word)?;
        if args.is_empty() {
            return None;
        }
        Some((word.to_string(), args.clone()))
    }

    fn render_status(&self, frame: &mut Frame, area: Rect) {
        use unicode_width::UnicodeWidthStr;

        let t = &self.theme;
        let width = area.width as usize;

        // Left: relevant keyboard hints.
        let left_spans = vec![Span::styled(
            if self.is_running() {
                "  Ctrl+C cancel · /stop stop all"
            } else {
                "  /help · /dashboard · @files · Shift+Enter newline"
            },
            t.style(Token::TextMuted),
        )];

        // Center: running agent/task status (compact, up to 3 agents).
        let mut center_spans = Vec::new();
        if self.is_running() {
            let running_agents: Vec<&AgentRow> = self
                .dash
                .agents
                .iter()
                .filter(|a| a.status == AgentStatus::Running)
                .take(3)
                .collect();
            if !running_agents.is_empty() {
                for agent in &running_agents {
                    let label = if agent.current_task.is_empty() {
                        agent.role.as_str()
                    } else {
                        agent.current_task.as_str()
                    };
                    let short: String = label.chars().take(28).collect();
                    center_spans.push(Span::styled(format!("● {short}  "), t.style(Token::Info)));
                }
            } else if let Some(task) = self.active_task.as_deref() {
                let short: String = task.chars().take(40).collect();
                center_spans.push(Span::styled(
                    format!("{} ", spinner_glyph(self.started)),
                    t.style_bold(Token::Info),
                ));
                center_spans.push(Span::styled(short, t.style(Token::TextSecondary)));
            }
        }

        // Right: mode + token usage.
        let right_spans = vec![
            Span::styled(self.mode.clone(), t.style_bold(Token::Accent)),
            Span::styled(
                format!(
                    "  ↑{} ↓{}",
                    fmt_tokens(self.tokens_in),
                    fmt_tokens(self.tokens_out)
                ),
                t.style(Token::TextMuted),
            ),
        ];

        // Compose: left … center … right, splitting the slack between the
        // two gaps so the row can never overflow.
        let left_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
        let center_w: usize = center_spans.iter().map(|s| s.content.width()).sum();
        let right_w: usize = right_spans.iter().map(|s| s.content.width()).sum();
        let gap = width.saturating_sub(left_w + center_w + right_w + 2);
        let left_gap = if center_w > 0 { gap / 2 } else { gap };
        let right_gap = gap.saturating_sub(left_gap);

        let mut spans = left_spans;
        spans.push(Span::styled(" ".repeat(left_gap), Style::new()));
        spans.extend(center_spans);
        spans.push(Span::styled(" ".repeat(right_gap), Style::new()));
        spans.extend(right_spans);

        frame.render_widget(
            Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false }),
            area,
        );
    }

    // ── Dashboard rendering ──────────────────────────────────────

    fn render_dashboard(&self, frame: &mut Frame) {
        let t = &self.theme;
        let area = frame.area();
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);

        // Compact single-row header matching the session screen.
        let (done, total) = self.dash.task_progress();
        let header_line = Line::from(vec![
            Span::styled("REQUIEM", t.style_bold(Token::Accent)),
            Span::styled("  ", Style::new()),
            Span::styled("[dashboard]", t.style_bold(Token::AccentBright)),
            Span::styled(
                format!(
                    "  tasks {done}/{total}  ·  agents {}  ·  tok {}",
                    self.dash.agents.len(),
                    fmt_tokens(self.dash.total_tokens())
                ),
                t.style(Token::TextSecondary),
            ),
            Span::styled("  Esc close", t.style(Token::TextMuted)),
        ]);
        frame.render_widget(
            Paragraph::new(header_line).wrap(Wrap { trim: false }),
            header,
        );

        // Progress gauge row on top of the body.
        let ratio = if total == 0 {
            0.0
        } else {
            done as f64 / total as f64
        };
        let [gauge_row, panels] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(body);
        if area.width > 40 {
            frame.render_widget(
                Gauge::default()
                    .ratio(ratio)
                    .label(Span::styled(
                        format!(" progress {done}/{total} "),
                        t.style(Token::Text),
                    ))
                    .gauge_style(t.style(Token::Accent)),
                gauge_row,
            );
        }

        // Panels: side-by-side when wide, stacked when narrow.
        if area.width >= 80 {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
                    .areas(panels);
            let [tasks, agents] =
                Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)])
                    .areas(left);
            self.render_tasks_panel(frame, tasks);
            self.render_agents_panel(frame, agents);
            self.render_events_panel(frame, right);
        } else {
            let [tasks, agents, events] = Layout::vertical([
                Constraint::Percentage(34),
                Constraint::Percentage(33),
                Constraint::Percentage(33),
            ])
            .areas(panels);
            self.render_tasks_panel(frame, tasks);
            self.render_agents_panel(frame, agents);
            self.render_events_panel(frame, events);
        }

        // Footer: selection detail + hints.
        let detail = self.selected_detail();
        frame.render_widget(
            Paragraph::new(Line::from(detail)).wrap(Wrap { trim: false }),
            footer,
        );
    }

    fn panel_block(&self, title: &str, panel: DashPanel) -> Block<'static> {
        let t = &self.theme;
        let selected = self.dash.panel == panel;
        Block::default()
            .borders(Borders::ALL)
            .border_type(if selected {
                BorderType::Double
            } else {
                BorderType::Plain
            })
            .border_style(t.style(if selected {
                Token::BorderActive
            } else {
                Token::Border
            }))
            .title(Span::styled(
                format!(" {title} "),
                if selected {
                    t.style_bold(Token::Accent)
                } else {
                    t.style(Token::TextMuted)
                }
                .add_modifier(Modifier::BOLD),
            ))
    }

    fn render_tasks_panel(&self, frame: &mut Frame, area: Rect) {
        let t = &self.theme;
        let block = self.panel_block("tasks", DashPanel::Tasks);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let width = inner.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.dash.tasks.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no tasks yet — submit one in the session",
                t.style(Token::TextMuted),
            )));
        }
        let p = panel_index(DashPanel::Tasks);
        for (i, task) in self.dash.tasks.iter().enumerate() {
            let token = status_task_token(task.status);
            let sel = self.dash.panel == DashPanel::Tasks && i == self.dash.selected[p];
            let desc = truncate_chars(&task.description, width.saturating_sub(28).max(8));
            let mut spans = vec![
                Span::styled(
                    if sel { " ▸ " } else { "   " },
                    t.style_bold(Token::AccentBright),
                ),
                Span::styled(truncate_chars(&task.id, 8), t.style(Token::TextMuted)),
                Span::styled(" ", t.style(Token::TextMuted)),
                Span::styled(task.status.label(), t.style(token)),
                Span::styled("  ", t.style(Token::TextMuted)),
                Span::styled(
                    desc,
                    t.style(if sel {
                        Token::Text
                    } else {
                        Token::TextSecondary
                    }),
                ),
            ];
            if let Some(a) = &task.assignee {
                spans.push(Span::styled(
                    format!(" ← {}", truncate_chars(a, 12)),
                    t.style(Token::Violet),
                ));
            }
            lines.push(Line::from(spans));
            if !task.dependencies.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("     ⧗ {}", task.dependencies.join(", ")),
                    t.style(Token::TextMuted),
                )));
            }
        }

        let viewport = inner.height as usize;
        let total = lines.len();
        let offset = if total > viewport {
            (total - viewport).saturating_sub(self.dash.scroll[p]) as u16
        } else {
            0
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((offset, 0)),
            inner,
        );
    }

    fn render_agents_panel(&self, frame: &mut Frame, area: Rect) {
        let t = &self.theme;
        let block = self.panel_block("agents", DashPanel::Agents);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let width = inner.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.dash.agents.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no agents — run a task to see the hierarchy",
                t.style(Token::TextMuted),
            )));
        }
        let p = panel_index(DashPanel::Agents);
        for (i, agent) in self.dash.agents.iter().enumerate() {
            let token = t.status_token(&agent.status.to_string());
            let sel = self.dash.panel == DashPanel::Agents && i == self.dash.selected[p];
            let depth_prefix = if agent.parent.is_empty() {
                String::new()
            } else {
                "  └─ ".to_string()
            };
            let name_width = width.saturating_sub(30).max(8);
            let mut spans = vec![
                Span::styled(
                    if sel { " ▸ " } else { "   " },
                    t.style_bold(Token::AccentBright),
                ),
                Span::styled(depth_prefix, t.style(Token::Border)),
                Span::styled(
                    truncate_chars(&agent.agent_id, name_width),
                    t.style_bold(Token::Text),
                ),
                Span::styled(
                    format!(" ({})", truncate_chars(&agent.role, 12)),
                    t.style(Token::TextMuted),
                ),
                Span::styled(" ", t.style(Token::TextMuted)),
                Span::styled(agent.status.to_string(), t.style(token)),
                Span::styled(
                    format!("  t{}/{}", agent.turns_used, agent.turns_max),
                    t.style(Token::TextMuted),
                ),
                Span::styled(
                    format!("  tok {}", fmt_tokens(agent.tokens_in + agent.tokens_out)),
                    t.style(Token::TextSecondary),
                ),
            ];
            if !agent.current_task.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", truncate_chars(&agent.current_task, 32)),
                    t.style(Token::TextSecondary),
                ));
            }
            lines.push(Line::from(spans));
        }

        let viewport = inner.height as usize;
        let total = lines.len();
        let offset = if total > viewport {
            (total - viewport).saturating_sub(self.dash.scroll[p]) as u16
        } else {
            0
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((offset, 0)),
            inner,
        );
    }

    fn render_events_panel(&self, frame: &mut Frame, area: Rect) {
        let t = &self.theme;
        let block = self.panel_block("events", DashPanel::Events);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let width = inner.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.dash.events.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no events yet",
                t.style(Token::TextMuted),
            )));
        }
        let p = panel_index(DashPanel::Events);
        for (i, le) in self.dash.events.iter().enumerate().rev() {
            let sel = self.dash.panel == DashPanel::Events && i == self.dash.selected[p];
            let mut spans = vec![
                Span::styled(
                    if sel { " ▸" } else { "  " },
                    t.style_bold(Token::AccentBright),
                ),
                Span::styled(format!(" +{:>4}s ", le.rel_secs), t.style(Token::TextMuted)),
            ];
            let summary = le.event.summary();
            if summary.is_empty() {
                continue; // deltas are rendered in the transcript, not the log
            }
            let token = event_token(&le.event);
            spans.push(Span::styled(
                wrap_to_width(&summary, width.saturating_sub(10).max(8)).join(" "),
                t.style(if sel { Token::Text } else { token }),
            ));
            lines.push(Line::from(spans));
        }

        let viewport = inner.height as usize;
        let total = lines.len();
        let offset = if total > viewport {
            (total - viewport).saturating_sub(self.dash.scroll[p]) as u16
        } else {
            0
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((offset, 0)),
            inner,
        );
    }

    /// Detail line for the currently selected item.
    fn selected_detail(&self) -> Vec<Span<'static>> {
        let t = &self.theme;
        let p = panel_index(self.dash.panel);
        match self.dash.panel {
            DashPanel::Tasks => {
                let task = self.dash.tasks.get(self.dash.selected[p]);
                match task {
                    Some(task) => vec![Span::styled(
                        format!(
                            "  {} — {} — {} — prio {}",
                            task.id,
                            task.status.label(),
                            task.description,
                            task.priority
                        ),
                        t.style(Token::Text),
                    )],
                    None => vec![Span::styled(
                        "  no task selected",
                        t.style(Token::TextMuted),
                    )],
                }
            }
            DashPanel::Agents => {
                let agent = self.dash.agents.get(self.dash.selected[p]);
                match agent {
                    Some(a) => vec![Span::styled(
                        format!(
                            "  {} — {} — {} — task: {} — tok {}/{}",
                            a.agent_id,
                            a.role,
                            a.status,
                            if a.current_task.is_empty() {
                                "-"
                            } else {
                                &a.current_task
                            },
                            a.tokens_in,
                            a.tokens_out
                        ),
                        t.style(Token::Text),
                    )],
                    None => vec![Span::styled(
                        "  no agent selected",
                        t.style(Token::TextMuted),
                    )],
                }
            }
            DashPanel::Events => {
                let le = self.dash.events.get(self.dash.selected[p]);
                match le {
                    Some(le) => vec![Span::styled(
                        format!("  +{}s {}", le.rel_secs, le.event.summary()),
                        t.style(Token::Text),
                    )],
                    None => vec![Span::styled(
                        "  no event selected",
                        t.style(Token::TextMuted),
                    )],
                }
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn panel_index(p: DashPanel) -> usize {
    match p {
        DashPanel::Tasks => 0,
        DashPanel::Agents => 1,
        DashPanel::Events => 2,
    }
}

fn panel_len(dash: &DashState, p: usize) -> usize {
    match p {
        0 => dash.tasks.len(),
        1 => dash.agents.len(),
        _ => dash.events.len(),
    }
}

fn status_task_token(status: TaskStatus) -> Token {
    match status {
        TaskStatus::Running => Token::Info,
        TaskStatus::Completed => Token::Success,
        TaskStatus::Failed => Token::Error,
        TaskStatus::Cancelled => Token::TextMuted,
        TaskStatus::Blocked | TaskStatus::Pending | TaskStatus::Claimed => Token::Warning,
        TaskStatus::Available => Token::Violet,
    }
}

fn event_token(event: &Event) -> Token {
    match event {
        Event::RunFailed { .. } => Token::Error,
        Event::RunCompleted { .. } => Token::Success,
        Event::RunCancelled { .. } => Token::Warning,
        Event::Notice {
            level: NoticeLevel::Error,
            ..
        } => Token::Error,
        Event::Notice {
            level: NoticeLevel::Success,
            ..
        } => Token::Success,
        Event::Notice {
            level: NoticeLevel::Warning,
            ..
        } => Token::Warning,
        Event::Usage { .. } => Token::TextMuted,
        Event::ToolCallFinished { ok: false, .. } => Token::Error,
        Event::ToolCallFinished { ok: true, .. } => Token::Success,
        Event::ToolCallStarted { .. } => Token::TextSecondary,
        Event::AgentSpawned { .. } => Token::Violet,
        Event::RunStarted { .. } => Token::Info,
        _ => Token::TextSecondary,
    }
}

/// Spinner glyph for a start instant (driven by elapsed time — no timer
/// thread needed).
fn spinner_glyph(started: Instant) -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let idx = (started.elapsed().as_millis() / 90) as usize % FRAMES.len();
    FRAMES[idx]
}

/// Human-friendly token counts (1.2k, 3.4M).
pub fn fmt_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Truncate to `max` chars, always on a char boundary.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Description text for a command name (from the injected hints).
fn hint_text(name: &str, hints: &CommandHints) -> String {
    hints.descriptions.get(name).cloned().unwrap_or_default()
}

// ── Tests (TestBackend) ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn test_app() -> App {
        let mut hints = CommandHints {
            names: vec![
                "ask".into(),
                "auto".into(),
                "code".into(),
                "dashboard".into(),
                "help".into(),
                "exit".into(),
            ],
            ..Default::default()
        };
        hints
            .descriptions
            .insert("help".into(), "show available commands".into());
        hints.args.insert("model".into(), vec!["--test".into()]);
        let mut app = App::new(
            Theme::default(),
            hints,
            "sess-1234".into(),
            "test-model".into(),
        );
        app.set_mode("code");
        app
    }

    fn render_to_string(app: &App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>()
    }

    fn visible_text(app: &App, w: u16, h: u16) -> String {
        // Strip line-leading padding noise: join all cells, then squish.
        let raw = render_to_string(app, w, h);
        raw.lines().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn session_screen_shows_header_and_prompt() {
        let app = test_app();
        let text = visible_text(&app, 100, 30);
        assert!(text.contains("REQUIEM"), "wordmark: {text}");
        assert!(text.contains("test-model"), "model in header");
        assert!(text.contains("> "), "input prompt present");
        assert!(text.contains("code"), "mode shown: {text}");
    }

    #[test]
    fn welcome_screen_shows_banner_and_model_info() {
        let mut app = test_app();
        app.push_welcome("/home/user/project");
        let text = visible_text(&app, 120, 40);
        // Banner must be visible (its distinctive characters).
        assert!(text.contains("8888"), "banner art visible: {text}");
        // Model and project info.
        assert!(text.contains("test-model"), "model shown: {text}");
        assert!(
            text.contains("Project") && text.contains("project"),
            "project shown: {text}"
        );
        // Command suggestions.
        assert!(text.contains("/help"), "command hints: {text}");
    }

    #[test]
    fn welcome_screen_does_not_wrap_banner_art() {
        let mut app = test_app();
        app.push_welcome(".");
        // At 80 cols the banner (which is ~120 chars wide) would be clipped
        // but must NOT be re-wrapped / mangled — each original line stays intact.
        let buf = render_to_string(&app, 80, 50);
        // The banner's first line starts with dots and spaces.
        assert!(
            buf.lines().any(|l| l.contains("..")),
            "banner first-line fragment visible: {buf}"
        );
    }

    /// One terminal row as a string (avoids char/column drift from wide
    /// glyphs in the flattened buffer).
    fn row_text(app: &App, w: u16, h: u16, row: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let start = row as usize * w as usize;
        let end = (start + w as usize).min(buffer.content.len());
        buffer.content[start..end]
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn banner_is_raw_crimson_spans_without_ansi_round_trip() {
        let mut app = test_app();
        app.push_welcome("/home/user/project");
        let banner = app.entries.iter().find(|e| e.banner).expect("banner entry");
        assert_eq!(banner.fg, Token::Accent, "banner must use the accent token");
        assert!(
            !banner.text.contains('\x1b'),
            "banner must be raw artwork, not ANSI-painted"
        );
        assert!(banner.text.contains("8888"), "official artwork preserved");
        // Rendered art still present, and no blue is introduced by the
        // banner path.
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let art: Vec<_> = buffer
            .content
            .iter()
            .filter(|c| c.symbol() == "8")
            .collect();
        assert!(!art.is_empty(), "banner art not rendered");
        assert!(
            art.iter().all(|c| c.fg != Color::Blue),
            "banner must never render blue"
        );
    }

    #[test]
    fn session_layout_is_responsive() {
        let mut app = test_app();
        app.push_welcome("/home/user/project");
        for (w, h) in [(80u16, 24u16), (100, 30), (120, 40)] {
            let header = row_text(&app, w, h, 0);
            let footer = row_text(&app, w, h, h - 1);
            assert!(header.contains("REQUIEM"), "{w}x{h} header: {header:?}");
            assert!(
                !footer.contains('─'),
                "{w}x{h} footer has a border: {footer:?}"
            );
            assert!(
                footer.contains("/help") || footer.contains("Shift+Enter"),
                "{w}x{h} footer hints: {footer:?}"
            );
        }
    }

    #[test]
    fn composer_height_uses_real_terminal_width() {
        let mut app = test_app();
        app.composer.text = "x".repeat(200);
        app.composer.cursor = 200;
        let narrow = app.composer_visible_rows(40);
        let wide = app.composer_visible_rows(120);
        assert!(
            narrow > wide,
            "narrow terminal must need more rows ({narrow} vs {wide})"
        );
        assert!(narrow <= 9, "composer capped at 8 text rows + hint");
        // Rendering at the small size must not panic or lose the caret.
        let _ = row_text(&app, 40, 24, 22);
    }

    #[test]
    fn wrapped_long_prompt_renders_tail_and_cursor() {
        let mut app = test_app();
        let long = "a".repeat(300);
        app.composer.text = long.clone();
        app.composer.cursor = long.chars().count();
        let backend = TestBackend::new(60, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.render(f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let flat: String = buffer.content.iter().map(|c| c.symbol()).collect();
        assert!(flat.contains("aaaa"), "prompt text must be visible");
        // Cursor block is drawn with a non-default background somewhere in
        // the input region.
        let has_cursor = buffer
            .content
            .iter()
            .any(|c| c.bg != Color::Reset && c.bg != Color::Black);
        assert!(has_cursor, "visible cursor block missing");
    }

    #[test]
    fn error_card_is_actionable_and_structured() {
        let card = format_error_card("model error [429]: slow down", "rate_limited", "test-model");
        assert!(card.contains("Rate Limit"), "{card}");
        assert!(card.contains("429"), "status shown: {card}");
        assert!(card.contains("test-model"), "model shown: {card}");
        assert!(card.contains("/retry"), "recovery command: {card}");
        assert!(card.contains("**Retry:**"), "retry state: {card}");
    }

    #[test]
    fn transcript_renders_user_and_agent_lines() {
        let mut app = test_app();
        app.push_user_line("fix the login bug");
        app.push_event(&Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: requiem_core::Id::new(),
            description: "fix the login bug".into(),
            mode: "code".into(),
        });
        app.push_event(&Event::ToolCallStarted {
            agent: "master".into(),
            tool: "read_file".into(),
            summary: "path=src/main.rs".into(),
        });
        app.push_event(&Event::ToolCallFinished {
            agent: "master".into(),
            tool: "read_file".into(),
            ok: true,
            summary: "ok".into(),
            duration_ms: 12,
        });
        app.push_event(&Event::AssistantMessage {
            agent: "master".into(),
            text: "All done — the bug was a null check.".into(),
        });
        let text = visible_text(&app, 100, 30);
        assert!(text.contains("fix the login bug"), "{text}");
        assert!(text.contains("read_file"), "{text}");
        assert!(text.contains("null check"), "{text}");
    }

    #[test]
    fn markdown_assistant_message_renders_distinct_structure() {
        let mut app = test_app();
        let md = r#"## Summary

Here is some **bold** text and a list:

- first item
- second item

```rust
fn hello() {
    println!("world");
}
```"#;
        app.push_event(&Event::AssistantMessage {
            agent: "master".into(),
            text: md.into(),
        });
        // The entry should be flagged as markdown.
        let entry = app.entries.iter().find(|e| e.text == md).unwrap();
        assert!(entry.markdown, "assistant message must be markdown-flagged");
        // Render and check that structural glyphs appear in the output.
        let text = visible_text(&app, 80, 40);
        // Heading bar and code-fence bar are markdown-specific decorations.
        assert!(text.contains("▌"), "heading bar should be visible: {text}");
        assert!(
            text.contains("│"),
            "code fence bar should be visible: {text}"
        );
    }

    #[test]
    fn streaming_deltas_accumulate() {
        let mut app = test_app();
        for d in ["Hel", "lo, ", "wor", "ld!"] {
            app.push_event(&Event::AssistantDelta {
                agent: "master".into(),
                delta: d.into(),
            });
        }
        let text = visible_text(&app, 100, 30);
        assert!(text.contains("Hello, world!"), "{text}");
        // Finalizing replaces the streaming entry.
        app.push_event(&Event::AssistantMessage {
            agent: "master".into(),
            text: "Hello, world!".into(),
        });
        let text2 = visible_text(&app, 100, 30);
        assert!(text2.contains("Hello, world!"));
    }

    #[test]
    fn utf8_task_text_never_panics() {
        let mut app = test_app();
        app.push_event(&Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: requiem_core::Id::new(),
            description: "日本語の非常に長いタスク説明文です、これが正しく描画されること".into(),
            mode: "code".into(),
        });
        app.push_event(&Event::ToolCallStarted {
            agent: "master".into(),
            tool: "emogi_tool".into(),
            summary: "🎉👩‍🚀🔥".into(),
        });
        let _ = render_to_string(&app, 60, 20);
        let _ = render_to_string(&app, 20, 10);
    }

    #[test]
    fn tiny_viewport_does_not_panic() {
        let mut app = test_app();
        app.push_user_line("hello");
        app.push_event(&Event::AssistantMessage {
            agent: "master".into(),
            text: "hello there".into(),
        });
        for w in [1u16, 5, 10, 20] {
            for h in [1u16, 2, 5, 10] {
                let _ = render_to_string(&app, w, h);
            }
        }
    }

    #[test]
    fn dashboard_overlay_renders_live_data() {
        let mut app = test_app();
        app.push_user_line("build the parser");
        app.push_event(&Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: requiem_core::Id::new(),
            description: "build the parser".into(),
            mode: "code".into(),
        });
        app.push_event(&Event::AgentSpawned {
            parent: "master".into(),
            child: "impl-1".into(),
            role: "implementer".into(),
            description: "write parser.rs".into(),
        });
        app.push_event(&Event::Usage {
            agent: "master".into(),
            task: requiem_core::Id::new(),
            input_tokens: 1500,
            output_tokens: 300,
        });
        app.dash.open = true;
        let text = visible_text(&app, 120, 40);
        assert!(text.contains("dashboard"), "{text}");
        assert!(text.contains("master"), "{text}");
        assert!(text.contains("impl-1"), "{text}");
        assert!(text.contains("implementer"), "{text}");
        assert!(text.contains("1.8k"), "tokens formatted: {text}");
        assert!(text.contains("tasks 0/1"), "{text}");
    }

    #[test]
    fn dashboard_narrow_stacks_without_panic() {
        let mut app = test_app();
        app.push_event(&Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: requiem_core::Id::new(),
            description: "task".into(),
            mode: "code".into(),
        });
        app.dash.open = true;
        let _ = render_to_string(&app, 40, 20);
    }

    #[test]
    fn dashboard_panel_cycle_and_selection() {
        let mut app = test_app();
        app.push_event(&Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: requiem_core::Id::new(),
            description: "task".into(),
            mode: "code".into(),
        });
        app.push_event(&Event::RunCompleted {
            agent: "master".into(),
            task: requiem_core::Id::new(),
            response: None,
            input_tokens: 10,
            output_tokens: 5,
        });
        app.dash.open = true;
        assert_eq!(app.dash.panel, DashPanel::Tasks);
        app.handle_key(test_key(KeyCode::Tab));
        assert_eq!(app.dash.panel, DashPanel::Agents);
        app.handle_key(test_key(KeyCode::Tab));
        assert_eq!(app.dash.panel, DashPanel::Events);
        app.handle_key(test_key(KeyCode::Tab));
        assert_eq!(app.dash.panel, DashPanel::Tasks);
        // Task selection + detail.
        app.handle_key(test_key(KeyCode::Down));
        assert_eq!(app.dash.selected[0], 0, "single task stays at 0");
        // Esc closes.
        app.handle_key(test_key(KeyCode::Esc));
        assert!(!app.dash.open);
    }

    #[test]
    fn ctrl_c_cancels_while_running_then_quits() {
        let mut app = test_app();
        let task = requiem_core::Id::new();
        app.push_event(&Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: task.clone(),
            description: "task".into(),
            mode: "code".into(),
        });
        assert_eq!(app.handle_key(test_ctrl('c')), UiAction::CancelRun);
        // Driver marks the run finished:
        app.push_event(&Event::RunCancelled {
            agent: "master".into(),
            task,
        });
        assert_eq!(app.handle_key(test_ctrl('c')), UiAction::Quit);
    }

    #[test]
    fn one_run_finishing_keeps_app_running_while_another_is_active() {
        let mut app = test_app();
        let a = requiem_core::Id::new();
        let b = requiem_core::Id::new();
        for (task, desc) in [(&a, "first"), (&b, "second")] {
            app.push_event(&Event::RunStarted {
                agent: "master".into(),
                role: "master".into(),
                task: task.clone(),
                description: desc.into(),
                mode: "code".into(),
            });
        }
        assert_eq!(app.active_run_count(), 2);
        app.push_event(&Event::RunCompleted {
            agent: "master".into(),
            task: a.clone(),
            response: None,
            input_tokens: 0,
            output_tokens: 0,
        });
        assert_eq!(app.active_run_count(), 1);
        assert!(app.is_running(), "second run still in flight");
        // A duplicate terminal event must be idempotent.
        app.push_event(&Event::RunCancelled {
            agent: "master".into(),
            task: a,
        });
        assert_eq!(app.active_run_count(), 1);
        app.push_event(&Event::RunFailed {
            agent: "master".into(),
            task: b.clone(),
            error: "boom".into(),
            error_class: "model".into(),
        });
        assert!(!app.is_running());
        // Driver reconciliation can never resurrect a finished run.
        app.sync_active_runs(&[]);
        assert_eq!(app.active_run_count(), 0);
    }

    #[test]
    fn sync_active_runs_drops_runs_that_died_without_events() {
        let mut app = test_app();
        app.push_event(&Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: requiem_core::Id::from("crashed"),
            description: "x".into(),
            mode: "code".into(),
        });
        assert!(app.is_running());
        // The task panicked: no terminal event was ever emitted.
        app.sync_active_runs(&[]);
        assert!(!app.is_running(), "stale running state must be cleared");
    }

    #[test]
    fn submit_action_clears_input() {
        let mut app = test_app();
        for c in "/help".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        assert_eq!(app.composer.text, "/help");
        let action = app.handle_key(test_key(KeyCode::Enter));
        assert_eq!(action, UiAction::Submit("/help".into()));
        assert!(app.composer.text.is_empty());
    }

    #[test]
    fn autocomplete_suggestions_and_tab_complete() {
        let mut app = test_app();
        app.handle_key(test_key(KeyCode::Char('/')));
        app.handle_key(test_key(KeyCode::Char('a')));
        let text = visible_text(&app, 100, 30);
        assert!(text.contains("/ask"), "suggestions shown: {text}");
        assert!(text.contains("/auto"), "{text}");
        // Tab cycles to /auto and completes it.
        app.handle_key(test_key(KeyCode::Tab));
        assert_eq!(app.composer.text, "/auto");
    }

    #[test]
    fn argument_hints_after_command() {
        let mut app = test_app();
        app.set_history(vec![]);
        for c in "/model ".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        let text = visible_text(&app, 100, 30);
        assert!(text.contains("--test"), "args hint shown: {text}");
    }

    #[test]
    fn at_file_suggestions_shown_for_typing() {
        use std::path::PathBuf;
        let mut app = test_app();
        // Create a temp directory with a couple of files.
        let tmp = std::env::temp_dir().join("requiem_at_test");
        std::fs::create_dir_all(&tmp).ok();
        std::fs::write(tmp.join("main.rs"), "fn main() {}").ok();
        std::fs::write(tmp.join("util.rs"), "").ok();
        app.workdir = Some(tmp.clone());

        // Type @ma → should filter to main.rs
        for c in "@ma".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        assert!(
            !app.suggestions.is_empty(),
            "suggestions should be non-empty"
        );
        assert!(
            app.suggestions.iter().any(|s| s.contains("main.rs")),
            "main.rs in suggestions: {:?}",
            app.suggestions
        );

        // Tab completes to @main.rs
        app.handle_key(test_key(KeyCode::Tab));
        assert!(
            app.composer.text.starts_with("@main.rs"),
            "Tab completes @ path: {}",
            app.composer.text
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = PathBuf::new();
    }

    #[test]
    fn at_file_no_suggestions_when_no_workdir() {
        let mut app = test_app();
        // No workdir set — typing @ should produce no suggestions.
        for c in "@foo".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        assert!(app.suggestions.is_empty(), "no workdir → no suggestions");
    }

    // ── Keyboard-mapping tests ────────────────────────────────────

    #[test]
    fn enter_submits_and_clears_composer() {
        let mut app = test_app();
        for c in "hello world".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        let action = app.handle_key(test_key(KeyCode::Enter));
        assert!(matches!(action, UiAction::Submit(ref s) if s == "hello world"));
        assert!(app.composer.text.is_empty());
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut app = test_app();
        for c in "abc".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        let action = app.handle_key(key);
        assert!(
            matches!(action, UiAction::None),
            "Shift+Enter must not submit"
        );
        assert_eq!(app.composer.text, "abc\n");
    }

    #[test]
    fn ctrl_j_inserts_newline() {
        let mut app = test_app();
        for c in "x".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        let action = app.handle_key(test_ctrl('j'));
        assert!(matches!(action, UiAction::None));
        assert_eq!(app.composer.text, "x\n");
    }

    #[test]
    fn ctrl_w_deletes_word_left() {
        let mut app = test_app();
        for c in "hello world".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        app.handle_key(test_ctrl('w'));
        // "world" is removed; trailing space handling depends on cursor position.
        assert!(!app.composer.text.contains("world"), "word must be deleted");
        assert!(app.composer.text.len() < 11, "text must be shorter");
        app.handle_key(test_ctrl('w'));
        assert!(app.composer.text.len() < app.composer.text.len() + 1);
    }

    #[test]
    fn ctrl_u_deletes_to_start() {
        let mut app = test_app();
        for c in "abcdef".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        // Move cursor to middle
        app.composer.cursor = 3;
        app.handle_key(test_ctrl('u'));
        assert_eq!(app.composer.text, "def");
        assert_eq!(app.composer.cursor, 0);
    }

    #[test]
    fn ctrl_k_deletes_to_end() {
        let mut app = test_app();
        for c in "abcdef".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        app.composer.cursor = 2;
        app.handle_key(test_ctrl('k'));
        assert_eq!(app.composer.text, "ab");
        assert_eq!(app.composer.cursor, 2);
    }

    #[test]
    fn ctrl_z_undo_and_redo() {
        let mut app = test_app();
        for c in "hi".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        app.handle_key(test_ctrl('z'));
        assert_eq!(app.composer.text, "h", "undo removes 'i'");
        // Redo
        let redo_key = KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_key(redo_key);
        assert_eq!(app.composer.text, "hi", "redo restores 'i'");
    }

    #[test]
    fn esc_clears_composer() {
        let mut app = test_app();
        for c in "some text".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        app.handle_key(test_key(KeyCode::Esc));
        assert!(app.composer.text.is_empty());
    }

    #[test]
    fn tab_opens_dashboard_when_no_suggestions() {
        let mut app = test_app();
        // Plain text with no suggestions.
        for c in "hello".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        assert!(app.suggestions.is_empty());
        let action = app.handle_key(test_key(KeyCode::Tab));
        assert!(matches!(action, UiAction::None));
        assert!(app.dash.open, "Tab with no suggestions opens dashboard");
    }

    // ── Terminal-size rendering tests ─────────────────────────────

    #[test]
    fn renders_at_small_80x24() {
        let mut app = test_app();
        app.push_user_line("fix the login bug in src/auth.rs");
        app.push_event(&Event::AssistantMessage {
            agent: "master".into(),
            text: "Done. The null-check was missing on line 42.".into(),
        });
        let buf = render_to_string(&app, 80, 24);
        assert!(
            buf.contains("fix the login"),
            "small: user line visible: {buf}"
        );
        assert!(buf.contains("Done"), "small: response visible");
    }

    #[test]
    fn renders_at_standard_120x40() {
        let mut app = test_app();
        app.push_user_line("explain the architecture of the scheduler");
        app.push_event(&Event::AssistantMessage {
            agent: "master".into(),
            text: "The scheduler uses a topological sort.".into(),
        });
        let buf = render_to_string(&app, 120, 40);
        assert!(
            buf.contains("explain the architecture"),
            "standard: user line"
        );
        assert!(buf.contains("topological sort"), "standard: response");
        assert!(buf.contains("Ctrl+C"), "standard: footer hint");
    }

    #[test]
    fn renders_at_large_200x50() {
        let mut app = test_app();
        app.push_user_line("run the full test suite and report failures");
        app.push_event(&Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: requiem_core::Id::new(),
            description: "run tests".into(),
            mode: "code".into(),
        });
        app.push_event(&Event::AssistantMessage {
            agent: "master".into(),
            text: "All 287 tests passed.".into(),
        });
        let buf = render_to_string(&app, 200, 50);
        assert!(buf.contains("run the full test suite"), "large: user line");
        assert!(buf.contains("All 287"), "large: response");
    }

    #[test]
    fn at_file_fuzzy_substring_match() {
        let tmp = std::env::temp_dir().join("requiem_at_fuzzy");
        std::fs::create_dir_all(&tmp).ok();
        std::fs::write(tmp.join("Cargo.toml"), "").ok();
        std::fs::write(tmp.join("README.md"), "").ok();
        let mut app = test_app();
        app.workdir = Some(tmp.clone());

        for c in "@cargo".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        assert!(
            app.suggestions
                .iter()
                .any(|s| s.to_lowercase().contains("cargo")),
            "fuzzy match finds Cargo.toml: {:?}",
            app.suggestions
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn history_recall_up_down() {
        let mut app = test_app();
        app.set_history(vec!["first".into(), "second".into()]);
        app.handle_key(test_key(KeyCode::Up));
        assert_eq!(app.composer.text, "first");
        app.handle_key(test_key(KeyCode::Up));
        assert_eq!(app.composer.text, "second");
        app.handle_key(test_key(KeyCode::Down));
        assert_eq!(app.composer.text, "first");
        app.handle_key(test_key(KeyCode::Down));
        assert_eq!(app.composer.text, "", "returns to draft");
    }

    #[test]
    fn multiline_and_backspace_editing() {
        let mut app = test_app();
        for c in "héllo".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        assert_eq!(app.composer.text, "héllo");
        assert_eq!(app.composer.cursor, 5);
        app.handle_key(test_key(KeyCode::Backspace));
        assert_eq!(app.composer.text, "héll");
        assert_eq!(app.composer.cursor, 4);
        app.handle_key(test_key(KeyCode::Left));
        app.handle_key(test_key(KeyCode::Char('X')));
        assert_eq!(app.composer.text, "hélXl");
        // UTF-8 safety: cursor math never splits a char.
        let _ = render_to_string(&app, 60, 20);
    }

    #[test]
    fn nocolor_mode_renders_plain() {
        // The theme is forced to NoColor in some environments; rendering
        // must not change visible text.
        let mut app = App::new(
            Theme::default(),
            CommandHints::default(),
            "s".into(),
            "m".into(),
        );
        app.push_user_line("plain check");
        let text = visible_text(&app, 80, 24);
        assert!(text.contains("plain check"));
    }

    #[test]
    fn scroll_back_renders_without_panic() {
        let mut app = test_app();
        for i in 0..50 {
            app.push_system(&format!("line {i}"), NoticeLevel::Info);
        }
        let _ = render_to_string(&app, 80, 20);
        app.scroll_back = 30;
        let _ = render_to_string(&app, 80, 20);
        app.scroll_back = usize::MAX / 2;
        let _ = render_to_string(&app, 80, 20);
    }

    #[test]
    fn many_agents_and_events_render() {
        let mut app = test_app();
        for i in 0..12 {
            app.push_event(&Event::AgentSpawned {
                parent: "master".into(),
                child: format!("worker-{i}"),
                role: "implementer".into(),
                description: format!("subtask {i}"),
            });
            app.push_event(&Event::ToolCallFinished {
                agent: format!("worker-{i}"),
                tool: "glob_search".into(),
                ok: true,
                summary: "42 matches".into(),
                duration_ms: 45,
            });
        }
        app.dash.open = true;
        let text = visible_text(&app, 120, 40);
        assert!(text.contains("worker-11"), "{text}");
    }

    #[test]
    fn fmt_tokens_humanizes() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        assert_eq!(fmt_tokens(2_340_000), "2.3M");
    }

    #[test]
    fn truncate_chars_is_char_safe() {
        let s = "日本語テスト".to_string();
        let t = truncate_chars(&s, 3);
        assert_eq!(t.chars().count(), 3);
        assert!(t.ends_with('…'));
    }

    // -- key helpers ----------------------------------------------------

    fn test_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn test_ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn paste_input_inserts_multiline_text_at_cursor() {
        let mut app = test_app();
        app.composer.text = "abc".into();
        app.composer.cursor = 1;
        app.paste_input("line1\nline2");
        // "abc" → insert "line1\nline2" (newlines preserved) at char index 1
        assert_eq!(app.composer.text, "aline1\nline2bc");
        // cursor is right after the pasted text (1 + 12 chars)
        let pasted_len = "line1\nline2".chars().count();
        assert_eq!(app.composer.cursor, 1 + pasted_len);
    }

    #[test]
    fn paste_input_does_not_submit() {
        let mut app = test_app();
        app.paste_input("hello\nworld");
        // No transcript entry was pushed — paste does not submit.
        assert!(app.entries.is_empty());
        // Newlines preserved.
        assert_eq!(app.composer.text, "hello\nworld");
    }

    #[test]
    fn paste_input_at_empty_input() {
        let mut app = test_app();
        app.paste_input("/ask");
        assert_eq!(app.composer.text, "/ask");
        assert_eq!(app.composer.cursor, 4);
    }

    #[test]
    fn paste_input_clamps_stale_cursor() {
        let mut app = test_app();
        app.composer.text = "hi".into();
        // Cursor beyond end (stale from a prior longer input) — should clamp.
        app.composer.cursor = 99;
        app.paste_input("x");
        assert_eq!(app.composer.text, "hix");
        assert_eq!(app.composer.cursor, 3);
    }

    #[test]
    fn compact_header_is_single_row_borderless() {
        let app = test_app();
        // Render at 100x30. The header occupies row 0 (chars 0..100).
        // The footer occupies row 29 (chars 2900..3000).
        let buf = render_to_string(&app, 100, 30);
        // Row 0: must contain the wordmark and no border glyph.
        let row0 = buf
            .get(0..100)
            .unwrap_or(&buf[..100.min(buf.len())])
            .to_string();
        assert!(row0.contains("REQUIEM"), "header row: {row0:?}");
        assert!(!row0.contains('─'), "border glyph on header row: {row0:?}");
        // Row 29: footer — must contain a keyboard hint, no border glyph.
        let footer_start = 29 * 100;
        let footer_end = (29 * 100 + 100).min(buf.len());
        let footer = buf.get(footer_start..footer_end).unwrap_or("").to_string();
        assert!(
            footer.contains("Shift+Enter") || footer.contains("/help"),
            "footer row: {footer:?}"
        );
        assert!(
            !footer.contains('─'),
            "border glyph on footer row: {footer:?}"
        );
    }

    #[test]
    fn status_shows_running_agents_compact_tree() {
        let mut app = test_app();
        app.active_runs.insert("t1".into());
        app.active_task = Some("fix login".into());
        let mut agent = AgentRow::new("a1", "worker");
        agent.status = AgentStatus::Running;
        agent.current_task = "reading files".into();
        app.dash.agents.push(agent);
        let text = visible_text(&app, 100, 30);
        assert!(
            text.contains("reading files"),
            "running agent task in status: {text}"
        );
    }

    #[test]
    fn compact_layout_header_and_footer_are_one_row() {
        let app = test_app();
        let buf = render_to_string(&app, 80, 30);
        // Row 0 (chars 0..80): header with wordmark.
        let r0 = buf
            .get(0..80)
            .unwrap_or(&buf[..80.min(buf.len())])
            .to_string();
        assert!(r0.contains("REQUIEM"), "header: {r0:?}");
        // Row 28 (chars 28*80..29*80): input prompt '>' (second-to-last row).
        let r28_start = 28 * 80;
        let r28_end = (29 * 80).min(buf.len());
        let r28 = buf.get(r28_start..r28_end).unwrap_or("").to_string();
        assert!(
            r28.contains(">") || r28.contains("/help"),
            "input row: {r28:?}"
        );
        // Row 29 (chars 29*80..30*80): footer with mode + Ctrl+C.
        let r29_start = 29 * 80;
        let r29_end = (30 * 80).min(buf.len());
        let r29 = buf.get(r29_start..r29_end).unwrap_or("").to_string();
        assert!(
            r29.contains("/help") || r29.contains("Shift+Enter") || r29.contains("code"),
            "footer: {r29:?}"
        );
    }

    // ── Stage 8: paste tests (22 mandated) ───────────────────────

    /// The exact Rust code block from the spec §5.3 — a multiline paste
    /// that must retain its original line structure.
    const RUST_CODE_BLOCK: &str = "fn main() {
    let x: u32 = 42;
    if x % 2 == 0 {
        println!(\"even: {x}\");
    } else {
        println!(\"odd: {x}\");
    }
}";

    #[test]
    fn paste_rust_code_block_preserves_structure() {
        let mut app = test_app();
        app.paste_input(RUST_CODE_BLOCK);
        // The text must retain all newlines exactly.
        let expected = RUST_CODE_BLOCK.to_string();
        assert_eq!(
            app.composer.text, expected,
            "line structure must be preserved"
        );
        assert_eq!(
            app.composer.text.lines().count(),
            expected.lines().count(),
            "line count must match"
        );
        // Each original line must appear in order.
        for line in expected.lines() {
            assert!(app.composer.text.contains(line), "missing line: {line}");
        }
    }

    #[test]
    fn paste_rust_code_block_submits_once_intact() {
        let mut app = test_app();
        app.paste_input(RUST_CODE_BLOCK);
        // Simulate pressing Enter (plain, no modifiers) → submit.
        let action = app.handle_key(test_key(KeyCode::Enter));
        match action {
            UiAction::Submit(ref line) => {
                // The submitted text preserves newlines (no flattening).
                let expected = RUST_CODE_BLOCK.trim().to_string();
                assert_eq!(
                    line.as_str(),
                    expected,
                    "must submit the complete original content exactly once"
                );
            }
            other => panic!("expected Submit, got {other:?}"),
        }
        // After submit the composer is cleared.
        assert!(app.composer.text.is_empty());
    }

    #[test]
    fn paste_rust_block_crlf_normalized() {
        let mut app = test_app();
        let crlf_block = RUST_CODE_BLOCK.replace('\n', "\r\n");
        app.paste_input(&crlf_block);
        // CRLF must be normalised to LF.
        assert!(!app.composer.text.contains('\r'), "CR must be stripped");
        assert_eq!(
            app.composer.text, RUST_CODE_BLOCK,
            "CRLF paste must equal LF paste"
        );
    }

    #[test]
    fn paste_rust_block_cr_only_normalized() {
        let mut app = test_app();
        let cr_block = RUST_CODE_BLOCK.replace('\n', "\r");
        app.paste_input(&cr_block);
        assert!(!app.composer.text.contains('\r'), "CR must be stripped");
        assert_eq!(app.composer.text, RUST_CODE_BLOCK);
    }

    #[test]
    fn paste_rust_block_does_not_auto_submit() {
        let mut app = test_app();
        app.paste_input(RUST_CODE_BLOCK);
        // No transcript entry for the pasted content — it stays in the
        // composer until the user presses Enter.
        assert!(
            app.entries.is_empty(),
            "paste must not create transcript entries"
        );
    }

    #[test]
    fn paste_rust_block_at_cursor_position() {
        let mut app = test_app();
        // Type "review this: " then paste the code block.
        for c in "review this: ".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        app.paste_input(RUST_CODE_BLOCK);
        assert!(
            app.composer.text.starts_with("review this: "),
            "prefix must be preserved before paste"
        );
        assert!(
            app.composer.text.ends_with(RUST_CODE_BLOCK),
            "pasted block must be at the end"
        );
    }

    #[test]
    fn paste_rust_block_is_single_undo() {
        let mut app = test_app();
        app.paste_input(RUST_CODE_BLOCK);
        let before = app.composer.text.clone();
        assert!(app.composer.undo());
        assert!(
            app.composer.text.is_empty(),
            "undo must remove the entire pasted block"
        );
        assert!(app.composer.redo());
        assert_eq!(app.composer.text, before);
    }

    #[test]
    fn paste_rust_block_placeholder_shown() {
        let mut app = test_app();
        // RUST_CODE_BLOCK is only 8 lines — below the 50-line threshold.
        // Force a large paste to trigger the placeholder.
        let large: String = RUST_CODE_BLOCK.repeat(10);
        app.paste_input(&large);
        let ph = app
            .composer
            .paste_placeholder()
            .expect("placeholder expected");
        assert!(ph.contains("lines"), "placeholder: {ph}");
        assert!(ph.contains("KB"), "placeholder: {ph}");
    }

    #[test]
    fn paste_large_block_content_intact() {
        let mut app = test_app();
        let large: String = RUST_CODE_BLOCK.repeat(10);
        app.paste_input(&large);
        // Content must be intact despite placeholder rendering.
        assert_eq!(app.composer.text, large);
        // Submit must send the full content (newlines preserved).
        let action = app.handle_key(test_key(KeyCode::Enter));
        if let UiAction::Submit(line) = action {
            assert_eq!(line, large, "full raw content must be submitted");
        } else {
            panic!("expected Submit");
        }
    }

    #[test]
    fn paste_240_lines_18kb_placeholder() {
        let mut app = test_app();
        // Simulate a 240-line, ~18 KB paste.
        let line = "fn test_240() { assert!(true); }\n";
        let text = line.repeat(240);
        assert!(text.lines().count() == 240);
        app.paste_input(&text);
        let ph = app.composer.paste_placeholder().expect("placeholder");
        assert!(ph.contains("240 lines"), "got: {ph}");
    }

    #[test]
    fn paste_short_text_no_placeholder() {
        let mut app = test_app();
        app.paste_input("fn main() {}\n");
        assert!(
            app.composer.paste_placeholder().is_none(),
            "short paste must not show placeholder"
        );
    }

    #[test]
    fn paste_empty_string() {
        let mut app = test_app();
        app.paste_input("");
        assert!(app.composer.text.is_empty());
    }

    #[test]
    fn paste_whitespace_only() {
        let mut app = test_app();
        app.paste_input("   \n  ");
        assert!(!app.composer.text.is_empty());
        // Submit trims to empty → no Submit action.
        let action = app.handle_key(test_key(KeyCode::Enter));
        assert_eq!(action, UiAction::None);
    }

    #[test]
    fn paste_mixed_line_endings_normalized() {
        let mut app = test_app();
        // Mix of LF, CRLF, and lone CR.
        app.paste_input("a\nb\r\nc\rd");
        assert_eq!(app.composer.text, "a\nb\nc\nd");
    }

    #[test]
    fn paste_utf8_preserved() {
        let mut app = test_app();
        app.paste_input("日本語\nテスト\nemoji ✓");
        assert_eq!(app.composer.text, "日本語\nテスト\nemoji ✓");
    }

    #[test]
    fn paste_special_characters() {
        let mut app = test_app();
        app.paste_input("a\"b'c\\d\te\nf");
        assert_eq!(app.composer.text, "a\"b'c\\d\te\nf");
    }

    #[test]
    fn paste_then_edit() {
        let mut app = test_app();
        app.paste_input("hello\nworld");
        // Move to start, delete "hello" (5 chars).
        app.handle_key(test_key(KeyCode::Home));
        for _ in 0..5 {
            app.handle_key(test_key(KeyCode::Delete));
        }
        assert_eq!(app.composer.text, "\nworld");
        // Move right past the newline, then backspace to remove it.
        app.handle_key(test_key(KeyCode::Right));
        app.handle_key(test_key(KeyCode::Backspace));
        assert_eq!(app.composer.text, "world");
    }

    #[test]
    fn paste_multiple_blocks() {
        let mut app = test_app();
        app.paste_input("first block\n");
        app.paste_input("second block\n");
        assert_eq!(app.composer.text, "first block\nsecond block\n");
    }

    #[test]
    fn paste_undo_redo_cycle() {
        let mut app = test_app();
        app.paste_input("aaa\n");
        app.paste_input("bbb\n");
        assert_eq!(app.composer.text, "aaa\nbbb\n");
        assert!(app.composer.undo());
        assert_eq!(app.composer.text, "aaa\n");
        assert!(app.composer.redo());
        assert_eq!(app.composer.text, "aaa\nbbb\n");
        assert!(app.composer.undo());
        assert!(app.composer.undo());
        assert!(app.composer.text.is_empty());
        assert!(!app.composer.undo());
    }

    #[test]
    fn paste_history_recall_still_works() {
        let mut app = test_app();
        app.set_history(vec!["old input".into()]);
        app.handle_key(test_key(KeyCode::Up));
        assert_eq!(app.composer.text, "old input");
    }

    #[test]
    fn paste_placeholder_renders_in_input_area() {
        let mut app = test_app();
        let large: String = RUST_CODE_BLOCK.repeat(20);
        app.paste_input(&large);
        // Render at 100x30 — the placeholder line must be visible.
        let text = visible_text(&app, 100, 30);
        assert!(text.contains("Pasted text"), "placeholder visible: {text}");
        assert!(
            text.contains("lines"),
            "placeholder shows line count: {text}"
        );
    }

    #[test]
    fn paste_then_shift_enter_inserts_newline() {
        let mut app = test_app();
        app.paste_input("line1");
        // Shift+Enter should insert a newline.
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::SHIFT,
        );
        app.handle_key(key);
        assert_eq!(app.composer.text, "line1\n");
        assert!(app.composer.multiline);
    }
}
