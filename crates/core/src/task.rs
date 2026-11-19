//! The shared task graph: task units, states, and the graph itself.
//!
//! The task graph is the coordination backbone of REQUIEM. Agents claim
//! tasks, publish results, and the scheduler detects when dependencies are
//! satisfied. The master always holds a complete view of the graph.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::agent::AgentId;
use crate::{Id, Timestamp};

/// The lifecycle state of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Created but not yet admissible (dependencies not satisfied).
    #[default]
    Pending,
    /// All dependencies completed; ready to be claimed.
    Available,
    /// Claimed by an agent; about to run.
    Claimed,
    /// An agent is actively executing it.
    Running,
    /// Waiting on an external condition (e.g. another task, user input).
    Blocked,
    /// Finished successfully.
    Completed,
    /// Finished with a failure that has been recorded.
    Failed,
    /// Explicitly cancelled.
    Cancelled,
}

impl TaskStatus {
    /// Whether this state is terminal.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
    }

    /// A short human label for TUI rendering.
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Pending => "PENDING",
            TaskStatus::Available => "AVAILABLE",
            TaskStatus::Claimed => "CLAIMED",
            TaskStatus::Running => "RUNNING",
            TaskStatus::Blocked => "BLOCKED",
            TaskStatus::Completed => "DONE",
            TaskStatus::Failed => "FAILED",
            TaskStatus::Cancelled => "CANCELLED",
        }
    }
}

/// A unit of work within the task graph.
///
/// A task has a single owner at a time. `dependencies` are `TaskId`s that
/// must reach `Completed` before this task may move to `Available`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier.
    pub id: Id,
    /// A one-line description shown in the TUI and logs.
    pub description: String,
    /// An optional longer body (instructions, files, context references).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The parent task that created this one, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Id>,
    /// The agent that currently owns this task, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<AgentId>,
    /// Task ids that must complete first.
    #[serde(default)]
    pub dependencies: Vec<Id>,
    /// File paths this task is expected to touch (for ownership tracking).
    #[serde(default)]
    pub files: Vec<String>,
    /// Priority: higher runs first when multiple tasks are available.
    #[serde(default)]
    pub priority: u8,
    /// Current status.
    #[serde(default)]
    pub status: TaskStatus,
    /// The result payload, set when the task completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Failure details, set when the task fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// How many consecutive verification passes have failed (bounded loop).
    #[serde(default)]
    pub verification_attempts: u32,
    /// When the task was claimed (for lease tracking). `None` when the
    /// task is not in a claimed/running state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<Timestamp>,
    /// The task's lease in milliseconds. A task in `Claimed`/`Running`
    /// state that exceeds this window without a status update is
    /// considered stalled and eligible for reassignment.
    #[serde(default)]
    pub lease_ms: u64,
    /// When the task was created.
    #[serde(default = "Timestamp::now")]
    pub created: Timestamp,
    /// When the task last changed status, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<Timestamp>,
}

impl Task {
    /// A new pending task with no dependencies.
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            id: Id::new(),
            description: description.into(),
            body: None,
            parent: None,
            owner: None,
            dependencies: Vec::new(),
            files: Vec::new(),
            priority: 0,
            status: TaskStatus::Pending,
            result: None,
            failure: None,
            verification_attempts: 0,
            claimed_at: None,
            lease_ms: 0,
            created: Timestamp::now(),
            updated: None,
        }
    }

    /// Builder: set the parent.
    pub fn with_parent(mut self, parent: Id) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Builder: add a dependency.
    pub fn depends_on(mut self, dep: Id) -> Self {
        self.dependencies.push(dep);
        self
    }

    /// Builder: set the body.
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Builder: mark the files this task will touch.
    pub fn touching(mut self, files: impl Into<Vec<String>>) -> Self {
        self.files = files.into();
        self
    }

    /// Builder: set priority.
    pub fn with_priority(mut self, p: u8) -> Self {
        self.priority = p;
        self
    }

    /// Whether all of this task's dependencies are `Completed`.
    ///
    /// Dependencies that are `Failed` or `Cancelled` do *not* satisfy the
    /// dependency; the scheduler must surface that as a blocker.
    pub fn dependencies_satisfied(&self, graph: &TaskGraph) -> bool {
        self.dependencies
            .iter()
            .all(|d| matches!(graph.get(d).status, TaskStatus::Completed))
    }

    /// Whether any dependency has terminally failed (not completed).
    pub fn has_failed_dependency(&self, graph: &TaskGraph) -> bool {
        self.dependencies.iter().any(|d| {
            matches!(
                graph.get(d).status,
                TaskStatus::Failed | TaskStatus::Cancelled
            )
        })
    }
}

/// The directed task graph with fast lookups.
///
/// This is the single source of truth for task state. All mutations go
/// through its methods so that dependency satisfaction can be re-evaluated
/// atomically and the invariant "no available task has a pending
/// dependency" holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskGraph {
    /// All tasks, keyed by id.
    #[serde(default)]
    pub tasks: HashMap<Id, Task>,
}

impl TaskGraph {
    /// Insert a task. If its dependencies are all already `Completed` it
    /// starts as `Available`, otherwise `Pending`. Returns the task's id.
    pub fn add(&mut self, task: Task) -> Id {
        let mut t = task;
        if t.status == TaskStatus::Pending {
            let all_deps = t.dependencies.iter().all(|d| {
                self.tasks
                    .get(d)
                    .map(|dep| matches!(dep.status, TaskStatus::Completed))
                    .unwrap_or(false)
            });
            t.status = if all_deps {
                TaskStatus::Available
            } else {
                TaskStatus::Pending
            };
        }
        let id = t.id.clone();
        self.tasks.insert(id.clone(), t);
        id
    }

    /// Re-evaluate pending tasks whose dependencies may have just
    /// completed. Returns the ids that transitioned to `Available`.
    ///
    /// Call this after every `complete` so the graph stays consistent.
    pub fn reevaluate(&mut self) -> Vec<Id> {
        // Collect (id, dependencies) pairs first so we can take a mutable
        // borrow of the map afterwards without a self-referential loop.
        let pending: Vec<(Id, Vec<Id>)> = self
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Pending)
            .map(|t| (t.id.clone(), t.dependencies.clone()))
            .collect();
        let mut unlocked = Vec::new();
        for (id, deps) in pending {
            let deps_satisfied = deps.iter().all(|d| {
                matches!(
                    self.tasks.get(d).map(|x| x.status),
                    Some(TaskStatus::Completed)
                )
            });
            if deps_satisfied {
                if let Some(t) = self.tasks.get_mut(&id) {
                    t.status = TaskStatus::Available;
                    t.updated = Some(Timestamp::now());
                }
                unlocked.push(id);
            }
        }
        unlocked
    }

    /// All currently-available tasks, highest priority first.
    pub fn available(&self) -> Vec<&Task> {
        let mut v: Vec<&Task> = self
            .tasks
            .values()
            .filter(|t| t.status == TaskStatus::Available)
            .collect();
        v.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.id.as_str().cmp(b.id.as_str()))
        });
        v
    }

    /// Tasks currently running.
    pub fn running(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| t.status == TaskStatus::Running)
            .collect()
    }

    /// Claim an available task for an agent. Returns the task id, or
    /// `None` if it was not available. Sets `claimed_at` so the scheduler
    /// can track lease expiration for stalled-agent detection.
    pub fn claim(&mut self, task: Id, agent: AgentId) -> Option<Id> {
        let t = self.tasks.get_mut(&task)?;
        if t.status != TaskStatus::Available {
            return None;
        }
        t.status = TaskStatus::Claimed;
        t.owner = Some(agent);
        t.claimed_at = Some(Timestamp::now());
        t.updated = Some(Timestamp::now());
        Some(task)
    }

    /// Mark a claimed/running task as running.
    pub fn start(&mut self, task: Id) {
        if let Some(t) = self.tasks.get_mut(&task) {
            t.status = TaskStatus::Running;
            t.updated = Some(Timestamp::now());
        }
    }

    /// Complete a task with a result. Re-evaluates dependents.
    ///
    /// Returns the ids of newly-unlocked tasks.
    pub fn complete(&mut self, task: Id, result: serde_json::Value) -> Vec<Id> {
        if let Some(t) = self.tasks.get_mut(&task) {
            t.status = TaskStatus::Completed;
            t.result = Some(result);
            t.updated = Some(Timestamp::now());
        }
        self.reevaluate()
    }

    /// Fail a task. Re-evaluates; dependents that only had this dep will
    /// remain `Pending` (their dep did not complete).
    pub fn fail(&mut self, task: Id, reason: impl Into<String>) {
        if let Some(t) = self.tasks.get_mut(&task) {
            t.status = TaskStatus::Failed;
            t.failure = Some(reason.into());
            t.updated = Some(Timestamp::now());
        }
    }

    /// Cancel a task and all of its unfinished descendants.
    pub fn cancel(&mut self, task: Id) {
        self.cancel_recursive(&task, &mut Vec::new());
        // Cancelling a task does not unlock its dependents (they now have a
        // non-completed dependency), so no reevaluate needed.
    }

    fn cancel_recursive(&mut self, task: &Id, visited: &mut Vec<Id>) {
        if !visited.contains(task) {
            visited.push(task.clone());
            if let Some(t) = self.tasks.get_mut(task) {
                if !t.status.is_terminal() {
                    t.status = TaskStatus::Cancelled;
                    t.updated = Some(Timestamp::now());
                }
            }
            // Collect children before reborrowing self mutably.
            let children: Vec<Id> = self
                .tasks
                .values()
                .filter(|c| c.parent.as_ref() == Some(task))
                .map(|c| c.id.clone())
                .collect();
            for child in children {
                self.cancel_recursive(&child, visited);
            }
        }
    }

    /// A direct task lookup. Panics if the id is not in the graph — callers
    /// must hold a valid id obtained from this graph.
    pub fn get(&self, id: &Id) -> &Task {
        &self.tasks[id]
    }

    /// Get-or-default accessor that never panics; returns the task or a
    /// sentinel indicating absence.
    pub fn try_get(&self, id: &Id) -> Option<&Task> {
        self.tasks.get(id)
    }

    /// Total task count.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Fraction of tasks that are in a terminal state (0.0..=1.0).
    pub fn progress(&self) -> f32 {
        if self.tasks.is_empty() {
            return 0.0;
        }
        let done = self
            .tasks
            .values()
            .filter(|t| t.status.is_terminal())
            .count();
        done as f32 / self.tasks.len() as f32
    }

    /// Tasks that are in `Claimed` or `Running` state and whose lease
    /// has expired. A task with `lease_ms == 0` is never considered
    /// stalled (infinite lease). Returns the ids of stalled tasks so the
    /// scheduler can reassign them.
    pub fn stalled_tasks(&self) -> Vec<Id> {
        let now_ms = Timestamp::now().as_millis();
        self.tasks
            .values()
            .filter(|t| {
                matches!(t.status, TaskStatus::Claimed | TaskStatus::Running)
                    && t.lease_ms > 0
                    && t.claimed_at.as_ref().is_some_and(|claimed| {
                        let claimed_ms = claimed.as_millis();
                        now_ms.saturating_sub(claimed_ms) >= t.lease_ms
                    })
            })
            .map(|t| t.id.clone())
            .collect()
    }

    /// Release a stalled task back to `Available` so it can be
    /// re-dispatched. Preserves the task's description and dependencies.
    pub fn release_stalled(&mut self, task: &Id) {
        if let Some(t) = self.tasks.get_mut(task) {
            t.status = TaskStatus::Available;
            t.owner = None;
            t.claimed_at = None;
            t.updated = Some(Timestamp::now());
        }
    }

    /// Topological order of task ids (for deterministic replay and session
    /// persistence). Panics on a cycle.
    pub fn topological_order(&self) -> Vec<Id> {
        let mut indegree: HashMap<Id, usize> = self
            .tasks
            .values()
            .map(|t| (t.id.clone(), t.dependencies.len()))
            .collect();
        let mut queue: VecDeque<Id> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(id, _)| id.clone())
            .collect();
        let mut order = Vec::new();
        let mut seen: usize = 0;
        while let Some(id) = queue.pop_front() {
            order.push(id.clone());
            seen += 1;
            // Find tasks that depend on `id` and decrement their indegree.
            for t in self.tasks.values() {
                if t.dependencies.contains(&id) {
                    if let Some(d) = indegree.get_mut(&t.id) {
                        *d = d.saturating_sub(1);
                        if *d == 0 {
                            queue.push_back(t.id.clone());
                        }
                    }
                }
            }
        }
        if seen != self.tasks.len() {
            // Cycle. Return the partial order; the caller is responsible for
            // detecting it.
        }
        order
    }
}

impl std::fmt::Display for TaskGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "TaskGraph({} tasks)", self.tasks.len())?;
        for t in self.tasks.values() {
            writeln!(
                f,
                "  [{}] {} {}",
                t.status.label(),
                t.id.as_str(),
                t.description
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_reevaluate_unlocks_dependents() {
        let mut g = TaskGraph::default();
        let a = g.add(Task::new("A"));
        // A has no deps -> starts Available.
        assert_eq!(g.get(&a).status, TaskStatus::Available);
        let b = g.add(Task::new("B").depends_on(a.clone()));
        // B depends on A which is not completed -> stays Pending.
        assert_eq!(g.get(&b).status, TaskStatus::Pending);

        let unlocked = g.complete(a.clone(), serde_json::json!({"ok": true}));
        assert!(unlocked.contains(&b));
        assert_eq!(g.get(&b).status, TaskStatus::Available);
    }

    #[test]
    fn cancel_recursive_cancels_children() {
        let mut g = TaskGraph::default();
        let parent = g.add(Task::new("parent"));
        let child = g.add(Task::new("child").with_parent(parent.clone()));
        g.cancel(parent.clone());
        assert_eq!(g.get(&child).status, TaskStatus::Cancelled);
    }

    #[test]
    fn progress_is_fraction_of_terminal() {
        let mut g = TaskGraph::default();
        let a = g.add(Task::new("a"));
        let b = g.add(Task::new("b"));
        g.complete(a.clone(), serde_json::json!(null));
        assert!((g.progress() - 0.5).abs() < 1e-6);
        g.complete(b.clone(), serde_json::json!(null));
        assert!((g.progress() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn topological_order_respects_dependencies() {
        let mut g = TaskGraph::default();
        let a = g.add(Task::new("a"));
        let b = g.add(Task::new("b").depends_on(a.clone()));
        let order = g.topological_order();
        let pos_a = order.iter().position(|x| x == &a).expect("a in order");
        let pos_b = order.iter().position(|x| x == &b).expect("b in order");
        assert!(pos_a < pos_b);
    }
}
