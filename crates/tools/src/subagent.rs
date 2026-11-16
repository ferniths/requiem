//! Subagent spawning tool.
//!
//! The model can call `spawn_subagent` to delegate a subtask to a child
//! agent. The child runs with the same model but a focused context slice
//! and the parent's permission profile. The result is returned to the
//! parent as a tool result.
//!
//! Use `make_subagent_tool` to build a `Box<dyn Tool>` with the current
//! parent agent and task context.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::{Result, Tool};

use requiem_core::agent::Agent;
use requiem_core::task::Task;
use requiem_model::client::ModelClient;
use requiem_orchestration::agent_runtime::{run_agent, AgentRun, ToolExecutor};
use requiem_orchestration::subagent::{spawn_subagent, SubagentSpec};

/// Build a `SubagentTool` bound to the current parent agent and task session.
///
/// `parent` is the agent that will own the spawned subagents.
/// `task_session` is the task-session `Id` for this run.
/// `tools` are the tool defs the subagent can use.
pub fn make_subagent_tool(
    client: Arc<ModelClient>,
    executor: Arc<dyn ToolExecutor>,
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
    }
}

/// A tool that lets the model spawn a subagent to handle a subtask.
pub struct SubagentTool {
    client: Arc<ModelClient>,
    executor: Arc<dyn ToolExecutor>,
    parent: Agent,
    task_session: requiem_core::Id,
    tools: Vec<requiem_model::request::ToolDef>,
}

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

    fn execute(&self, args: &Value) -> Result<Value> {
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

        let context_str = context.map(|s| s.to_string());
        let spec = SubagentSpec {
            name: name.to_string(),
            role,
            task_id: Task::new(task_desc).id,
            context: context_str.clone(),
        };

        let handle = spawn_subagent(&self.parent, spec, self.task_session.clone())
            .map_err(|e| crate::ToolError::File(e.to_string()))?;

        let mut sub_task = Task::new(task_desc);
        if let Some(ctx) = context_str {
            sub_task.body = Some(ctx);
        }

        // Clone what we need for the async closure.
        let client = self.client.clone();
        let executor = self.executor.clone();
        let tools = self.tools.clone();
        let sub_agent = handle.agent.clone();
        let task = sub_task.clone();

        // Shared handle so the async block can write the final response
        // and we can read it back after block_on returns.
        let run_handle: Arc<Mutex<Option<AgentRun>>> = Arc::new(Mutex::new(None));

        // Run the subagent. block_in_place lets us call block_on from
        // within an async context (the Tool::execute is sync, but the
        // parent agent runtime is async).
        let run_handle_inner = run_handle.clone();
        let result = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let mut run = AgentRun::new(sub_agent, task);
                let res = run_agent(&client, &executor, &mut run, &tools).await;
                *run_handle_inner.lock().unwrap() = Some(run);
                res
            })
        });

        // Read the subagent's final response from the shared handle.
        let sub_response = run_handle.lock().unwrap().as_ref().and_then(|r| {
            r.final_response.as_ref().and_then(|resp| {
                resp.message
                    .content
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
        });

        match result {
            Ok(()) => Ok(serde_json::json!({
                "status": "completed",
                "agent_name": name,
                "response": sub_response.unwrap_or_else(|| "(no response text)".into()),
            })),
            Err(e) => Ok(serde_json::json!({
                "status": "failed",
                "agent_name": name,
                "error": e.to_string(),
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
            client: Arc::new(ModelClient::new(cfg).expect("valid config should build")),
            executor: Arc::new(crate::tests::noop_executor()),
            parent: Agent::master("test-master", requiem_core::Id::new()),
            task_session: requiem_core::Id::new(),
            tools: vec![],
        }
    }

    #[test]
    fn schema_is_valid_json() {
        let tool = test_tool();
        let schema = tool.schema();
        let s = serde_json::to_string(&schema).unwrap();
        assert!(!s.is_empty());
    }

    #[test]
    fn execute_rejects_missing_args() {
        let tool = test_tool();
        let result = tool.execute(&serde_json::json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn execute_rejects_unknown_role() {
        let tool = test_tool();
        let result = tool.execute(&serde_json::json!({
            "name": "test",
            "role": "unknown-role",
            "task": "do something"
        }));
        assert!(result.is_err());
    }
}
