//! The global multi-task scheduler.
//!
//! The `Scheduler` is the top-level loop that drives the entire REQUIEM
//! session. It owns the `TaskGraph` and dispatches available tasks to
//! agents concurrently, respecting the agent pool size and task
//! dependencies.

use std::sync::{Arc, Mutex};

use requiem_core::agent::{AgentId, AgentRegistry, AgentStatus};
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

    /// Build a scheduler that restores a previously-persisted task graph.
    ///
    /// Used on session resume: the graph state (task statuses, results,
    /// failure reasons) is rehydrated from the checkpoint so in-flight
    /// multi-agent work continues rather than starting fresh. The agent
    /// registry is rebuilt empty; tasks that were `Running`/`Claimed`
    /// at checkpoint time are treated as re-dispatchable.
    pub fn restore(config: SchedulerConfig, graph: TaskGraph) -> Self {
        Self {
            graph,
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

    /// Poll: return the ids of currently-available tasks that no live
    /// agent is working on, bounded by the number of free agent slots
    /// (`max_agents` minus busy agents).
    ///
    /// The caller is responsible for actually launching the agents and
    /// claiming the returned tasks via [`Scheduler::claim`]. A task that
    /// a live agent already holds (`Agent::current_task`) is skipped, so
    /// concurrent polls never double-dispatch the same task.
    pub fn poll_available(&mut self) -> Vec<Id> {
        // Tasks already held by a live agent are not re-dispatchable.
        let held: std::collections::HashSet<String> = self
            .registry
            .agents
            .values()
            .filter(|a| {
                !matches!(
                    a.status,
                    AgentStatus::Done | AgentStatus::Failed | AgentStatus::Cancelled
                )
            })
            .filter_map(|a| a.current_task.as_ref().map(|t| t.as_str().to_string()))
            .collect();

        // Agents occupying a slot: running, or live and holding a task.
        let busy = self
            .registry
            .agents
            .values()
            .filter(|a| {
                a.status == AgentStatus::Running
                    || (a.current_task.is_some()
                        && !matches!(
                            a.status,
                            AgentStatus::Done | AgentStatus::Failed | AgentStatus::Cancelled
                        ))
            })
            .count();
        let free_slots = self.config.max_agents.saturating_sub(busy);

        self.graph
            .available()
            .iter()
            .filter(|t| !held.contains(t.id.as_str()))
            .map(|t| t.id.clone())
            .take(free_slots)
            .collect()
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

    /// How many tasks are currently in the `Running` state.
    ///
    /// Used by the session driver to gate concurrency: a new task can only
    /// start when `running_task_count() < max_agents`.
    pub fn running_task_count(&self) -> usize {
        self.graph
            .tasks
            .values()
            .filter(|t| t.status == requiem_core::task::TaskStatus::Running)
            .count()
    }

    /// The configured maximum number of concurrent agents.
    pub fn max_agents(&self) -> usize {
        self.config.max_agents
    }

    /// Reclaim tasks whose lease has expired (stalled workers).
    ///
    /// Returns the ids of tasks that were moved back to `Available` so
    /// they can be re-dispatched. Tasks with `lease_ms == 0` (infinite
    /// lease) are never reclaimed.
    pub fn reclaim_stalled(&mut self) -> Vec<Id> {
        let stalled = self.graph.stalled_tasks();
        for id in &stalled {
            self.graph.release_stalled(id);
        }
        let _ = self.graph.reevaluate();
        stalled
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

#[cfg(test)]
mod tests {
    use super::*;
    use requiem_core::agent::Agent;
    use requiem_core::task::{Task, TaskStatus};

    fn scheduler_with_tasks(n: usize) -> Scheduler {
        let mut s = Scheduler::new(SchedulerConfig::default());
        for i in 0..n {
            s.add_task(Task::new(format!("task-{i}")));
        }
        s
    }

    #[test]
    fn poll_returns_available_tasks_bounded_by_max_agents() {
        let mut s = scheduler_with_tasks(6);
        let polled = s.poll_available();
        assert_eq!(polled.len(), 4, "max_agents default is 4");
    }

    #[test]
    fn poll_skips_tasks_held_by_live_agents() {
        let mut s = scheduler_with_tasks(4);
        // A live agent is already working on the first task.
        let mut agent = Agent::master("master", Id::new());
        agent.assign_task(polled_ids(&s)[0].clone());
        agent.start();
        s.registry.register(agent);

        let polled = s.poll_available();
        assert_eq!(polled.len(), 3, "1 slot busy, 3 tasks not held");
        assert!(
            !polled.contains(&polled_ids(&s)[0]),
            "held task must not be re-dispatched"
        );
    }

    #[test]
    fn poll_skips_held_tasks_even_when_agent_not_started() {
        let mut s = scheduler_with_tasks(4);
        // Assigned but not yet started: still occupies a slot.
        let mut agent = Agent::master("master", Id::new());
        agent.assign_task(polled_ids(&s)[1].clone());
        let aid = s.registry.register(agent);

        let polled = s.poll_available();
        assert_eq!(polled.len(), 3);
        assert!(!polled.contains(&Id::new())); // sanity: no phantom ids
        assert!(
            !held_check(&s, &aid, &polled),
            "held task not re-dispatched"
        );
    }

    #[test]
    fn poll_zero_when_all_slots_busy() {
        let mut s = scheduler_with_tasks(4);
        for (i, id) in polled_ids(&s).into_iter().enumerate() {
            let mut agent = Agent::master(format!("agent-{i}"), Id::new());
            agent.assign_task(id);
            agent.start();
            s.registry.register(agent);
        }
        assert!(s.poll_available().is_empty());
    }

    #[test]
    fn claim_moves_task_out_of_available() {
        let mut s = scheduler_with_tasks(2);
        let id = s.poll_available()[0].clone();
        let agent_id = AgentId("worker-1".into());
        let claimed = s.claim(&id, &agent_id);
        assert_eq!(claimed.as_ref(), Some(&id));
        assert!(s.poll_available().iter().all(|i| i != &id));
        assert_eq!(s.graph.tasks.get(&id).unwrap().status, TaskStatus::Claimed);
    }

    #[test]
    fn terminal_agents_free_their_slots() {
        let mut s = scheduler_with_tasks(4);
        let id = polled_ids(&s)[0].clone();
        let mut agent = Agent::master("master", Id::new());
        agent.assign_task(id.clone());
        agent.start();
        let aid = s.registry.register(agent);
        assert!(s.poll_available().len() == 3);

        s.registry.agents.get_mut(&aid).unwrap().finish();
        // The agent finished: its slot frees up again.
        assert_eq!(s.poll_available().len(), 4);
    }

    #[test]
    fn running_task_count_tracks_status() {
        let mut s = scheduler_with_tasks(3);
        assert_eq!(s.running_task_count(), 0);
        let id = polled_ids(&s)[0].clone();
        s.start_task(&id);
        assert_eq!(s.running_task_count(), 1);
        s.complete_task(&id, serde_json::json!(null));
        assert_eq!(s.running_task_count(), 0);
    }

    #[test]
    fn restore_preserves_graph_state() {
        let mut original = scheduler_with_tasks(3);
        let a = polled_ids(&original)[0].clone();
        let _ = original.complete_task(&a, serde_json::json!({"done": true}));
        // Mark one task failed.
        let b = polled_ids(&original)[0].clone();
        original.fail_task(&b, "boom");

        // Restore into a fresh scheduler; graph state must survive.
        let restored = Scheduler::restore(SchedulerConfig::default(), original.graph.clone());
        assert_eq!(restored.graph.len(), 3);
        assert_eq!(
            restored.graph.get(&a).status,
            TaskStatus::Completed,
            "completed task status must survive restore"
        );
        assert_eq!(
            restored.graph.get(&b).status,
            TaskStatus::Failed,
            "failed task status must survive restore"
        );
        // The registry is rebuilt empty on restore.
        assert!(restored.registry.agents.is_empty());
    }

    /// Regression: a task claimed with a short lease must be reclaimed
    /// when the lease expires, and the scheduler must offer it for
    /// re-dispatch.
    #[test]
    fn reclaim_stalled_releases_lease_expired_tasks() {
        let mut s = scheduler_with_tasks(1);
        let id = s.poll_available()[0].clone();
        let agent_id = AgentId("stalled-worker".into());
        s.claim(&id, &agent_id).unwrap();
        s.start_task(&id);
        // Set a 1 ms lease and backdate the claim so it's already expired.
        {
            let t = s.graph.tasks.get_mut(&id).unwrap();
            t.lease_ms = 1;
            t.claimed_at = Some(requiem_core::Timestamp::now());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        let reclaimed = s.reclaim_stalled();
        assert_eq!(reclaimed.len(), 1, "stalled task must be reclaimed");
        assert_eq!(reclaimed[0], id);
        // The task is back in Available state and can be re-polled.
        let polled = s.poll_available();
        assert!(
            polled.contains(&id),
            "reclaimed task must be re-dispatchable"
        );
        assert_eq!(
            s.graph.tasks.get(&id).unwrap().status,
            TaskStatus::Available
        );
    }

    /// A task with an infinite lease (lease_ms == 0) must never be
    /// reclaimed, even if it's been running for a long time.
    #[test]
    fn infinite_lease_tasks_are_never_reclaimed() {
        let mut s = scheduler_with_tasks(1);
        let id = s.poll_available()[0].clone();
        let agent_id = AgentId("long-runner".into());
        s.claim(&id, &agent_id).unwrap();
        s.start_task(&id);
        // No lease set: lease_ms defaults to 0 (infinite).
        let reclaimed = s.reclaim_stalled();
        assert!(
            reclaimed.is_empty(),
            "infinite-lease tasks must not be reclaimed"
        );
    }

    // -- helpers ------------------------------------------------------------

    fn polled_ids(s: &Scheduler) -> Vec<Id> {
        s.graph.available().iter().map(|t| t.id.clone()).collect()
    }

    fn held_check(s: &Scheduler, agent: &AgentId, polled: &[Id]) -> bool {
        let held = s.registry.agents[agent]
            .current_task
            .as_ref()
            .map(|t| polled.iter().any(|p| p == t))
            .unwrap_or(false);
        held
    }
}
