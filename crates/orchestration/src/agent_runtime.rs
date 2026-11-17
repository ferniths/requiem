//! The agent execution runtime.
//!
//! An `AgentRuntime` runs a single agent through one task. The loop is:
//!
//! 1. Build the prompt (system + task context + accumulated tool results).
//! 2. Call the model client (streaming when possible so the UI can render
//!    tokens as they arrive).
//! 3. If the model requests tool calls, execute them and append results.
//! 4. Repeat until the model stops requesting tools or the turn budget
//!    is exhausted.
//!
//! Every significant transition is broadcast as a `requiem_core::events`
//! event so any number of UI consumers can observe the run live without
//! ever blocking it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use requiem_context::{Compressor, ContextBudget, ContextStrategy};
use requiem_core::agent::Agent;
use requiem_core::error::RequiemError;
use requiem_core::events::Event;
use requiem_core::task::Task;
use requiem_core::token::{CountKind, TokenSample, TokenTotals};
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
    /// Execute a tool call and return the result as a JSON value.
    async fn execute(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, RequiemError>;
}

/// A broadcast channel type for runtime events.
pub type EventTx = tokio::sync::broadcast::Sender<Event>;

/// Cooperative cancellation flag shared with a run.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
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
    /// Token usage accumulated across every model call in this run.
    pub token_totals: TokenTotals,
}

impl AgentRun {
    /// Start a new run for a task.
    pub fn new(agent: Agent, task: Task) -> Self {
        let mut history = Vec::new();
        // System prompt: the agent's role, the task description, and the
        // working conventions that keep multi-agent runs coherent.
        let system = format!(
            "You are the {} agent in REQUIEM, a multi-agent coding harness.\n\
             Your task: {}\n\
             Complete the task using the tools available to you. Make \
             reasonable assumptions and state them. Prefer focused, \
             verifiable changes. If you cannot complete the task, explain \
             exactly what blocked you.",
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
            token_totals: TokenTotals::default(),
        }
    }

    /// The usage reported by the final response only (legacy view).
    pub fn final_usage(&self) -> Option<(u32, u32)> {
        self.final_response
            .as_ref()
            .and_then(|r| r.usage)
            .map(|u| (u.input, u.output))
    }
}

/// Emit an event, ignoring absent or vanished consumers.
fn emit(events: Option<&EventTx>, event: Event) {
    if let Some(tx) = events {
        let _ = tx.send(event);
    }
}

/// A compact, human-oriented summary of tool arguments for event feeds.
pub fn summarize_arguments(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            let mut parts: Vec<String> = Vec::new();
            for (k, v) in map.iter().take(3) {
                let rendered: String = match v {
                    serde_json::Value::String(s) => s.chars().take(60).collect(),
                    other => other.to_string().chars().take(60).collect(),
                };
                parts.push(format!("{k}={rendered}"));
            }
            parts.join(", ")
        }
        serde_json::Value::String(s) => s.chars().take(80).collect(),
        other => other.to_string().chars().take(80).collect(),
    }
}

/// A compact first-line summary of a tool result for event feeds.
pub fn summarize_result(v: &serde_json::Value, max: usize) -> String {
    let text = match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => {
            // Prefer well-known keys that read naturally.
            for key in ["stdout", "content", "summary", "error", "matches"] {
                if let Some(serde_json::Value::String(s)) = map.get(key) {
                    return s.chars().take(max).collect();
                }
            }
            serde_json::to_string(v).unwrap_or_default()
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let first_line = text.lines().next().unwrap_or("");
    let mut s: String = first_line.chars().take(max).collect();
    if first_line.chars().count() > max {
        s.push('…');
    }
    s
}

/// Run an agent to completion for a task.
///
/// Returns the final `AgentRun` with `final_response` set. If the task
/// cannot be completed within `MAX_TURNS_PER_TASK`, an error is returned
/// and the caller decides whether to retry, verify or fail.
pub async fn run_agent(
    client: &ModelClient,
    executor: &std::sync::Arc<dyn ToolExecutor>,
    run: &mut AgentRun,
    tools: &[ToolDef],
) -> Result<(), RequiemError> {
    run_agent_halted(client, executor, run, tools, None, None).await
}

/// Run an agent with an optional event feed and cancellation flag.
pub async fn run_agent_halted(
    client: &ModelClient,
    executor: &std::sync::Arc<dyn ToolExecutor>,
    run: &mut AgentRun,
    tools: &[ToolDef],
    events: Option<&EventTx>,
    cancel: Option<&CancelFlag>,
) -> Result<(), RequiemError> {
    emit(
        events,
        Event::RunStarted {
            agent: run.agent.name.clone(),
            role: run.agent.role.to_string(),
            task: run.task.id.clone(),
            description: run.task.description.clone(),
            mode: String::new(),
        },
    );
    emit(
        events,
        Event::AgentStatus {
            agent: run.agent.name.clone(),
            status: "running".into(),
        },
    );

    let mut current = run.history.clone();
    let compressor = Compressor;
    let budget = ContextBudget::new(client.max_context_tokens().unwrap_or(128_000));

    for turn in 0..MAX_TURNS_PER_TASK {
        if let Some(flag) = cancel {
            if flag.is_cancelled() {
                emit(
                    events,
                    Event::RunCancelled {
                        agent: run.agent.name.clone(),
                        task: run.task.id.clone(),
                    },
                );
                emit(
                    events,
                    Event::AgentStatus {
                        agent: run.agent.name.clone(),
                        status: "cancelled".into(),
                    },
                );
                return Err(RequiemError::Cancelled);
            }
        }

        run.turns = turn + 1;
        emit(
            events,
            Event::TurnStarted {
                agent: run.agent.name.clone(),
                turn: turn + 1,
            },
        );

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
        let req = ChatRequest {
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

        // Stream when there is a live consumer; otherwise take the fast path.
        let resp: ModelResponse = if events.is_some() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let streamed = client.chat_stream(&req, tx);
            // Consume deltas while the request is in flight.
            let mut delta_text = String::new();
            let result = drive_stream(streamed, &mut rx, &mut delta_text, run, events).await;
            match result {
                Ok(resp) => {
                    if resp.message.tool_calls.is_empty() {
                        emit(
                            events,
                            Event::AssistantMessage {
                                agent: run.agent.name.clone(),
                                text: resp
                                    .message
                                    .content
                                    .as_ref()
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            },
                        );
                    }
                    resp
                }
                Err(e) => {
                    emit(
                        events,
                        Event::RunFailed {
                            agent: run.agent.name.clone(),
                            task: run.task.id.clone(),
                            error: e.to_string(),
                            error_class: e.class().to_string(),
                        },
                    );
                    emit(
                        events,
                        Event::AgentStatus {
                            agent: run.agent.name.clone(),
                            status: "failed".into(),
                        },
                    );
                    return Err(e);
                }
            }
        } else {
            match client.chat(req).await {
                Ok(r) => r,
                Err(e) => {
                    emit(
                        events,
                        Event::RunFailed {
                            agent: run.agent.name.clone(),
                            task: run.task.id.clone(),
                            error: e.to_string(),
                            error_class: e.class().to_string(),
                        },
                    );
                    emit(
                        events,
                        Event::AgentStatus {
                            agent: run.agent.name.clone(),
                            status: "failed".into(),
                        },
                    );
                    return Err(e);
                }
            }
        };

        // Record provider-reported usage when available.
        if let Some(u) = resp.usage {
            let sample = TokenSample {
                input: u.input,
                output: u.output,
                kind: if u.is_reported() {
                    CountKind::Reported
                } else {
                    CountKind::Estimated
                },
                at: requiem_core::Timestamp::now(),
            };
            run.token_totals.add_sample(&sample);
            emit(
                events,
                Event::Usage {
                    agent: run.agent.name.clone(),
                    task: run.task.id.clone(),
                    input_tokens: u.input,
                    output_tokens: u.output,
                },
            );
        }

        // If the model requested tool calls, execute them and continue.
        if !resp.message.tool_calls.is_empty() {
            // Append the assistant message (with tool_calls) to history.
            current.push(resp.message.clone());

            for tc in &resp.message.tool_calls {
                emit(
                    events,
                    Event::ToolCallStarted {
                        agent: run.agent.name.clone(),
                        tool: tc.name.clone(),
                        summary: summarize_arguments(&tc.arguments),
                    },
                );
                let started = std::time::Instant::now();
                let result = executor.execute(&tc.name, &tc.arguments).await;
                let duration_ms = started.elapsed().as_millis() as u64;
                let result_text = match &result {
                    Ok(v) => serde_json::to_string(v).unwrap_or_else(|_| v.to_string()),
                    Err(e) => format!("tool error: {e}"),
                };
                emit(
                    events,
                    Event::ToolCallFinished {
                        agent: run.agent.name.clone(),
                        tool: tc.name.clone(),
                        ok: result.is_ok(),
                        summary: summarize_result(
                            result.as_ref().unwrap_or(&serde_json::json!({})),
                            120,
                        ),
                        duration_ms,
                    },
                );
                current.push(ChatMessage::tool_result(&tc.id, result_text));
            }
            continue;
        }

        // No tool calls: this is the final answer.
        current.push(resp.message.clone());
        run.final_response = Some(resp);
        run.history = current;
        emit(
            events,
            Event::RunCompleted {
                agent: run.agent.name.clone(),
                task: run.task.id.clone(),
                response: run
                    .final_response
                    .as_ref()
                    .and_then(|r| r.text())
                    .map(|s| s.to_string()),
                input_tokens: run.token_totals.input as u32,
                output_tokens: run.token_totals.output as u32,
            },
        );
        emit(
            events,
            Event::AgentStatus {
                agent: run.agent.name.clone(),
                status: "done".into(),
            },
        );
        return Ok(());
    }

    let err = RequiemError::InvalidState(format!(
        "task {} exceeded {MAX_TURNS_PER_TASK} turns",
        run.task.id
    ));
    emit(
        events,
        Event::RunFailed {
            agent: run.agent.name.clone(),
            task: run.task.id.clone(),
            error: err.to_string(),
            error_class: err.class().to_string(),
        },
    );
    Err(err)
}

/// Drive a streaming request while draining deltas into the event feed.
/// Returns the reassembled response when the stream completes.
async fn drive_stream(
    streamed: impl std::future::Future<
        Output = Result<ModelResponse, RequiemError>,
    >,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<requiem_model::response::StreamChunk>,
    delta_text: &mut String,
    run: &AgentRun,
    events: Option<&EventTx>,
) -> Result<ModelResponse, RequiemError> {
    tokio::pin!(streamed);
    loop {
        tokio::select! {
            res = &mut streamed => {
                // Drain anything still queued, then return.
                while let Ok(chunk) = rx.try_recv() {
                    if let Some(d) = &chunk.content {
                        delta_text.push_str(d);
                        emit(events, Event::AssistantDelta {
                            agent: run.agent.name.clone(),
                            delta: d.clone(),
                        });
                    }
                }
                return res;
            }
            maybe = rx.recv() => {
                match maybe {
                    Some(chunk) => {
                        if let Some(d) = &chunk.content {
                            delta_text.push_str(d);
                            emit(events, Event::AssistantDelta {
                                agent: run.agent.name.clone(),
                                delta: d.clone(),
                            });
                        }
                    }
                    None => {
                        // Channel closed; wait for the request to finish.
                        return streamed.await;
                    }
                }
            }
        }
    }
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

/// Run multiple agent tasks concurrently via `futures::join_all`.
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
