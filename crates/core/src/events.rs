//! Structured runtime events for live UI updates.
//!
//! The orchestration runtime emits these as work happens — agent starts,
//! tool calls, streamed assistant text, token usage, completions and
//! errors — and every interface (the interactive TUI, the plain REPL,
//! logs) consumes the same stream. Rendering stays independent from the
//! model runtime: events are broadcast, never polled from run state.

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// A live runtime event.
///
/// Events are small, serializable and cloneable. They are broadcast on a
/// tokio broadcast channel; consumers must tolerate loss (a slow UI must
/// never block the agent runtime).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A task was accepted and a run is starting.
    RunStarted {
        /// Agent running the task.
        agent: String,
        /// Agent role.
        role: String,
        /// The task.
        task: Id,
        /// One-line task description.
        description: String,
        /// Operating mode at submission time.
        mode: String,
    },
    /// A model turn began.
    TurnStarted {
        agent: String,
        /// 1-based turn number.
        turn: u32,
    },
    /// Incremental assistant text (streaming delta).
    AssistantDelta {
        agent: String,
        /// The new text fragment.
        delta: String,
    },
    /// A complete assistant message (non-streaming or end of stream).
    AssistantMessage { agent: String, text: String },
    /// A tool call is starting.
    ToolCallStarted {
        agent: String,
        tool: String,
        /// A compact, human-oriented summary of the arguments.
        summary: String,
    },
    /// A tool call finished.
    ToolCallFinished {
        agent: String,
        tool: String,
        ok: bool,
        /// A compact result summary (first line, truncated).
        summary: String,
        /// Wall-clock duration in milliseconds.
        duration_ms: u64,
    },
    /// A subagent was spawned.
    AgentSpawned {
        parent: String,
        child: String,
        role: String,
        /// The subtask description.
        description: String,
    },
    /// Agent status changed.
    AgentStatus { agent: String, status: String },
    /// Token usage reported for one model call.
    Usage {
        agent: String,
        task: Id,
        input_tokens: u32,
        output_tokens: u32,
    },
    /// A run finished successfully.
    RunCompleted {
        agent: String,
        task: Id,
        /// Final assistant text, if any.
        response: Option<String>,
        input_tokens: u32,
        output_tokens: u32,
    },
    /// A run failed.
    RunFailed {
        agent: String,
        task: Id,
        error: String,
        error_class: String,
    },
    /// A run was cancelled by the user.
    RunCancelled { agent: String, task: Id },
    /// A free-form system notice (config changes, session events…).
    Notice { text: String, level: NoticeLevel },
}

/// Severity for `Event::Notice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NoticeLevel {
    /// Neutral information.
    #[default]
    Info,
    /// Something succeeded.
    Success,
    /// Worth attention.
    Warning,
    /// Something failed.
    Error,
}

impl Event {
    /// The timestamp when this event was created.
    pub fn stamped(self) -> StampedEvent {
        StampedEvent {
            event: self,
            at: Timestamp::now(),
        }
    }

    /// A short single-line summary for logs and the event ticker.
    pub fn summary(&self) -> String {
        match self {
            Event::RunStarted {
                agent, description, ..
            } => {
                format!("{agent} → started: {}", truncate_line(description, 60))
            }
            Event::TurnStarted { agent, turn } => format!("{agent} turn {turn}"),
            Event::AssistantDelta { .. } => String::new(),
            Event::AssistantMessage { agent, text } => {
                format!("{agent} → {}", truncate_line(text, 72))
            }
            Event::ToolCallStarted {
                agent,
                tool,
                summary,
            } => {
                format!("{agent} → {tool}({})", truncate_line(summary, 48))
            }
            Event::ToolCallFinished {
                agent,
                tool,
                ok,
                summary,
                duration_ms,
            } => {
                let mark = if *ok { "✓" } else { "✗" };
                format!(
                    "{agent} {mark} {tool} ({}ms) {}",
                    duration_ms,
                    truncate_line(summary, 40)
                )
            }
            Event::AgentSpawned {
                parent,
                child,
                role,
                ..
            } => {
                format!("{parent} spawned {child} [{role}]")
            }
            Event::AgentStatus { agent, status } => format!("{agent} → {status}"),
            Event::Usage {
                agent,
                input_tokens,
                output_tokens,
                ..
            } => {
                format!("{agent} usage: {input_tokens} in / {output_tokens} out")
            }
            Event::RunCompleted {
                agent,
                input_tokens,
                output_tokens,
                ..
            } => {
                format!("{agent} completed ({input_tokens} in / {output_tokens} out)")
            }
            Event::RunFailed { agent, error, .. } => format!("{agent} failed: {error}"),
            Event::RunCancelled { agent, .. } => format!("{agent} cancelled"),
            Event::Notice { text, .. } => text.clone(),
        }
    }
}

/// An event with the time it was emitted.
#[derive(Debug, Clone)]
pub struct StampedEvent {
    pub event: Event,
    pub at: Timestamp,
}

fn truncate_line(s: &str, max: usize) -> String {
    let s = s.lines().next().unwrap_or("");
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_single_line() {
        let e = Event::RunStarted {
            agent: "master".into(),
            role: "master".into(),
            task: Id::new(),
            description: "fix the bug\nwith two lines".into(),
            mode: "code".into(),
        };
        let s = e.summary();
        assert!(!s.contains('\n'));
        assert!(s.contains("master"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let long = "é".repeat(100);
        let e = Event::AssistantMessage {
            agent: "a".into(),
            text: long,
        };
        let s = e.summary();
        assert!(s.chars().count() <= 80);
    }

    #[test]
    fn events_serialize() {
        let e = Event::ToolCallFinished {
            agent: "a".into(),
            tool: "read_file".into(),
            ok: true,
            summary: "ok".into(),
            duration_ms: 12,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"tool_call_finished\""));
    }
}
