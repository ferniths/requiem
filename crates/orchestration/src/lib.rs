//! REQUIEM orchestration: scheduler, agent runtime, nested subagent loop.
//!
//! The orchestration crate sits between the model client and the tool
//! layer. It owns the task-graph execution loop:
//!
//! 1. The `Scheduler` polls the `TaskGraph` for available tasks.
//! 2. Each available task is dispatched to an `Agent` (spawning a new
//!    `Agent` per task when the pool is at capacity).
//! 3. The `AgentRuntime` runs the agent: builds the model prompt, calls
//!    the model, executes any requested tool calls, feeds results back,
//!    and loops until the task is done or a budget is exhausted.
//! 4. A `Verifier` can be attached to re-check a completed task before it
//!    is marked `Completed` (bounded retry).
//!
//! All model calls go through a single shared `ModelClient`.

pub mod agent_runtime;
pub mod scheduler;
pub mod subagent;
pub mod verifier;

#[cfg(test)]
pub mod test_executor {
    use async_trait::async_trait;
    use requiem_core::error::RequiemError;

    pub struct NoopExecutor;

    #[async_trait]
    impl crate::agent_runtime::ToolExecutor for NoopExecutor {
        async fn execute(
            &self,
            _name: &str,
            _arguments: &serde_json::Value,
        ) -> Result<serde_json::Value, RequiemError> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    pub fn test_executor() -> NoopExecutor {
        NoopExecutor
    }
}
