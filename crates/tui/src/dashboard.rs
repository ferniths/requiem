//! The live agent dashboard: shows running agents, their current tasks,
//! token usage, and overall session progress.

use serde::{Deserialize, Serialize};

use crate::theme::Theme;

/// The status of one agent in the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    #[default]
    Idle,
    Running,
    Waiting,
    Done,
    Failed,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

/// The glyph shown next to an agent's status in the dashboard.
fn agent_status_glyph(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "○",
        AgentStatus::Running => "▶",
        AgentStatus::Waiting => "⏸",
        AgentStatus::Done => "●",
        AgentStatus::Failed => "✗",
    }
}

/// One row in the dashboard: an agent and its current task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRow {
    pub agent_id: String,
    pub role: String,
    pub status: AgentStatus,
    pub current_task: String,
    pub turns_used: u32,
    pub turns_max: u32,
    pub tokens_in: u32,
    pub tokens_out: u32,
    /// The parent agent ID, empty for top-level (master) agents.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent: String,
}

impl AgentRow {
    pub fn new(agent_id: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            role: role.into(),
            status: AgentStatus::Idle,
            current_task: String::new(),
            turns_used: 0,
            turns_max: 32,
            tokens_in: 0,
            tokens_out: 0,
            parent: String::new(),
        }
    }

    /// Whether this agent is a top-level (master) agent.
    pub fn is_master(&self) -> bool {
        self.parent.is_empty()
    }
}

/// The full dashboard state, rendered by the TUI layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DashboardState {
    pub agents: Vec<AgentRow>,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub session_id: String,
}

impl DashboardState {
    /// Progress as a fraction 0.0..=1.0.
    pub fn progress(&self) -> f32 {
        if self.total_tasks == 0 {
            0.0
        } else {
            (self.completed_tasks as f32) / (self.total_tasks as f32)
        }
    }

    /// Number of agents currently running.
    pub fn running_count(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| a.status == AgentStatus::Running)
            .count()
    }

    /// Total tokens consumed across all agents.
    pub fn total_tokens(&self) -> u32 {
        self.agents.iter().map(|a| a.tokens_in + a.tokens_out).sum()
    }
}

/// A trait for the dashboard renderer. The concrete `ratatui` renderer
/// lives in the CLI layer; this trait allows testing without a terminal.
pub trait DashboardRenderer: Send + Sync {
    /// Render the dashboard to a `String` (plain text, no ANSI).
    fn render(&self, state: &DashboardState) -> String;
}

/// A themed dashboard renderer that uses the REQUIEM design tokens
/// and draws a tree visualization for parent→child agent relationships.
pub struct ThemedRenderer {
    theme: Theme,
}

impl ThemedRenderer {
    pub fn new(theme: &Theme) -> Self {
        Self {
            theme: *theme,
        }
    }

    /// Render the dashboard with theme colors and a tree structure.
    pub fn render_themed(&self, state: &DashboardState) -> String {
        let t = &self.theme;
        let mut out = String::with_capacity(1024);

        // Header with session info.
        let header = t.accent_bold("REQUIEM")
            .to_string()
            + &t.secondary(format!(" session {}", state.session_id)).to_string()
            + &t.muted(format!("  {}/{} tasks", state.completed_tasks, state.total_tasks))
                .to_string()
            + &t
                .info(format!(
                    "  {}/{} agents",
                    state.running_count(),
                    state.agents.len()
                ))
                .to_string();
        out.push_str(&header);
        out.push('\n');

        // Separator.
        out.push_str(&t.border("─".repeat(60)).to_string());
        out.push('\n');

        // Tree visualization: group by parent.
        let agents = &state.agents;
        let masters: Vec<&AgentRow> = agents.iter().filter(|a| a.is_master()).collect();
        let children: Vec<&AgentRow> = agents.iter().filter(|a| !a.is_master()).collect();

        for master in &masters {
            out.push_str(&self.render_agent_row(master, "", false));

            // Find children of this master.
            let my_children: Vec<&AgentRow> = children
                .iter()
                .filter(|c| c.parent == master.agent_id)
                .copied()
                .collect();

            for (i, child) in my_children.iter().enumerate() {
                let is_last = i + 1 == my_children.len();
                let prefix = if is_last { "  └─ " } else { "  ├─ " };
                out.push_str(&self.render_agent_row(child, prefix, true));
            }

            if my_children.is_empty() {
                out.push_str(&t.muted("  └─ (no subagents)").to_string());
                out.push('\n');
            }
        }

        // Orphaned children (no matching master).
        for child in &children {
            let orphaned = masters.iter().all(|m| m.agent_id != child.parent);
            if orphaned {
                out.push_str(&self.render_agent_row(child, "  ?─ ", true));
            }
        }

        // Progress bar.
        let pct = state.progress() * 100.0;
        let bar_len: usize = 24;
        let filled = (pct / 100.0 * bar_len as f32).round() as usize;
        let bar = format!(
            "{}{}",
            "█".repeat(filled),
            "░".repeat(bar_len.saturating_sub(filled))
        );
        out.push_str(&format!(
            "{} {}\n",
            t.secondary("Progress:"),
            t.info(&bar).to_string() + &t.muted(format!(" {pct:.1}%")).to_string()
        ));

        // Token totals.
        let tin: u32 = state.agents.iter().map(|a| a.tokens_in).sum();
        let tout: u32 = state.agents.iter().map(|a| a.tokens_out).sum();
        out.push_str(&format!(
            "{}  in: {}  out: {}  total: {}\n",
            t.secondary("Tokens:"),
            t.info(tin.to_string()),
            t.info(tout.to_string()),
            t.muted((tin + tout).to_string())
        ));

        out
    }

    /// Render one agent row, indented for tree display.
    fn render_agent_row(&self, agent: &AgentRow, prefix: &str, is_child: bool) -> String {
        let t = &self.theme;
        let mut out = String::new();
        out.push_str(prefix);

        // Status indicator: a colored glyph followed by the status label,
        // both in the status color.
        let status_str = agent.status.to_string();
        let token = t.status_token(&status_str);
        let glyph = agent_status_glyph(agent.status);
        out.push_str(&t.paint(token, glyph));
        out.push(' ');
        out.push_str(&t.paint(token, &status_str));
        out.push(' ');

        // Agent ID and role.
        let label = if is_child {
            format!("{} ({})", agent.agent_id, agent.role)
        } else {
            agent.agent_id.clone()
        };
        out.push_str(&t.primary(&label).to_string());

        // Task (truncated on a char boundary — byte slicing panics on
        // multi-byte UTF-8).
        if !agent.current_task.is_empty() {
            let task_display = truncate_str(&agent.current_task, 24);
            out.push_str(&format!("  {}", t.secondary(task_display)));
        }

        // Turns and tokens.
        out.push_str(&format!(
            "  {}  {}\n",
            t.muted(format!("turns {}/{}", agent.turns_used, agent.turns_max)),
            t.muted(format!("tok {}", agent.tokens_in + agent.tokens_out))
        ));

        out
    }
}

impl DashboardRenderer for ThemedRenderer {
    fn render(&self, state: &DashboardState) -> String {
        self.render_themed(state)
    }
}

/// A plain-text dashboard renderer (no ANSI, suitable for tests and
/// non-interactive output).
pub struct PlainRenderer;

impl DashboardRenderer for PlainRenderer {
    fn render(&self, state: &DashboardState) -> String {
        let mut out = String::with_capacity(512);
        out.push_str(&format!(
            "REQUIEM session {} — {}/{} tasks, {}/{} agents running\n",
            state.session_id,
            state.completed_tasks,
            state.total_tasks,
            state.running_count(),
            state.agents.len()
        ));
        out.push_str(&format!(
            "{:<8} {:<14} {:<10} {:<24} {:>6} {:>8}\n",
            "AGENT", "ROLE", "STATUS", "TASK", "TURNS", "TOKENS"
        ));
        out.push_str(&"-".repeat(72));
        out.push('\n');
        for agent in &state.agents {
            // Truncate on a char boundary — byte slicing panics on
            // multi-byte UTF-8.
            let task_display = truncate_str(&agent.current_task, 22);
            out.push_str(&format!(
                "{:<8} {:<14} {:<10} {:<24} {:>6}/{:<4} {:>8}\n",
                agent.agent_id,
                agent.role,
                agent.status,
                task_display,
                agent.turns_used,
                agent.turns_max,
                agent.tokens_in + agent.tokens_out
            ));
        }
        out.push_str(&format!(
            "Progress: {:.1}%  Total tokens: {}\n",
            state.progress() * 100.0,
            state.total_tokens()
        ));
        out
    }
}

// ── Task dashboard ──────────────────────────────────────────────────

/// The status of a task in the task dashboard, mirroring
/// `requiem_core::task::TaskStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    #[default]
    Pending,
    Available,
    Claimed,
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    /// A short label for TUI rendering.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Available => "AVAILABLE",
            Self::Claimed => "CLAIMED",
            Self::Running => "RUNNING",
            Self::Blocked => "BLOCKED",
            Self::Completed => "DONE",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        }
    }
}

/// One row in the task dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
    pub priority: u8,
    /// Short ids of the tasks this one depends on.
    pub dependencies: Vec<String>,
    /// The agent currently assigned, if any.
    pub assignee: Option<String>,
}

/// The full task-dashboard state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskDashboard {
    pub tasks: Vec<TaskRow>,
}

impl TaskDashboard {
    /// Count tasks in each status bucket.
    pub fn counts(&self) -> [usize; 8] {
        let mut c = [0usize; 8];
        for t in &self.tasks {
            c[match t.status {
                TaskStatus::Pending => 0,
                TaskStatus::Available => 1,
                TaskStatus::Claimed => 2,
                TaskStatus::Running => 3,
                TaskStatus::Blocked => 4,
                TaskStatus::Completed => 5,
                TaskStatus::Failed => 6,
                TaskStatus::Cancelled => 7,
            }] += 1;
        }
        c
    }
}

/// Render the task dashboard grouped by status, with priorities and
/// dependency annotations.
pub fn render_task_dashboard(dash: &TaskDashboard, theme: &Theme) -> String {
    use crate::theme::Token;
    let t = theme;
    let mut out = String::with_capacity(1024);

    let counts = dash.counts();

    out.push_str(&t.accent_bold("REQUIEM tasks").to_string());
    // Summary line: e.g. "3 running · 2 pending · 1 failed".
    let summary_items: Vec<(&Token, &str, usize)> = vec![
        (&Token::Info, "running", counts[3]),
        (&Token::TextSecondary, "available", counts[1]),
        (&Token::Warning, "pending", counts[0]),
        (&Token::Warning, "blocked", counts[4]),
        (&Token::Success, "done", counts[5]),
        (&Token::Error, "failed", counts[6]),
        (&Token::TextMuted, "cancelled", counts[7]),
    ];
    let summary: Vec<String> = summary_items
        .iter()
        .filter(|(_, _, n)| *n > 0)
        .map(|(tok, label, n)| {
            let s = format!("{label} {n}");
            match tok {
                Token::Info => t.info(&s).to_string(),
                Token::Warning => t.warning(&s).to_string(),
                Token::Success => t.success(&s).to_string(),
                Token::Error => t.error(&s).to_string(),
                Token::TextMuted => t.muted(&s).to_string(),
                _ => t.secondary(&s).to_string(),
            }
        })
        .collect();
    let summary_str = if summary.is_empty() {
        "(no tasks yet)".to_string()
    } else {
        summary.join(" · ")
    };
    out.push_str(&format!(
        "{}\n",
        t.secondary(format!("  — {} total · {}", dash.tasks.len(), summary_str))
    ));
    out.push('\n');
    out.push_str(&t.border("─".repeat(60)).to_string());
    out.push('\n');

    if dash.tasks.is_empty() {
        out.push_str(&t.muted("  (no tasks in this session yet)").to_string());
        return out;
    }

    // Group by status in a fixed order: active first, terminal last.
    let buckets: [(TaskStatus, &Token); 8] = [
        (TaskStatus::Running, &Token::Info),
        (TaskStatus::Claimed, &Token::TextSecondary),
        (TaskStatus::Available, &Token::TextSecondary),
        (TaskStatus::Blocked, &Token::Warning),
        (TaskStatus::Pending, &Token::Warning),
        (TaskStatus::Completed, &Token::Success),
        (TaskStatus::Failed, &Token::Error),
        (TaskStatus::Cancelled, &Token::TextMuted),
    ];

    for (status, tok) in buckets {
        let rows: Vec<&TaskRow> = dash
            .tasks
            .iter()
            .filter(|r| r.status == status)
            .collect();
        if rows.is_empty() {
            continue;
        }
        let header = format!("{} ({})", status.label(), rows.len());
        out.push_str(&format!(
            "{}\n",
            color_label(&header, *tok, t)
        ));
        // Sort this bucket by priority (highest first), then id.
        let mut rows = rows;
        rows.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
        for row in rows.iter() {
            let prio_tag = if row.priority > 0 {
                format!(" [p{}]", row.priority)
            } else {
                String::new()
            };
            let assignee = row
                .assignee
                .as_ref()
                .map(|a| format!("  {}", t.muted(format!("← {a}"))))
                .unwrap_or_default();
            let deps = if row.dependencies.is_empty() {
                String::new()
            } else {
                let deps_str = row.dependencies.join(", ");
                format!("  {}", t.muted(format!("⧗ {deps_str}")))
            };
            let desc = truncate_str(&row.description, 32);
            out.push_str(&format!(
                "  {}  {}{}{}\n",
                t.muted(&row.id),
                t.secondary(desc),
                t.muted(prio_tag),
                assignee
            ));
            if !deps.is_empty() {
                out.push_str(&deps);
                out.push('\n');
            }
        }
        out.push('\n');
    }

    out
}

/// Color a short status header using the given token.
fn color_label(s: &str, tok: crate::theme::Token, t: &Theme) -> String {
    use crate::theme::Token;
    match tok {
        Token::Info => t.info(s).to_string(),
        Token::Warning => t.warning(s).to_string(),
        Token::Success => t.success(s).to_string(),
        Token::Error => t.error(s).to_string(),
        Token::TextMuted => t.muted(s).to_string(),
        _ => t.secondary(s).to_string(),
    }
}

/// Truncate a string to `max` chars, appending an ellipsis.
fn truncate_str(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> DashboardState {
        DashboardState {
            agents: vec![
                AgentRow {
                    agent_id: "agent-1".into(),
                    role: "implementer".into(),
                    status: AgentStatus::Running,
                    current_task: "Implement the parser".into(),
                    turns_used: 5,
                    turns_max: 32,
                    tokens_in: 100,
                    tokens_out: 50,
                    parent: String::new(),
                },
                AgentRow {
                    agent_id: "agent-2".into(),
                    role: "tester".into(),
                    status: AgentStatus::Done,
                    current_task: "Write unit tests".into(),
                    turns_used: 12,
                    turns_max: 32,
                    tokens_in: 200,
                    tokens_out: 100,
                    parent: "agent-1".into(),
                },
            ],
            total_tasks: 4,
            completed_tasks: 1,
            session_id: "test-session".into(),
        }
    }

    #[test]
    fn progress_is_fraction() {
        let state = sample_state();
        let p = state.progress();
        assert!((p - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn running_count_filters_correctly() {
        let state = sample_state();
        assert_eq!(state.running_count(), 1);
    }

    #[test]
    fn total_tokens_sums_all_agents() {
        let state = sample_state();
        assert_eq!(state.total_tokens(), 450);
    }

    #[test]
    fn plain_renderer_produes_readable_output() {
        let renderer = PlainRenderer;
        let state = sample_state();
        let output = renderer.render(&state);
        assert!(output.contains("REQUIEM session"));
        assert!(output.contains("agent-1"));
        assert!(output.contains("implementer"));
        assert!(output.contains("running"));
        assert!(output.contains("25.0%"));
    }

    #[test]
    fn empty_state_renders_cleanly() {
        let renderer = PlainRenderer;
        let output = renderer.render(&DashboardState::default());
        assert!(output.contains("0/0 tasks"));
    }

    #[test]
    fn themed_renderer_shows_tree() {
        let theme = Theme::default();
        let renderer = ThemedRenderer::new(&theme);
        let state = sample_state();
        let output = renderer.render(&state);
        // Master agent should appear at the top level.
        assert!(output.contains("agent-1"));
        // Child agent should be indented with a tree connector.
        assert!(output.contains("└─"));
        // Progress bar should be present.
        assert!(output.contains("Progress:"));
        // Token totals should be present.
        assert!(output.contains("Tokens:"));
    }

    #[test]
    fn themed_renderer_empty_state() {
        let theme = Theme::default();
        let renderer = ThemedRenderer::new(&theme);
        let output = renderer.render(&DashboardState::default());
        assert!(output.contains("0/0 tasks"));
        assert!(output.contains("0/0 agents"));
    }

    #[test]
    fn renderers_survive_multibyte_task_text() {
        // Byte-slice truncation used to panic on multi-byte characters.
        let theme = Theme::default();
        let mut state = sample_state();
        state.agents[0].current_task = "日本語のタスク説明です".repeat(4);
        let themed = ThemedRenderer::new(&theme).render(&state);
        assert!(themed.contains("…"), "long task must be truncated with an ellipsis");

        let plain = PlainRenderer.render(&state);
        assert!(plain.contains("…"));
    }

    #[test]
    fn status_glyph_precedes_status_label() {
        // Regression: the glyph used to be appended raw after the
        // painted status text (wrong order, uncolored glyph).
        let theme = Theme::default();
        let renderer = ThemedRenderer::new(&theme);
        let state = sample_state();
        let out = renderer.render(&state);
        let row = out.lines().find(|l| l.contains("agent-1")).unwrap();
        let glyph_pos = row.find('▶').expect("running glyph present");
        let label_pos = row.find("running").expect("status label present");
        assert!(glyph_pos < label_pos, "glyph must precede the label");
    }

    #[test]
    fn agent_row_new_has_empty_parent() {
        let row = AgentRow::new("a1", "master");
        assert!(row.is_master());
        assert!(row.parent.is_empty());
    }

    #[test]
    fn task_dashboard_groups_by_status() {
        let theme = Theme::default();
        let dash = TaskDashboard {
            tasks: vec![
                TaskRow {
                    id: "t1".into(),
                    description: "Implement the parser".into(),
                    status: TaskStatus::Running,
                    priority: 5,
                    dependencies: vec!["t0".into()],
                    assignee: Some("agent-1".into()),
                },
                TaskRow {
                    id: "t0".into(),
                    description: "Set up scaffolding".into(),
                    status: TaskStatus::Completed,
                    priority: 9,
                    dependencies: vec![],
                    assignee: None,
                },
                TaskRow {
                    id: "t2".into(),
                    description: "Wire up the tests".into(),
                    status: TaskStatus::Blocked,
                    priority: 1,
                    dependencies: vec!["t1".into()],
                    assignee: None,
                },
            ],
        };
        let out = render_task_dashboard(&dash, &theme);
        assert!(out.contains("REQUIEM tasks"));
        assert!(out.contains("RUNNING"));
        assert!(out.contains("BLOCKED"));
        assert!(out.contains("DONE"));
        // Dependencies are annotated.
        assert!(out.contains("⧗ t0"));
        // Priority is shown for high-priority tasks.
        assert!(out.contains("[p9]"));
        // Assignee is shown.
        assert!(out.contains("← agent-1"));
    }

    #[test]
    fn task_dashboard_empty_state() {
        let theme = Theme::default();
        let dash = TaskDashboard::default();
        let out = render_task_dashboard(&dash, &theme);
        assert!(out.contains("no tasks yet"));
        assert!(out.contains("no tasks in this session"));
    }

    #[test]
    fn task_counts_sum_to_total() {
        let dash = TaskDashboard {
            tasks: vec![
                TaskRow {
                    status: TaskStatus::Running,
                    ..Default::default()
                },
                TaskRow {
                    status: TaskStatus::Failed,
                    ..Default::default()
                },
                TaskRow {
                    status: TaskStatus::Failed,
                    ..Default::default()
                },
            ],
        };
        let c = dash.counts();
        assert_eq!(c[3], 1); // running
        assert_eq!(c[6], 2); // failed
        assert_eq!(c.iter().sum::<usize>(), 3);
    }
}
