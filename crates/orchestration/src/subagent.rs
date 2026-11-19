//! Nested subagent delegation.
//!
//! A `SubagentSpec` describes a child agent that the master (or any
//! parent) wants to spawn for a subtask. The subagent runs with a
//! bounded depth and receives a focused context slice.

use std::fmt::Write;

use requiem_core::agent::{Agent, AgentId};
use requiem_core::error::RequiemError;
use requiem_core::Id;

/// The maximum delegation depth before nested spawning is refused.
pub const MAX_DEPTH: u32 = 4;

/// A spec for spawning a subagent.
#[derive(Debug, Clone)]
pub struct SubagentSpec {
    /// The name to give the subagent.
    pub name: String,
    /// The role the subagent will run under.
    pub role: requiem_core::Role,
    /// The task the subagent is responsible for.
    pub task_id: Id,
    /// Optional focused context (slice of the parent's working set).
    pub context: Option<String>,
    /// Optional structured handoff — a compact summary of prior work
    /// that replaces the full master conversation for the child agent.
    pub handoff: Option<HandoffContext>,
}

/// A running subagent handle returned by the spawner.
pub struct SubagentHandle {
    pub agent: Agent,
}

/// Spawn a subagent under a parent, enforcing the depth limit.
pub fn spawn_subagent(
    parent: &Agent,
    spec: SubagentSpec,
    task_session: Id,
) -> Result<SubagentHandle, RequiemError> {
    spawn_subagent_with_limit(parent, spec, task_session, MAX_DEPTH)
}

/// Spawn a subagent under a parent, with a configurable depth limit.
///
/// Use this variant when the limit comes from user configuration
/// (`Config.max_agent_depth`) rather than the compile-time default.
pub fn spawn_subagent_with_limit(
    parent: &Agent,
    spec: SubagentSpec,
    task_session: Id,
    max_depth: u32,
) -> Result<SubagentHandle, RequiemError> {
    if parent.depth + 1 > max_depth {
        return Err(RequiemError::InvalidState(format!(
            "cannot spawn subagent at depth {} (max {max_depth})",
            parent.depth + 1
        )));
    }
    let agent = Agent::child(spec.name, spec.role, parent, task_session);
    Ok(SubagentHandle { agent })
}

/// The set of subagents currently running under a parent.
pub struct SubagentPool {
    agents: Vec<SubagentHandle>,
}

impl SubagentPool {
    pub fn new() -> Self {
        Self { agents: Vec::new() }
    }

    /// Add a subagent to the pool.
    pub fn add(&mut self, handle: SubagentHandle) {
        self.agents.push(handle);
    }

    /// The number of live subagents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// All agent ids in the pool.
    pub fn agent_ids(&self) -> Vec<AgentId> {
        self.agents.iter().map(|h| h.agent.id.clone()).collect()
    }
}

impl Default for SubagentPool {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Compact structured handoffs
// ---------------------------------------------------------------------------

/// A compact, structured summary of the work completed so far that is
/// passed to a nested subagent instead of the full master conversation.
///
/// This keeps the subagent's context window focused on the subtask
/// while preserving enough signal to avoid redundant work.
#[derive(Debug, Clone, Default)]
pub struct HandoffContext {
    /// What the subagent is being asked to do.
    pub objective: String,
    /// Files the subagent should focus on (paths, not contents).
    pub relevant_files: Vec<String>,
    /// Key findings from prior work (one line each).
    pub findings: Vec<String>,
    /// Hard constraints the subagent must respect.
    pub constraints: Vec<String>,
    /// What "done" looks like — the completion criteria.
    pub completion_criteria: Vec<String>,
    /// The parent agent's name (for traceability in logs).
    pub parent_name: Option<String>,
    /// Delegation depth (0 = master, 1 = worker, …).
    pub depth: u32,
}

impl HandoffContext {
    /// Build a handoff from a parent's state.
    pub fn new(objective: impl Into<String>) -> Self {
        Self {
            objective: objective.into(),
            ..Default::default()
        }
    }

    /// Add a relevant file path.
    pub fn with_file(mut self, path: impl Into<String>) -> Self {
        self.relevant_files.push(path.into());
        self
    }

    /// Add a finding.
    pub fn with_finding(mut self, finding: impl Into<String>) -> Self {
        self.findings.push(finding.into());
        self
    }

    /// Add a constraint.
    pub fn with_constraint(mut self, c: impl Into<String>) -> Self {
        self.constraints.push(c.into());
        self
    }

    /// Add a completion criterion.
    pub fn with_done_when(mut self, c: impl Into<String>) -> Self {
        self.completion_criteria.push(c.into());
        self
    }

    /// Set the parent agent name.
    pub fn from_parent(mut self, parent: &Agent) -> Self {
        self.parent_name = Some(parent.name.clone());
        self.depth = parent.depth + 1;
        self
    }

    /// Render as a compact text block for injection into a subagent's
    /// system prompt. Output is bounded: max 5 files, 5 findings,
    /// 5 constraints, 3 criteria, each line truncated to 120 chars.
    pub fn to_prompt(&self) -> String {
        let mut out = String::with_capacity(512);
        let _ = write!(out, "\n\nSUBTASK OBJECTIVE: {}\n", self.objective);

        if !self.relevant_files.is_empty() {
            out.push_str("RELEVANT FILES:\n");
            for f in self.relevant_files.iter().take(5) {
                let _ = writeln!(out, "  - {}", truncate_str(f, 80));
            }
        }
        if !self.findings.is_empty() {
            out.push_str("PRIOR FINDINGS:\n");
            for f in self.findings.iter().take(5) {
                let _ = writeln!(out, "  - {}", truncate_str(f, 120));
            }
        }
        if !self.constraints.is_empty() {
            out.push_str("CONSTRAINTS:\n");
            for c in self.constraints.iter().take(5) {
                let _ = writeln!(out, "  - {}", truncate_str(c, 120));
            }
        }
        if !self.completion_criteria.is_empty() {
            out.push_str("DONE WHEN:\n");
            for c in self.completion_criteria.iter().take(3) {
                let _ = writeln!(out, "  - [ ] {}", truncate_str(c, 120));
            }
        }
        if let Some(p) = &self.parent_name {
            let _ = writeln!(out, "DELEGATED BY: {p} (depth {})", self.depth);
        }
        out
    }
}

/// Truncate a string to `max` chars, appending an ellipsis if cut.
fn truncate_str(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_prompt_contains_all_sections() {
        let h = HandoffContext::new("Fix the auth bug")
            .with_file("src/auth.rs")
            .with_finding("Token expiry check uses wrong timezone")
            .with_constraint("Must not break existing API")
            .with_done_when("All auth tests pass");

        let prompt = h.to_prompt();
        assert!(prompt.contains("SUBTASK OBJECTIVE: Fix the auth bug"));
        assert!(prompt.contains("src/auth.rs"));
        assert!(prompt.contains("Token expiry check"));
        assert!(prompt.contains("Must not break existing API"));
        assert!(prompt.contains("All auth tests pass"));
    }

    #[test]
    fn handoff_prompt_omits_empty_sections() {
        let h = HandoffContext::new("Just do X");
        let prompt = h.to_prompt();
        assert!(prompt.contains("SUBTASK OBJECTIVE: Just do X"));
        assert!(
            prompt.starts_with("\n\n"),
            "must start with newlines for clean append"
        );
        assert!(!prompt.contains("RELEVANT FILES:"));
        assert!(!prompt.contains("PRIOR FINDINGS:"));
        assert!(!prompt.contains("CONSTRAINTS:"));
        assert!(!prompt.contains("DONE WHEN:"));
    }

    #[test]
    fn handoff_prompt_respects_caps() {
        let h = HandoffContext::new("Task")
            .with_file("a.rs")
            .with_file("b.rs")
            .with_file("c.rs")
            .with_file("d.rs")
            .with_file("e.rs")
            .with_file("f.rs") // 6th should be dropped
            .with_finding("f1")
            .with_finding("f2")
            .with_finding("f3")
            .with_finding("f4")
            .with_finding("f5")
            .with_finding("f6"); // 6th dropped
        let prompt = h.to_prompt();
        assert!(!prompt.contains("f.rs"));
        assert!(!prompt.contains("f6"));
        assert!(prompt.contains("e.rs"));
        assert!(prompt.contains("f5"));
    }

    #[test]
    fn handoff_from_parent_sets_metadata() {
        let parent = Agent::master("root", Id::new());
        let h = HandoffContext::new("sub").from_parent(&parent).to_prompt();
        assert!(h.contains("DELEGATED BY: root"));
        assert!(h.contains("depth 1"));
    }
}
