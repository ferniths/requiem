//! The global multi-task scheduler.
//!
//! The `Scheduler` is the top-level loop that drives the entire REQUIEM
//! session. It owns the `TaskGraph` and dispatches available tasks to
//! agents concurrently, respecting the agent pool size and task
//! dependencies.

use std::sync::{Arc, Mutex};

use requiem_core::agent::{AgentId, AgentRegistry};
use requiem_core::task::TaskGraph;
use requiem_core::Id;

/// Configuration for the scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Maximum number of concurrent agents (0 = unlimited, but bounded by
    /// the model client's `max_concurrency` in practice).
    pub max_agents: usize,
    /// Maximum total verification attempts across all tasks before the
    /// session is marked blocked.
    pub max_verification_attempts: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_agents: 4,
            max_verification_attempts: 3,
        }
    }
}

/// The scheduler owns the task graph and agent registry.
///
/// All mutations go through `&mut self` so the graph invariant is
/// preserved; the scheduler is wrapped in a `Mutex` when shared across
/// async tasks.
#[derive(Debug, Clone)]
pub struct Scheduler {
    pub graph: TaskGraph,
    pub registry: AgentRegistry,
    config: SchedulerConfig,
}

impl Scheduler {
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            graph: TaskGraph::default(),
            registry: AgentRegistry::default(),
            config,
        }
    }

    /// Add a root task to the graph.
    pub fn add_task(&mut self, task: requiem_core::task::Task) -> Id {
        let id = self.graph.add(task);
        // Re-evaluate so the new task may immediately become Available.
        let _ = self.graph.reevaluate();
        id
    }

    /// Poll: return the ids of currently-available tasks that have not
    /// been claimed, up to `max_agents`.
    ///
    /// The caller is responsible for actually launching the agents.
    /// This method claims the tasks in the graph so that concurrent
    /// scheduler polls do not double-dispatch.
    pub fn poll_available(&mut self) -> Vec<Id> {
        let available: Vec<Id> = self
            .graph
            .available()
            .iter()
            .filter(|t| {
                self.registry
                    .agents
                    .get(&AgentId(t.id.as_str().to_string()))
                    .map(|a| a.current_task.as_ref() == Some(&t.id))
                    .unwrap_or(false)
            })
            .map(|t| t.id.clone())
            .take(self.config.max_agents)
            .collect();
        available
    }

    /// Claim a task for a newly-spawned agent.
    pub fn claim(&mut self, task: &Id, agent: &AgentId) -> Option<Id> {
        self.graph.claim(task.clone(), agent.clone())
    }

    /// Mark a task as started (agent is now running).
    pub fn start_task(&mut self, task: &Id) {
        self.graph.start(task.clone());
    }

    /// Complete a task and return newly-unlocked task ids.
    pub fn complete_task(&mut self, task: &Id, result: serde_json::Value) -> Vec<Id> {
        self.graph.complete(task.clone(), result)
    }

    /// Fail a task.
    pub fn fail_task(&mut self, task: &Id, reason: impl Into<String>) {
        self.graph.fail(task.clone(), reason);
    }

    /// Whether the session has any work remaining.
    pub fn has_pending_work(&self) -> bool {
        self.graph.tasks.values().any(|t| !t.status.is_terminal())
    }

    /// Fraction of tasks in a terminal state.
    pub fn progress(&self) -> f32 {
        self.graph.progress()
    }

    /// Total terminal count.
    pub fn terminal_count(&self) -> usize {
        self.graph
            .tasks
            .values()
            .filter(|t| t.status.is_terminal())
            .count()
    }
}

/// A shared, thread-safe scheduler handle for use across async tasks.
pub type SharedScheduler = Arc<Mutex<Scheduler>>;
