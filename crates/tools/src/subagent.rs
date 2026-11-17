//! Subagent spawning tool.
//!
//! The model can call `spawn_subagent` to delegate a subtask to a child
//! agent. The child runs with the same model but a focused context slice
//! and the parent's permission profile. The result is returned to the
//! parent as a tool result.
//!
//! Use `make_subagent_tool` to build a `SubagentTool` with the current
//! parent agent and task context. Event feeds and cancellation flags are
//! optional and propagate to the child so nested runs appear in the same
//! live feed and stop together.

use serde_json::Value;

use crate::{Result, Tool};

use requiem_core::agent::Agent;
use requiem_core::task::Task;
use requiem_model::client::ModelClient;
use requiem_orchestration::agent_runtime::{
    run_agent_halted, AgentRun, CancelFlag, EventTx, ToolExecutor,
};
use requiem_orchestration::subagent::{spawn_subagent, SubagentSpec};

/// Build a `SubagentTool` bound to the current parent agent and task session.
///
/// `parent` is the agent that will own the spawned subagents.
/// `task_session` is the task-session `Id` for this run.
/// `tools` are the tool defs the subagent can use.
pub fn make_subagent_tool(
    client: std::sync::Arc<ModelClient>,
    executor: std::sync::Arc<dyn ToolExecutor>,
    parent: Agent,
    task_session: requiem_core::Id,
    tools: Vec<requiem_model::request::ToolDef>,
) -> SubagentTool {
    SubagentTool {
        client,
        executor,
        parent,
        task_session,
        tools,
        events: None,
        cancel: None,
    }
}

/// A tool that lets the model spawn a subagent to handle a subtask.
pub struct SubagentTool {
    client: std::sync::Arc<ModelClient>,
    executor: std::sync::Arc<dyn ToolExecutor>,
    parent: Agent,
    task_session: requiem_core::Id,
    tools: Vec<requiem_model::request::ToolDef>,
    /// Event feed the child's run broadcasts into (same feed as parent).
    events: Option<EventTx>,
    /// Cancellation flag shared with the parent run.
    cancel: Option<CancelFlag>,
}

impl SubagentTool {
    /// Attach an event feed so nested runs emit into the same stream.
    pub fn with_events(mut self, events: EventTx) -> Self {
        self.events = Some(events);
        self
    }

    /// Attach a cancellation flag; cancelling the parent cancels children.
    pub fn with_cancel(mut self, cancel: CancelFlag) -> Self {
        self.cancel = Some(cancel);
        self
    }
}

#[async_trait::async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "spawn_subagent"
    }

    fn description(&self) -> &str {
        "Delegate a subtask to a child agent. The child runs the same model \
         with a focused context. Arguments: name (string), role (string — \
         researcher/implementer/tester/reviewer/debugger/architect/planner/\
         security/documenter/refactorer), task (string), context (optional \
         string — focused context slice for the child)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name for the subagent (e.g. 'research-1')"
                },
                "role": {
                    "type": "string",
                    "enum": [
                        "researcher",
                        "implementer",
                        "tester",
                        "reviewer",
                        "debugger",
                        "architect",
                        "planner",
                        "security",
                        "documenter",
                        "refactorer"
                    ],
                    "description": "The role the subagent will run under"
                },
                "task": {
                    "type": "string",
                    "description": "The subtask description for the child agent"
                },
                "context": {
                    "type": "string",
                    "description": "Optional focused context (files, findings, constraints)"
                }
            },
            "required": ["name", "role", "task"]
        })
    }

    async fn execute(&self, args: &Value) -> Result<Value> {
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let role_str = args.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let task_desc = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
        let context = args.get("context").and_then(|v| v.as_str());

        if name.is_empty() || role_str.is_empty() || task_desc.is_empty() {
            return Err(crate::ToolError::InvalidArgument(
                "name, role, and task are all required".to_string(),
            ));
        }

        let role = match role_str {
            "researcher" => requiem_core::Role::Researcher,
            "implementer" => requiem_core::Role::Implementer,
            "tester" => requiem_core::Role::Tester,
            "reviewer" => requiem_core::Role::Reviewer,
            "debugger" => requiem_core::Role::Debugger,
            "architect" => requiem_core::Role::Architect,
            "planner" => requiem_core::Role::Planner,
            "security" => requiem_core::Role::Security,
            "documenter" => requiem_core::Role::Documenter,
            "refactorer" => requiem_core::Role::Refactorer,
            other => {
                return Err(crate::ToolError::InvalidArgument(format!(
                    "unknown role: {other}"
                )))
            }
        };

        let mut sub_task = Task::new(task_desc);
        sub_task.body = context.map(|s| s.to_string());

        let spec = SubagentSpec {
            name: name.to_string(),
            role,
            task_id: sub_task.id.clone(),
            context: context.map(|s| s.to_string()),
        };

        let handle = spawn_subagent(&self.parent, spec, self.task_session.clone())
            .map_err(|e| crate::ToolError::InvalidArgument(e.to_string()))?;

        let mut run = AgentRun::new(handle.agent, sub_task);

        // Run the child inline: this function is async, so awaiting the
        // nested agent loop is safe on any runtime flavor. The child's
        // events merge into the parent feed and cancellation propagates.
        let result = run_agent_halted(
            &self.client,
            &self.executor,
            &mut run,
            &self.tools,
            self.events.as_ref(),
            self.cancel.as_ref(),
        )
        .await;

        let sub_response = run.final_response.as_ref().and_then(|resp| {
            resp.message
                .content
                .as_ref()
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

        match result {
            Ok(()) => Ok(serde_json::json!({
                "status": "completed",
                "agent_name": name,
                "response": sub_response.unwrap_or_else(|| "(no response text)".into()),
                "turns": run.turns,
                "tokens": {
                    "input": run.token_totals.input,
                    "output": run.token_totals.output,
                },
            })),
            Err(e) => Ok(serde_json::json!({
                "status": "failed",
                "agent_name": name,
                "error": e.to_string(),
                "turns": run.turns,
                "tokens": {
                    "input": run.token_totals.input,
                    "output": run.token_totals.output,
                },
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool() -> SubagentTool {
        let cfg = requiem_model::config::ModelConfig {
            endpoint: "http://localhost:9999".into(),
            api_key: "test-key".into(),
            model: "test-model".into(),
            timeout_ms: 5_000,
            max_concurrency: 1,
            max_retries: 1,
            max_context_tokens: None,
            max_output_tokens: None,
        };
        SubagentTool {
            client: std::sync::Arc::new(ModelClient::new(cfg).expect("valid config should build")),
            executor: std::sync::Arc::new(crate::tests::noop_executor()),
            parent: Agent::master("test-master", requiem_core::Id::new()),
            task_session: requiem_core::Id::new(),
            tools: vec![],
            events: None,
            cancel: None,
        }
    }

    #[test]
    fn schema_is_valid_json() {
        let tool = test_tool();
        let schema = tool.schema();
        let s = serde_json::to_string(&schema).unwrap();
        assert!(!s.is_empty());
    }

    #[tokio::test]
    async fn execute_rejects_missing_args() {
        let tool = test_tool();
        let result = tool.execute(&serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_unknown_role() {
        let tool = test_tool();
        let result = tool
            .execute(&serde_json::json!({
                "name": "test",
                "role": "unknown-role",
                "task": "do something"
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cancelled_flag_short_circuits_child_run() {
        let tool = test_tool().with_cancel(CancelFlag::new());
        if let Some(flag) = tool.cancel.as_ref() {
            flag.cancel();
        }
        let result = tool
            .execute(&serde_json::json!({
                "name": "child",
                "role": "researcher",
                "task": "explore"
            }))
            .await
            .unwrap();
        assert_eq!(result["status"], "failed");
        assert!(
            result["error"].as_str().unwrap().contains("cancel"),
            "expected cancellation error, got {}",
            result["error"]
        );
    }

    #[tokio::test]
    async fn events_feed_receives_child_run_events() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(64);
        let tool = test_tool().with_events(tx);

        // The child will fail to reach the model server, but the run
        // start / failure events must still arrive on the feed.
        let _ = tool
            .execute(&serde_json::json!({
                "name": "child",
                "role": "researcher",
                "task": "explore"
            }))
            .await;

        let mut saw_started = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(
                ev,
                requiem_core::events::Event::RunStarted { ref agent, .. } if agent == "child"
            ) {
                saw_started = true;
            }
        }
        assert!(saw_started, "RunStarted event for the child must be broadcast");
    }

    #[tokio::test]
    async fn spawn_refused_at_max_depth() {
        // Build a parent already at max depth: children are refused.
        let mut deep = Agent::master("deep", requiem_core::Id::new());
        deep.depth = requiem_orchestration::subagent::MAX_DEPTH;
        let cfg = requiem_model::config::ModelConfig {
            endpoint: "http://localhost:9999".into(),
            api_key: "test-key".into(),
            model: "test-model".into(),
            timeout_ms: 5_000,
            max_concurrency: 1,
            max_retries: 1,
            max_context_tokens: None,
            max_output_tokens: None,
        };
        let tool = SubagentTool {
            client: std::sync::Arc::new(ModelClient::new(cfg).unwrap()),
            executor: std::sync::Arc::new(crate::tests::noop_executor()),
            parent: deep,
            task_session: requiem_core::Id::new(),
            tools: vec![],
            events: None,
            cancel: None,
        };
        let result = tool
            .execute(&serde_json::json!({
                "name": "too-deep",
                "role": "tester",
                "task": "test"
            }))
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("depth"), "got: {err}");
    }
}
