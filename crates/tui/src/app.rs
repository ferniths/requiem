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
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, Paragraph, Wrap};
use ratatui::Frame;

use requiem_core::events::{Event, NoticeLevel};

use crate::dashboard::{AgentRow, AgentStatus, TaskRow, TaskStatus};
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
        self.agents
            .iter()
            .map(|a| a.tokens_in + a.tokens_out)
            .sum()
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
    /// Current mode name (without slash).
    pub mode: String,

    // Session screen state
    entries: Vec<TranscriptEntry>,
    input: String,
    cursor: usize,
    suggestions: Vec<String>,
    suggestion_idx: usize,
    history: Vec<String>,
    history_idx: Option<usize>,
    history_draft: String,
    scroll_back: usize,

    // Runtime-derived status
    started: Instant,
    running: bool,
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
            mode: "code".to_string(),
            entries: Vec::new(),
            input: String::new(),
            cursor: 0,
            suggestions: Vec::new(),
            suggestion_idx: 0,
            history: Vec::new(),
            history_idx: None,
            history_draft: String::new(),
            scroll_back: 0,
            started: Instant::now(),
            running: false,
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

    /// Record a user-submitted line in the transcript and history.
    pub fn push_user_line(&mut self, text: &str) {
        self.entries.push(TranscriptEntry {
            glyph: "›",
            text: text.to_string(),
            fg: Token::AccentBright,
            bold: true,
            indent: 4,
            streaming: false,
        });
        self.history.insert(0, text.to_string());
        self.history.truncate(200);
        self.history_idx = None;
        self.scroll_back = 0;
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
        self.scroll_back = 0;
    }

    // ── Event consumption ────────────────────────────────────────

    /// Mark whether a run is in flight (used when a run crashes without
    /// emitting its completion event).
    pub fn set_running(&mut self, running: bool) {
        self.running = running;
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
                self.running = true;
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
                    self.entries
                        .push(TranscriptEntry::new("◆", text.clone(), Token::Text));
                }
            }
            Event::ToolCallStarted { agent, tool, summary } => {
                self.entries.push(TranscriptEntry::new(
                    "⚙",
                    format!("{tool}  {summary}"),
                    Token::TextSecondary,
                ));
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
                self.entries.push(TranscriptEntry::new(
                    glyph,
                    format!("{tool} ({duration_ms}ms)  {summary}"),
                    fg,
                ));
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
                self.running = false;
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
                            self.entries
                                .push(TranscriptEntry::new("◆", text.clone(), Token::Text));
                        }
                    }
                }
                self.active_task = None;
            }
            Event::RunFailed { agent, task, error, .. } => {
                self.running = false;
                if let Some(i) = self.dash.agent_index(agent) {
                    self.dash.agents[i].status = AgentStatus::Failed;
                }
                if let Some(i) = self.dash.task_index(task.as_str()) {
                    self.dash.tasks[i].status = TaskStatus::Failed;
                }
                self.entries
                    .push(TranscriptEntry::new("✗", format!("task failed: {error}"), Token::Error));
                self.active_task = None;
            }
            Event::RunCancelled { agent, task } => {
                self.running = false;
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
                self.active_task = None;
            }
            Event::Notice { text, level } => self.push_system(text, *level),
        }
    }

    /// A snapshot of the live dashboard state (for persistence/summary).
    pub fn dashboard_snapshot(&self) -> &DashState {
        &self.dash
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
            KeyCode::Esc | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                return if self.running {
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
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return if self.running {
                    UiAction::CancelRun
                } else {
                    UiAction::Quit
                };
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear();
                self.cursor = 0;
                self.refresh_suggestions();
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_word();
            }
            KeyCode::Enter => {
                let line = self.input.trim().to_string();
                self.input.clear();
                self.cursor = 0;
                self.suggestions.clear();
                self.suggestion_idx = 0;
                if line.is_empty() {
                    return UiAction::None;
                }
                return UiAction::Submit(line);
            }
            KeyCode::Tab => {
                if !self.suggestions.is_empty() {
                    // Cycle the selection and complete to it.
                    self.suggestion_idx = (self.suggestion_idx + 1) % self.suggestions.len();
                    let chosen = self.suggestions[self.suggestion_idx].clone();
                    self.input = format!("/{chosen}");
                    self.cursor = self.input.chars().count();
                } else {
                    // No suggestions: Tab opens the dashboard.
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
                self.cursor = self.cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.input.chars().count());
            }
            KeyCode::Home => {
                self.cursor = 0;
            }
            KeyCode::End => {
                self.cursor = self.input.chars().count();
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let idx = char_to_byte(&self.input, self.cursor - 1);
                    self.input.remove(idx);
                    self.cursor -= 1;
                }
                self.refresh_suggestions();
            }
            KeyCode::Delete => {
                if self.cursor < self.input.chars().count() {
                    let idx = char_to_byte(&self.input, self.cursor + 1);
                    self.input.remove(idx);
                }
                self.refresh_suggestions();
            }
            KeyCode::Esc => {
                if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                    self.refresh_suggestions();
                }
            }
            KeyCode::Char(c) => {
                let byte = char_to_byte(&self.input, self.cursor);
                self.input.insert(byte, c);
                self.cursor += 1;
                self.refresh_suggestions();
            }
            _ => {}
        }
        UiAction::None
    }

    fn delete_word(&mut self) {
        // Delete the word left of the cursor.
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        let byte = chars[..i].iter().map(|c| c.len_utf8()).sum();
        self.input.truncate(byte);
        self.cursor = i;
        self.refresh_suggestions();
    }

    fn recall_history(&mut self, dir: i32) {
        if self.history.is_empty() {
            return;
        }
        match self.history_idx {
            None => {
                if dir > 0 {
                    self.history_draft = self.input.clone();
                    self.history_idx = Some(0);
                    self.input = self.history[0].clone();
                }
            }
            Some(i) => {
                let next = if dir > 0 {
                    (i + 1).min(self.history.len() - 1)
                } else if i == 0 {
                    // Restoring the draft.
                    self.history_idx = None;
                    self.input = self.history_draft.clone();
                    self.cursor = self.input.chars().count();
                    return;
                } else {
                    i - 1
                };
                self.history_idx = Some(next);
                self.input = self.history[next].clone();
            }
        }
        self.cursor = self.input.chars().count();
    }

    fn refresh_suggestions(&mut self) {
        self.suggestions.clear();
        self.suggestion_idx = 0;
        let trimmed = self.input.trim_start();
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
        let [header, transcript, input, status] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .areas(area);

        self.render_header(frame, header);
        self.render_transcript(frame, transcript);
        self.render_input(frame, input);
        self.render_status(frame, status);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let t = &self.theme;
        let mut spans = vec![
            Span::styled(" REQUIEM", t.style_bold(Token::Accent)),
            Span::styled(" multi-agent harness", t.style(Token::TextMuted)),
            Span::styled(format!("  session {}", self.session_label), t.style(Token::TextSecondary)),
            Span::styled(format!("  model {}", self.model), t.style(Token::TextSecondary)),
        ];
        spans.push(Span::styled(
            format!("  /{}", self.mode),
            t.style_bold(Token::Violet),
        ));
        if self.running {
            spans.push(Span::styled(
                format!("  {} running", spinner_glyph(self.started)),
                t.style(Token::Info),
            ));
        }
        let header_line = Line::from(spans);
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(t.style(Token::Border));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(header_line).wrap(Wrap { trim: false }),
            inner,
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
            .style(t.style(Token::Background));
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
        lines.push(Line::from(Span::styled(
            " ".repeat(width.max(1)),
            t.style(Token::Background),
        )));
        lines.push(Line::from(Span::styled("", t.style(Token::Background))));

        for e in &self.entries {
            let indent_str = " ".repeat(e.indent.min(width / 2));
            let text_width = width.saturating_sub(e.indent.min(width / 2) + 2);
            let wrapped = wrap_to_width(&e.text, text_width.max(8));
            for (i, chunk) in wrapped.iter().enumerate() {
                let mut spans = Vec::new();
                if i == 0 {
                    spans.push(Span::styled(
                        format!("  {glyph} ", glyph = e.glyph),
                        if e.bold {
                            t.style_bold(e.fg)
                        } else {
                            t.style(e.fg)
                        },
                    ));
                    spans.push(Span::styled(
                        indent_str.clone(),
                        t.style(Token::TextMuted),
                    ));
                } else {
                    spans.push(Span::styled(
                        format!("    {indent_str}"),
                        t.style(Token::TextMuted),
                    ));
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
        // Breathing room at the bottom.
        lines.push(Line::from(Span::styled("", t.style(Token::Background))));
        lines.push(Line::from(Span::styled(
            " ".repeat(width.max(1)),
            t.style(Token::Background),
        )));
        RenderedTranscript { lines }
    }

    fn render_input(&self, frame: &mut Frame, area: Rect) {
        let t = &self.theme;
        let title = if self.input.starts_with('/') {
            " input · command "
        } else {
            " input "
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(t.style(if self.input.starts_with('/') {
                Token::BorderActive
            } else {
                Token::Border
            }))
            .title(Span::styled(title, t.style(Token::TextMuted)))
            .title_alignment(ratatui::layout::Alignment::Left);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let [line_area, hint_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

        // Prompt + typed text.
        let prompt = Span::styled("› ", t.style_bold(Token::Accent));
        let chars: Vec<char> = self.input.chars().collect();
        let before: String = chars[..self.cursor.min(chars.len())].iter().collect();
        let at: String = chars
            .get(self.cursor.min(chars.len()))
            .map(|c| c.to_string())
            .unwrap_or_default();
        let after: String = chars[(self.cursor + 1).min(chars.len())..].iter().collect();
        let mut spans = vec![prompt];
        if !at.is_empty() {
            spans.push(Span::styled(before, t.style(Token::Text)));
            // Block cursor over the character at the caret.
            spans.push(Span::styled(
                at,
                t.style(Token::Background)
                    .bg(t.ratatui_color(Token::Accent)),
            ));
            spans.push(Span::styled(after, t.style(Token::Text)));
        } else {
            spans.push(Span::styled(format!("{before} "), t.style(Token::Text)));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), line_area);

        // Hint line: suggestions, argument hints, or default hints.
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
                    spans.push(Span::styled(format!(" /{s} "), style));
                    spans.push(Span::styled(
                        format!(" {desc}  "),
                        t.style(Token::TextSecondary),
                    ));
                } else {
                    spans.push(Span::styled(format!(" /{s} "), style));
                }
            }
            spans.push(Span::styled(
                " Tab cycle",
                t.style(Token::TextMuted),
            ));
            return spans;
        }
        if let Some((cmd, args)) = self.current_command_args() {
            let mut spans = vec![Span::styled(
                format!("  /{cmd} args: "),
                t.style(Token::TextSecondary),
            )];
            spans.push(Span::styled(
                args.join(" | "),
                t.style(Token::Info),
            ));
            return spans;
        }
        vec![
            Span::styled(
                "  Enter submit · Tab dashboard · ↑ history · /help commands",
                t.style(Token::TextMuted),
            ),
        ]
    }

    /// (command, arg hints) when the input names a known command.
    fn current_command_args(&self) -> Option<(String, Vec<String>)> {
        let trimmed = self.input.trim();
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
        let t = &self.theme;
        let mut spans = vec![
            Span::styled(format!(" /{} ", self.mode), t.style_bold(Token::Violet)),
            Span::styled("·", t.style(Token::TextMuted)),
        ];
        if self.running {
            let task = self.active_task.as_deref().unwrap_or("working");
            let short: String = task.chars().take(40).collect();
            spans.push(Span::styled(
                format!(" {} working: ", spinner_glyph(self.started)),
                t.style(Token::Info),
            ));
            spans.push(Span::styled(short, t.style(Token::TextSecondary)));
            spans.push(Span::styled(
                format!("  turn {}", self.turns),
                t.style(Token::TextMuted),
            ));
        } else {
            spans.push(Span::styled(" idle ", t.style(Token::TextMuted)));
        }
        spans.push(Span::styled(
            format!(
                "  ·  tok {} in / {} out",
                fmt_tokens(self.tokens_in),
                fmt_tokens(self.tokens_out)
            ),
            t.style(Token::TextSecondary),
        ));
        spans.push(Span::styled(
            format!(
                "  ·  Ctrl+C {} · /dashboard",
                if self.running { "cancel" } else { "quit" }
            ),
            t.style(Token::TextMuted),
        ));
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
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .areas(area);

        // Header.
        let (done, total) = self.dash.task_progress();
        let header_line = Line::from(vec![
            Span::styled(" REQUIEM", t.style_bold(Token::Accent)),
            Span::styled(" dashboard", t.style(Token::TextMuted)),
            Span::styled(
                format!("  session {}", self.session_label),
                t.style(Token::TextSecondary),
            ),
            Span::styled(
                format!("  tasks {done}/{total}"),
                t.style(Token::Info),
            ),
            Span::styled(
                format!("  agents {}", self.dash.agents.len()),
                t.style(Token::TextSecondary),
            ),
            Span::styled(
                format!("  tok {}", fmt_tokens(self.dash.total_tokens())),
                t.style(Token::TextSecondary),
            ),
            Span::styled("  Esc close", t.style(Token::TextMuted)),
        ]);
        let hblock = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(t.style(Token::Border));
        let hinner = hblock.inner(header);
        frame.render_widget(hblock, header);
        frame.render_widget(Paragraph::new(header_line).wrap(Wrap { trim: false }), hinner);

        // Progress gauge row on top of the body.
        let ratio = if total == 0 {
            0.0
        } else {
            done as f64 / total as f64
        };
        let [gauge_row, panels] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)])
            .areas(body);
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
        let fblock = Block::default()
            .borders(Borders::TOP)
            .border_style(t.style(Token::Border));
        let finner = fblock.inner(footer);
        frame.render_widget(fblock, footer);
        let [detail_row, hints_row] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(finner);
        frame.render_widget(
            Paragraph::new(Line::from(detail)).wrap(Wrap { trim: false }),
            detail_row,
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Tab panel", t.style(Token::TextMuted)),
                Span::styled(" · ↑↓ select", t.style(Token::TextMuted)),
                Span::styled(" · PgUp/PgDn scroll", t.style(Token::TextMuted)),
                Span::styled(" · Esc close", t.style(Token::TextMuted)),
            ])),
            hints_row,
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
                Span::styled(desc, t.style(if sel { Token::Text } else { Token::TextSecondary })),
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
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((offset, 0)),
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
                    format!(
                        "  t{}/{}",
                        agent.turns_used, agent.turns_max
                    ),
                    t.style(Token::TextMuted),
                ),
                Span::styled(
                    format!(
                        "  tok {}",
                        fmt_tokens(agent.tokens_in + agent.tokens_out)
                    ),
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
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((offset, 0)),
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
            Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((offset, 0)),
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
                            if a.current_task.is_empty() { "-" } else { &a.current_task },
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
        Event::Notice { level: NoticeLevel::Error, .. } => Token::Error,
        Event::Notice { level: NoticeLevel::Success, .. } => Token::Success,
        Event::Notice { level: NoticeLevel::Warning, .. } => Token::Warning,
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

/// Byte offset of char index `idx` in `s` (panics never).
fn char_to_byte(s: &str, idx: usize) -> usize {
    s.char_indices()
        .nth(idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Description text for a command name (from the injected hints).
fn hint_text(name: &str, hints: &CommandHints) -> String {
    hints
        .descriptions
        .get(name)
        .cloned()
        .unwrap_or_default()
}

// ── Tests (TestBackend) ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn test_app() -> App {
        let mut hints = CommandHints::default();
        hints.names = vec![
            "ask".into(),
            "auto".into(),
            "code".into(),
            "dashboard".into(),
            "help".into(),
            "exit".into(),
        ];
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
        assert!(text.contains("REQUIEM"), "header: {text}");
        assert!(text.contains("sess-1234"));
        assert!(text.contains("test-model"));
        assert!(text.contains("›"), "input prompt present");
        assert!(text.contains("/code"), "mode badge present");
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
        assert!(text.contains("1.5k"), "tokens formatted: {text}");
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
        app.push_event(&Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: requiem_core::Id::new(),
            description: "task".into(),
            mode: "code".into(),
        });
        assert_eq!(
            app.handle_key(test_ctrl('c')),
            UiAction::CancelRun
        );
        // Driver marks the run finished:
        app.push_event(&Event::RunCancelled {
            agent: "master".into(),
            task: requiem_core::Id::new(),
        });
        assert_eq!(app.handle_key(test_ctrl('c')), UiAction::Quit);
    }

    #[test]
    fn submit_action_clears_input() {
        let mut app = test_app();
        for c in "/help" {
            app.handle_key(test_key(KeyCode::Char(*c)));
        }
        assert_eq!(app.input, "/help");
        let action = app.handle_key(test_key(KeyCode::Enter));
        assert_eq!(action, UiAction::Submit("/help".into()));
        assert!(app.input.is_empty());
        assert!(app.history().contains(&"/help".to_string()));
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
        assert_eq!(app.input, "/auto");
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
    fn history_recall_up_down() {
        let mut app = test_app();
        app.set_history(vec!["first".into(), "second".into()]);
        app.handle_key(test_key(KeyCode::Up));
        assert_eq!(app.input, "first");
        app.handle_key(test_key(KeyCode::Up));
        assert_eq!(app.input, "second");
        app.handle_key(test_key(KeyCode::Down));
        assert_eq!(app.input, "first");
        app.handle_key(test_key(KeyCode::Down));
        assert_eq!(app.input, "", "returns to draft");
    }

    #[test]
    fn multiline_and_backspace_editing() {
        let mut app = test_app();
        for c in "héllo".chars() {
            app.handle_key(test_key(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "héllo");
        assert_eq!(app.cursor, 5);
        app.handle_key(test_key(KeyCode::Backspace));
        assert_eq!(app.input, "héll");
        assert_eq!(app.cursor, 4);
        app.handle_key(test_key(KeyCode::Left));
        app.handle_key(test_key(KeyCode::Char('X')));
        assert_eq!(app.input, "héXll");
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
}
