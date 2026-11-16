//! Structured coordination messages.
//!
//! REQUIEM deliberately does *not* make agents exchange raw conversational
//! histories. Instead, agents publish compact structured messages through a
//! central event bus. The master is always informed of significant events;
//! peers may subscribe to specific task streams.

use serde::{Deserialize, Serialize};

use crate::agent::{AgentId, AgentStatus};
use crate::{Id, Role, Timestamp};

/// A structured event in the coordination stream.
///
/// Every message carries the sender, a timestamp, and an optional task
/// reference so receivers can correlate messages to work items.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    /// An agent reported progress on a task (no completion yet).
    Progress {
        /// The task being worked on.
        task: Id,
        /// Free-text progress note.
        note: String,
    },
    /// An agent completed a task with a structured result.
    TaskCompleted {
        /// The task.
        task: Id,
        /// The result payload.
        result: serde_json::Value,
    },
    /// An agent failed a task.
    TaskFailed {
        /// The task.
        task: Id,
        /// Why it failed.
        reason: String,
    },
    /// An agent requests help from the master or a peer.
    AssistanceRequest {
        /// The task needing help.
        task: Id,
        /// What the agent needs.
        request: String,
    },
    /// An agent publishes a finding other agents may reuse (dedup signal).
    Finding {
        /// A short topic key (e.g. "auth-middleware").
        topic: String,
        /// The finding body.
        body: String,
        /// Optional file references the finding concerns.
        #[serde(default)]
        files: Vec<String>,
    },
    /// An agent spawned a subagent.
    AgentSpawned {
        /// The new agent.
        agent: AgentId,
        /// The role it was created for.
        role: Role,
        /// Its parent.
        parent: AgentId,
    },
    /// An agent's status changed.
    AgentStatusChanged {
        /// The agent.
        agent: AgentId,
        /// New status.
        status: AgentStatus,
    },
    /// A verification pass on a task succeeded.
    Verified {
        /// The task.
        task: Id,
    },
    /// A verification pass on a task failed.
    VerificationFailed {
        /// The task.
        task: Id,
        /// What the reviewer found.
        issues: String,
    },
    /// A user instruction arrived mid-run (e.g. "stop", "focus on X").
    UserInstruction {
        /// The instruction text.
        text: String,
    },
    /// The master is shutting down the session gracefully.
    Shutdown,
}

/// A message envelope: the payload plus routing metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// The message body.
    pub message: Message,
    /// Which agent sent it.
    pub sender: AgentId,
    /// When it was sent.
    pub at: Timestamp,
    /// The task session it belongs to.
    pub task_session: Id,
}

impl Envelope {
    /// Build an envelope for a message.
    pub fn new(message: Message, sender: AgentId, task_session: Id) -> Self {
        Self {
            message,
            sender,
            at: Timestamp::now(),
            task_session,
        }
    }
}
