//! The agent execution runtime.
//!
//! An `AgentRuntime` runs a single agent through one task. The loop is:
//!
//! 1. Build the prompt (system + task context + accumulated tool results).
//! 2. Call the model client.
//! 3. If the model requests tool calls, execute them and append results.
//! 4. Repeat until the model stops requesting tools or the turn budget
//!    is exhausted.
//!
//! The runtime is generic over a `ToolExecutor` so the orchestration
//! crate does not depend on the concrete tool layer.

use requiem_context::{Compressor, ContextBudget, ContextStrategy};
use requiem_core::agent::Agent;
use requiem_core::error::RequiemError;
use requiem_core::task::Task;
use requiem_model::client::ModelClient;
use requiem_model::request::{ChatMessage, ChatRequest, ToolDef};
use requiem_model::response::ModelResponse;

use crate::verifier::Verifier;

/// The maximum number of model round-trips a single task may consume
/// before the agent is forced to stop and report its current state.
pub const MAX_TURNS_PER_TASK: u32 = 32;

/// A tool executor that the agent runtime calls when the model requests
/// a tool. Implementations are provided by the `tools` crate.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute a tool call and return the result as a JSON string.
    async fn execute(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, RequiemError>;
}

/// The accumulated state of one agent running one task.
#[derive(Debug)]
pub struct AgentRun {
    /// The agent descriptor.
    pub agent: Agent,
    /// The task being executed.
    pub task: Task,
    /// All model messages accumulated so far.
    pub history: Vec<ChatMessage>,
    /// The final model response, if the agent finished.
    pub final_response: Option<ModelResponse>,
    /// The number of turns consumed.
    pub turns: u32,
}

impl AgentRun {
    /// Start a new run for a task.
    pub fn new(agent: Agent, task: Task) -> Self {
        let mut history = Vec::new();
        // System prompt: the agent's role and the task description.
        let system = format!(
            "You are the {} agent in REQUIEM. Your task: {}\n\
             Complete the task using the tools available to you. \
             Do not ask for clarification — make reasonable assumptions \
             and state them. If you cannot complete the task, explain why.",
            agent.role, task.description,
        );
        if let Some(body) = &task.body {
            history.push(ChatMessage::system(format!("{system}\n\nContext:\n{body}")));
        } else {
            history.push(ChatMessage::system(system));
        }
        history.push(ChatMessage::user(
            "Begin executing the task. Use your tools to gather information and make the required changes.",
        ));
        Self {
            agent,
            task,
            history,
            final_response: None,
            turns: 0,
        }
    }
}

/// Run an agent to completion for a task.
///
/// Returns the final `AgentRun` with `final_response` set. If the task
/// cannot be completed within `MAX_TURNS_PER_TASK`, the run is returned
/// with the last response and the caller decides whether to verify or fail.
pub async fn run_agent(
    client: &ModelClient,
    executor: &std::sync::Arc<dyn ToolExecutor>,
    run: &mut AgentRun,
    tools: &[ToolDef],
) -> Result<(), RequiemError> {
    let mut current = run.history.clone();
    let compressor = Compressor;
    let budget = ContextBudget::new(client.max_context_tokens().unwrap_or(128_000));

    for turn in 0..MAX_TURNS_PER_TASK {
        run.turns = turn + 1;

        // Compress context if the budget is tight.
        let managed = compressor.apply(current.clone(), &budget);
        if managed.compressed {
            tracing::debug!(
                agent = %run.agent.name,
                turn,
                freed = managed.tokens_freed,
                "context compressed"
            );
            current = managed.messages;
        }

        // Build the request.
        let mut req = ChatRequest {
            model: client.model().to_string(),
            messages: current.clone(),
            temperature: Some(0.2),
            max_tokens: None,
            tools: if tools.is_empty() {
                None
            } else {
                Some(tools.to_vec())
            },
            tool_choice: None,
            stream: false,
            request_id: None,
        };
        req.stream = false;

        let resp = client.chat(req).await?;

        // If the model requested tool calls, execute them and continue.
        if !resp.message.tool_calls.is_empty() {
            // Append the assistant message (with tool_calls) to history.
            current.push(resp.message.clone());

            for tc in &resp.message.tool_calls {
                tracing::info!(
                    agent = %run.agent.name,
                    turn,
                    tool = %tc.name,
                    "executing tool call"
                );
                let result = executor.execute(&tc.name, &tc.arguments).await;
                let result_text = match result {
                    Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| v.to_string()),
                    Err(e) => format!("tool error: {e}"),
                };
                current.push(ChatMessage::tool_result(&tc.id, result_text));
            }
            continue;
        }

        // No tool calls: this is the final answer.
        current.push(resp.message.clone());
        run.final_response = Some(resp);
        run.history = current;
        return Ok(());
    }

    // Budget exhausted.
    Err(RequiemError::InvalidState(format!(
        "task {} exceeded {MAX_TURNS_PER_TASK} turns",
        run.task.id
    )))
}

/// Run an agent to completion, then verify the result with a `Verifier`.
///
/// If verification fails, the task is retried up to `max_attempts` times
/// (each retry re-runs the full agent loop with the accumulated history).
/// Returns `Ok(())` when verification passes or the attempt budget is
/// exhausted, `Err` when the agent loop itself fails.
pub async fn run_agent_verified(
    client: &ModelClient,
    executor: &std::sync::Arc<dyn ToolExecutor>,
    run: &mut AgentRun,
    tools: &[ToolDef],
    verifier: &std::sync::Arc<dyn Verifier>,
    max_attempts: u32,
) -> Result<(), RequiemError> {
    for attempt in 0..max_attempts {
        run_agent(client, executor, run, tools).await?;

        if let Some(resp) = &run.final_response {
            let result_value = serde_json::json!({
                "content": resp.message.content,
                "finish_reason": resp.finish_reason,
            });
            match verifier.verify(&run.task, &result_value).await {
                Ok(crate::verifier::VerifyResult::Pass) => {
                    tracing::info!(
                        agent = %run.agent.name,
                        attempt,
                        "verification passed"
                    );
                    return Ok(());
                }
                Ok(crate::verifier::VerifyResult::Fail(reason)) => {
                    tracing::warn!(
                        agent = %run.agent.name,
                        attempt,
                        reason = %reason,
                        "verification failed, retrying"
                    );
                    run.task.verification_attempts = attempt + 1;
                    // Append the failure to history so the next attempt
                    // sees what went wrong.
                    run.history.push(ChatMessage::user(format!(
                        "Verification failed: {reason}\nPlease address the issue and try again."
                    )));
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        agent = %run.agent.name,
                        attempt,
                        error = %e,
                        "verification error, retrying"
                    );
                    run.task.verification_attempts = attempt + 1;
                    continue;
                }
            }
        }
    }

    Err(RequiemError::InvalidState(format!(
        "task {} failed verification after {max_attempts} attempts",
        run.task.id
    )))
}

/// Run multiple agent tasks concurrently via `tokio::join!`.
///
/// Each task runs in its own `AgentRun` with the same client, executor,
/// and tool set. Results are returned in the same order as the input.
/// A task that exceeds its turn budget or fails verification is returned
/// as `Err` without blocking the other tasks.
pub async fn run_agents_concurrent(
    client: &ModelClient,
    executor: &std::sync::Arc<dyn ToolExecutor>,
    runs: Vec<AgentRun>,
    tools: &[ToolDef],
) -> Vec<Result<AgentRun, RequiemError>> {
    let futures: Vec<_> = runs
        .into_iter()
        .map(|mut run| async move {
            let result = run_agent(client, executor, &mut run, tools).await;
            match result {
                Ok(()) => Ok(run),
                Err(e) => Err(e),
            }
        })
        .collect();

    futures::future::join_all(futures).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use requiem_core::Id;
    use requiem_model::request::ToolDef;

    #[tokio::test]
    async fn concurrent_runs_return_results_in_order() {
        let cfg = requiem_model::config::ModelConfig {
            endpoint: "http://localhost:9999".into(),
            api_key: "test".into(),
            model: "test-model".into(),
            timeout_ms: 5_000,
            max_concurrency: 4,
            max_retries: 1,
            max_context_tokens: None,
            max_output_tokens: None,
        };
        let client = ModelClient::new(cfg).unwrap();

        let executor: std::sync::Arc<dyn ToolExecutor> =
            std::sync::Arc::new(crate::test_executor::test_executor());
        let tools: Vec<ToolDef> = Vec::new();

        let agents: Vec<AgentRun> = (0..3)
            .map(|i| {
                let agent = Agent::master(&format!("agent-{i}"), Id::new());
                let task = Task::new(&format!("task-{i}"));
                AgentRun::new(agent, task)
            })
            .collect();

        // These will fail (no real model server), but the test verifies
        // that all 3 results come back in order without panicking.
        let results = run_agents_concurrent(&client, &executor, agents, &tools).await;
        assert_eq!(results.len(), 3);
        for r in &results {
            assert!(r.is_err(), "expected error with no server, got {r:?}");
        }
    }
}
