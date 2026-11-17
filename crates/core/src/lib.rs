//! REQUIEM core types, errors, and shared contracts.
//!
//! This crate defines the foundational data structures and interfaces used
//! across the entire REQUIEM workspace: tasks, task graphs, agent
//! descriptors, messages, operating modes, permission levels, and token
//! accounting. Every other crate builds on these contracts.

pub mod agent;
pub mod error;
pub mod events;
pub mod message;
pub mod mode;
pub mod task;
pub mod token;

use serde::{Deserialize, Serialize};

/// A globally unique identifier for tasks, agents, messages, and sessions.
///
/// Stored as a `String` (UUID v4) so it serializes cleanly to JSON and
/// is human-inspectable in logs and session files.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(String);

impl Id {
    /// Create a new random identifier.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Create an identifier from an existing string (for deserialization or
    /// cross-reference).
    pub fn from(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A wall-clock timestamp in UTC.
///
/// Uses `chrono`'s DateTime<Utc> which serializes to RFC 3339.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(chrono::DateTime<chrono::Utc>);

impl Timestamp {
    pub fn now() -> Self {
        Self(chrono::Utc::now())
    }

    pub fn as_datetime(&self) -> &chrono::DateTime<chrono::Utc> {
        &self.0
    }

    /// Milliseconds since Unix epoch (useful for compact session storage).
    pub fn as_millis(&self) -> u64 {
        self.0.timestamp_millis() as u64
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::now()
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_rfc3339())
    }
}

/// The role of an agent within a task hierarchy.
///
/// Roles are dynamic — the master selects them based on task requirements.
/// They do not dictate which model is used (all agents share one configured
/// model); they only shape the system prompt and tool access expectations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The top-level coordinator for a user task.
    #[default]
    Master,
    /// Investigates files, docs, and sources; produces structured findings.
    Researcher,
    /// Designs architecture and decomposition before implementation.
    Architect,
    /// Produces a structured plan of subtasks.
    Planner,
    /// Writes or modifies code/files.
    Implementer,
    /// Investigates and fixes failures.
    Debugger,
    /// Writes and runs tests.
    Tester,
    /// Independently inspects results.
    Reviewer,
    /// Security-focused review.
    Security,
    /// Writes documentation and notes.
    Documenter,
    /// Restructures code without changing behavior.
    Refactorer,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Role::Master => "master",
            Role::Researcher => "researcher",
            Role::Architect => "architect",
            Role::Planner => "planner",
            Role::Implementer => "implementer",
            Role::Debugger => "debugger",
            Role::Tester => "tester",
            Role::Reviewer => "reviewer",
            Role::Security => "security",
            Role::Documenter => "documenter",
            Role::Refactorer => "refactorer",
        };
        write!(f, "{s}")
    }
}
