//! Agent descriptors, hierarchy, and lifecycle state.
//!
//! An *agent* in REQUIEM is a running logical unit that owns one or more
//! tasks and can spawn child agents. All agents share the same configured
//! model; what differs is their `Role`, the context they receive, and the
//! permission profile they run under.

use serde::{Deserialize, Serialize};

use crate::{Id, Role, Timestamp};

/// A stable identity for an agent instance.
///
/// Distinct from `Id` (used for tasks, sessions) only to give agents a
/// dedicated namespace in logs and the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new() -> Self {
        Self(Id::new().0)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

/// The runtime status of an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// Allocated but not yet started.
    #[default]
    Idle,
    /// Actively executing its current task.
    Running,
    /// Waiting for a dependency or user input.
    Waiting,
    /// Finished its assignment (will be reclaimed or terminated).
    Done,
    /// Crashed or hit a fatal error.
    Failed,
    /// Explicitly cancelled.
    Cancelled,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Idle => "IDLE",
            AgentStatus::Running => "RUNNING",
            AgentStatus::Waiting => "WAITING",
            AgentStatus::Done => "DONE",
            AgentStatus::Failed => "FAILED",
            AgentStatus::Cancelled => "CANCELLED",
        }
    }
}

/// A full descriptor for a single agent instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    /// Stable identifier.
    pub id: AgentId,
    /// Human-readable name (e.g. "research-1").
    pub name: String,
    /// The role this agent was spawned for.
    pub role: Role,
    /// The agent that spawned this one, if any (the root has `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<AgentId>,
    /// The task session this agent belongs to.
    pub task_session: Id,
    /// Delegation depth: 0 = master, 1 = worker, 2 = subagent, ...
    pub depth: u32,
    /// Current status.
    #[serde(default)]
    pub status: AgentStatus,
    /// The task the agent is currently on, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task: Option<Id>,
    /// When the agent was created.
    #[serde(default = "Timestamp::now")]
    pub created: Timestamp,
    /// When the agent last reported activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<Timestamp>,
}

impl Agent {
    /// Create a new agent at depth 0 (the master).
    pub fn master(name: impl Into<String>, task_session: Id) -> Self {
        Self {
            id: AgentId::new(),
            name: name.into(),
            role: Role::Master,
            parent: None,
            task_session,
            depth: 0,
            status: AgentStatus::Idle,
            current_task: None,
            created: Timestamp::now(),
            last_activity: None,
        }
    }

    /// Create a child agent under a parent.
    pub fn child(name: impl Into<String>, role: Role, parent: &Agent, task_session: Id) -> Self {
        Self {
            id: AgentId::new(),
            name: name.into(),
            role,
            parent: Some(parent.id.clone()),
            task_session,
            depth: parent.depth + 1,
            status: AgentStatus::Idle,
            current_task: None,
            created: Timestamp::now(),
            last_activity: None,
        }
    }

    /// Assign this agent to a task.
    pub fn assign_task(&mut self, task: Id) {
        self.current_task = Some(task);
        self.last_activity = Some(Timestamp::now());
    }

    /// Mark this agent running.
    pub fn start(&mut self) {
        self.status = AgentStatus::Running;
        self.last_activity = Some(Timestamp::now());
    }

    /// Mark this agent done.
    pub fn finish(&mut self) {
        self.status = AgentStatus::Done;
        self.last_activity = Some(Timestamp::now());
    }

    /// Mark this agent failed.
    pub fn fail(&mut self) {
        self.status = AgentStatus::Failed;
        self.last_activity = Some(Timestamp::now());
    }

    /// Mark this agent cancelled.
    pub fn cancel(&mut self) {
        self.status = AgentStatus::Cancelled;
        self.last_activity = Some(Timestamp::now());
    }
}

/// A registry of all live agents in a task session, keyed by id, plus a
/// quick map from parent to children.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentRegistry {
    /// All agents, keyed by agent id.
    pub agents: std::collections::HashMap<AgentId, Agent>,
    /// The master agent's id, if one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<AgentId>,
}

impl AgentRegistry {
    /// Register a new agent. Returns its id.
    pub fn register(&mut self, agent: Agent) -> AgentId {
        let id = agent.id.clone();
        let is_master = agent.depth == 0;
        self.agents.insert(id.clone(), agent);
        if is_master {
            self.master = Some(id.clone());
        }
        id
    }

    /// The master agent, if present.
    pub fn master(&self) -> Option<&Agent> {
        self.master.as_ref().and_then(|id| self.agents.get(id))
    }

    /// Direct children of an agent.
    pub fn children(&self, parent: &AgentId) -> Vec<&Agent> {
        self.agents
            .values()
            .filter(|a| a.parent.as_ref() == Some(parent))
            .collect()
    }

    /// All agents currently running.
    pub fn running(&self) -> Vec<&Agent> {
        self.agents
            .values()
            .filter(|a| a.status == AgentStatus::Running)
            .collect()
    }

    /// All agents in a task session that is no longer making progress
    /// (terminal status) — useful for the scheduler to reclaim them.
    pub fn terminal(&self) -> Vec<&Agent> {
        self.agents
            .values()
            .filter(|a| {
                matches!(
                    a.status,
                    AgentStatus::Done | AgentStatus::Failed | AgentStatus::Cancelled
                )
            })
            .collect()
    }

    /// Number of live (non-terminal) agents.
    pub fn live_count(&self) -> usize {
        self.agents
            .values()
            .filter(|a| {
                !matches!(
                    a.status,
                    AgentStatus::Done | AgentStatus::Failed | AgentStatus::Cancelled
                )
            })
            .count()
    }

    /// Remove an agent that has reached a terminal state.
    pub fn reap(&mut self, id: &AgentId) -> Option<Agent> {
        let agent = self.agents.get(id)?;
        if !matches!(
            agent.status,
            AgentStatus::Done | AgentStatus::Failed | AgentStatus::Cancelled
        ) {
            return None;
        }
        self.agents.remove(id)
    }
}
