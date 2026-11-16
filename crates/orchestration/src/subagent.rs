//! Nested subagent delegation.
//!
//! A `SubagentSpec` describes a child agent that the master (or any
//! parent) wants to spawn for a subtask. The subagent runs with a
//! bounded depth and receives a focused context slice.

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
    if parent.depth + 1 > MAX_DEPTH {
        return Err(RequiemError::InvalidState(format!(
            "cannot spawn subagent at depth {} (max {MAX_DEPTH})",
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
