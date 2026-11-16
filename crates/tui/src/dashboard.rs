//! The live agent dashboard: shows running agents, their current tasks,
//! token usage, and overall session progress.

use serde::{Deserialize, Serialize};

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
        }
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
            let task_display: String = {
                let t = &agent.current_task;
                if t.len() > 22 {
                    format!("{}…", &t[..22])
                } else {
                    t.clone()
                }
            };
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
}
