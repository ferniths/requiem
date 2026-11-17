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
use requiem_orchestration::subagent::SubagentSpec;

/// Build a `SubagentTool` bound to the current parent agent and task session.
///
/// `parent` is the agent that will own the spawned subagents.
/// `task_session` is the task-session `Id` for this run.
/// `tools` are the tool defs the subagent can use.
/// `max_depth` is the maximum nesting depth (from user config).
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
        max_depth: requiem_orchestration::subagent::MAX_DEPTH,
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
    /// Configurable depth limit (from user config).
    max_depth: u32,
}

impl SubagentTool {
    /// Set the maximum nesting depth (override the default).
    pub fn with_max_depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

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

        let handle = requiem_orchestration::subagent::spawn_subagent_with_limit(
            &self.parent,
            spec,
            self.task_session.clone(),
            self.max_depth,
        )
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

/// Build a `ParallelSubagentTool` — lets the model spawn *multiple*
/// subagents concurrently in a single tool call.
pub fn make_parallel_subagent_tool(
    client: std::sync::Arc<ModelClient>,
    executor: std::sync::Arc<dyn ToolExecutor>,
    parent: Agent,
    task_session: requiem_core::Id,
    tools: Vec<requiem_model::request::ToolDef>,
) -> ParallelSubagentTool {
    ParallelSubagentTool {
        client,
        executor,
        parent,
        task_session,
        tools,
        events: None,
        cancel: None,
        max_depth: requiem_orchestration::subagent::MAX_DEPTH,
    }
}

/// A tool that lets the model spawn multiple subagents concurrently.
///
/// Arguments: `subagents` — an array of objects with `name`, `role`,
/// `task`, and optional `context`. All children run in parallel; a
/// single failure does not abort the siblings.
pub struct ParallelSubagentTool {
    client: std::sync::Arc<ModelClient>,
    executor: std::sync::Arc<dyn ToolExecutor>,
    parent: Agent,
    task_session: requiem_core::Id,
    tools: Vec<requiem_model::request::ToolDef>,
    events: Option<EventTx>,
    cancel: Option<CancelFlag>,
    max_depth: u32,
}

impl ParallelSubagentTool {
    pub fn with_max_depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    pub fn with_events(mut self, events: EventTx) -> Self {
        self.events = Some(events);
        self
    }

    pub fn with_cancel(mut self, cancel: CancelFlag) -> Self {
        self.cancel = Some(cancel);
        self
    }
}

#[async_trait::async_trait]
impl Tool for ParallelSubagentTool {
    fn name(&self) -> &str {
        "spawn_subagents"
    }

    fn description(&self) -> &str {
        "Spawn multiple subagents concurrently in a single call. All \
         subagents run in parallel; a failure in one does not abort the \
         others. Arguments: subagents (array of objects, each with \
         name/role/task/context)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "subagents": {
                    "type": "array",
                    "description": "Array of subagent specs to run concurrently",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {
                                "type": "string",
                                "description": "Name for this subagent"
                            },
                            "role": {
                                "type": "string",
                                "enum": [
                                    "researcher", "implementer", "tester",
                                    "reviewer", "debugger", "architect",
                                    "planner", "security", "documenter", "refactorer"
                                ],
                                "description": "The role this subagent will run under"
                            },
                            "task": {
                                "type": "string",
                                "description": "The subtask description for this child agent"
                            },
                            "context": {
                                "type": "string",
                                "description": "Optional focused context slice for the child"
                            }
                        },
                        "required": ["name", "role", "task"]
                    }
                }
            },
            "required": ["subagents"]
        })
    }

    async fn execute(&self, args: &Value) -> Result<Value> {
        let specs = match args.get("subagents").and_then(|v| v.as_array()) {
            Some(a) if !a.is_empty() => a,
            _ => {
                return Err(crate::ToolError::InvalidArgument(
                    "subagents must be a non-empty array".to_string(),
                ))
            }
        };

        // Validate and build all (agent, task) pairs up front so a bad
        // spec in one entry rejects the entire batch early.
        let mut tasks: Vec<(Agent, Task)> = Vec::with_capacity(specs.len());

        for (i, spec) in specs.iter().enumerate() {
            let name = spec.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let role_str = spec.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let task_desc = spec.get("task").and_then(|v| v.as_str()).unwrap_or("");

            if name.is_empty() || role_str.is_empty() || task_desc.is_empty() {
                return Err(crate::ToolError::InvalidArgument(format!(
                    "subagents[{i}]: name, role, and task are all required"
                )));
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
                        "subagents[{i}]: unknown role: {other}"
                    )))
                }
            };

            let mut task = Task::new(task_desc);
            let ctx = spec.get("context").and_then(|v| v.as_str());
            task.body = ctx.map(|s| s.to_string());

            let saspec = SubagentSpec {
                name: name.to_string(),
                role,
                task_id: task.id.clone(),
                context: ctx.map(|s| s.to_string()),
            };

            let handle = requiem_orchestration::subagent::spawn_subagent_with_limit(
                &self.parent,
                saspec,
                self.task_session.clone(),
                self.max_depth,
            )
            .map_err(|e| crate::ToolError::InvalidArgument(format!("subagents[{i}]: {e}")))?;

            tasks.push((handle.agent, task));
        }

        // Run all subagents concurrently. Each child's error is captured
        // in its own result entry without aborting siblings.
        let futures_iter = tasks.into_iter().map(|(agent, task)| {
            let client = self.client.clone();
            let executor = self.executor.clone();
            let tools = self.tools.clone();
            let events = self.events.clone();
            let cancel = self.cancel.clone();
            async move {
                let mut run = AgentRun::new(agent.clone(), task);
                let result = run_agent_halted(
                    &client,
                    &executor,
                    &mut run,
                    &tools,
                    events.as_ref(),
                    cancel.as_ref(),
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
                    Ok(()) => serde_json::json!({
                        "status": "completed",
                        "agent_name": agent.name,
                        "response": sub_response.unwrap_or_else(|| "(no response text)".into()),
                        "turns": run.turns,
                        "tokens": {
                            "input": run.token_totals.input,
                            "output": run.token_totals.output,
                        }
                    }),
                    Err(e) => serde_json::json!({
                        "status": "failed",
                        "agent_name": agent.name,
                        "error": e.to_string(),
                        "turns": run.turns,
                        "tokens": {
                            "input": run.token_totals.input,
                            "output": run.token_totals.output,
                        }
                    }),
                }
            }
        });

        let results = futures::future::join_all(futures_iter).await;

        Ok(serde_json::json!({
            "results": results,
            "completed": results.iter().filter(|r| r["status"] == "completed").count(),
            "failed": results.iter().filter(|r| r["status"] == "failed").count(),
        }))
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
            max_depth: 4,
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
        assert!(
            saw_started,
            "RunStarted event for the child must be broadcast"
        );
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
            max_depth: 4,
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

    // ---- ParallelSubagentTool tests ----

    fn test_parallel_tool() -> ParallelSubagentTool {
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
        ParallelSubagentTool {
            client: std::sync::Arc::new(ModelClient::new(cfg).unwrap()),
            executor: std::sync::Arc::new(crate::tests::noop_executor()),
            parent: Agent::master("test-master", requiem_core::Id::new()),
            task_session: requiem_core::Id::new(),
            tools: vec![],
            events: None,
            cancel: None,
            max_depth: 4,
        }
    }

    #[test]
    fn parallel_schema_is_valid_json() {
        let tool = test_parallel_tool();
        let schema = tool.schema();
        let s = serde_json::to_string(&schema).unwrap();
        assert!(s.contains("subagents"));
        assert!(s.contains("name"));
        assert!(s.contains("role"));
    }

    #[test]
    fn parallel_tool_name() {
        let tool = test_parallel_tool();
        assert_eq!(Tool::name(&tool), "spawn_subagents");
    }

    #[tokio::test]
    async fn parallel_rejects_empty_array() {
        let tool = test_parallel_tool();
        let result = tool.execute(&serde_json::json!({ "subagents": [] })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parallel_rejects_missing_subagents_key() {
        let tool = test_parallel_tool();
        let result = tool.execute(&serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parallel_rejects_missing_required_field() {
        let tool = test_parallel_tool();
        let result = tool
            .execute(&serde_json::json!({
                "subagents": [{ "role": "researcher", "task": "x" }]
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parallel_rejects_unknown_role() {
        let tool = test_parallel_tool();
        let result = tool
            .execute(&serde_json::json!({
                "subagents": [{ "name": "a", "role": "unknown", "task": "x" }]
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parallel_cancelled_flag_short_circuits_all_children() {
        let tool = test_parallel_tool().with_cancel(CancelFlag::new());
        if let Some(flag) = tool.cancel.as_ref() {
            flag.cancel();
        }
        let result = tool
            .execute(&serde_json::json!({
                "subagents": [
                    { "name": "a", "role": "researcher", "task": "x" },
                    { "name": "b", "role": "tester", "task": "y" }
                ]
            }))
            .await
            .unwrap();
        // Both children should fail (cancelled), not abort.
        assert_eq!(result["failed"], 2);
        assert_eq!(result["completed"], 0);
        for r in result["results"].as_array().unwrap() {
            assert_eq!(r["status"], "failed");
        }
    }

    #[tokio::test]
    async fn parallel_spawn_refused_at_max_depth() {
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
        let tool = ParallelSubagentTool {
            client: std::sync::Arc::new(ModelClient::new(cfg).unwrap()),
            executor: std::sync::Arc::new(crate::tests::noop_executor()),
            parent: deep,
            task_session: requiem_core::Id::new(),
            tools: vec![],
            events: None,
            cancel: None,
            max_depth: 4,
        };
        let result = tool
            .execute(&serde_json::json!({
                "subagents": [{ "name": "a", "role": "researcher", "task": "x" }]
            }))
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("depth"), "got: {err}");
    }
}
