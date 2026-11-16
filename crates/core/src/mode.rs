//! Operating modes and permission profiles.
//!
//! A `Mode` is the user-visible reason the harness reasons/plans/executes.
//! Each mode carries a default `PermissionProfile` that the permission
//! system enforces. Users can switch modes with slash commands; the profile
//! is recomputed on every switch.

use serde::{Deserialize, Serialize};

/// The operating modes, mirroring the product spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Answer and inspect; no file mutation, no destructive ops.
    #[default]
    Ask,
    /// Investigate and produce a plan; no implementation until user approves.
    Plan,
    /// Implement requested changes; multi-agent delegation; runs builds/tests.
    Code,
    /// Fully autonomous: plan, delegate, implement, test, review, correct.
    Auto,
    /// Investigate errors, reproduce, find root cause, fix and verify.
    Debug,
    /// Parallel research; produce structured findings; save to Obsidian.
    Research,
}

impl Mode {
    /// The default permission profile for this mode.
    pub fn permission_profile(self) -> PermissionProfile {
        match self {
            Mode::Ask => PermissionProfile {
                file_write: FileWrite::Denied,
                terminal: TerminalExec::ReadOnly,
                git_mutation: GitMutation::Denied,
                vault_write: VaultWrite::Denied,
                agent_spawn: AgentSpawn::Denied,
            },
            Mode::Plan => PermissionProfile {
                file_write: FileWrite::PlanOnly,
                terminal: TerminalExec::ReadOnly,
                git_mutation: GitMutation::StatusOnly,
                vault_write: VaultWrite::Read,
                agent_spawn: AgentSpawn::Read,
            },
            Mode::Code => PermissionProfile {
                file_write: FileWrite::Approved,
                terminal: TerminalExec::Approved,
                git_mutation: GitMutation::Approved,
                vault_write: VaultWrite::Read,
                agent_spawn: AgentSpawn::Approved,
            },
            Mode::Auto => PermissionProfile {
                file_write: FileWrite::Full,
                terminal: TerminalExec::Full,
                git_mutation: GitMutation::Full,
                vault_write: VaultWrite::Write,
                agent_spawn: AgentSpawn::Full,
            },
            Mode::Debug => PermissionProfile {
                file_write: FileWrite::Approved,
                terminal: TerminalExec::Approved,
                git_mutation: GitMutation::Approved,
                vault_write: VaultWrite::Read,
                agent_spawn: AgentSpawn::Approved,
            },
            Mode::Research => PermissionProfile {
                file_write: FileWrite::Denied,
                terminal: TerminalExec::ReadOnly,
                git_mutation: GitMutation::StatusOnly,
                vault_write: VaultWrite::Write,
                agent_spawn: AgentSpawn::Approved,
            },
        }
    }

    /// All modes, for menu rendering.
    pub const fn all() -> [Mode; 6] {
        [
            Mode::Ask,
            Mode::Plan,
            Mode::Code,
            Mode::Auto,
            Mode::Debug,
            Mode::Research,
        ]
    }

    /// The slash command that selects this mode.
    pub fn slash(self) -> &'static str {
        match self {
            Mode::Ask => "/ask",
            Mode::Plan => "/plan",
            Mode::Code => "/code",
            Mode::Auto => "/auto",
            Mode::Debug => "/debug",
            Mode::Research => "/research",
        }
    }

    /// A one-line description of what the mode does.
    pub fn description(self) -> &'static str {
        match self {
            Mode::Ask => "answer questions and inspect context; no mutations",
            Mode::Plan => "investigate and produce a plan; no implementation",
            Mode::Code => "implement changes, delegate, run builds and tests",
            Mode::Auto => "fully autonomous until the objective is met",
            Mode::Debug => "reproduce, root-cause, fix, and verify failures",
            Mode::Research => "parallel research and structured findings",
        }
    }

    /// Parse from a slash command string.
    pub fn from_slash(s: &str) -> Option<Mode> {
        match s {
            "/ask" => Some(Mode::Ask),
            "/plan" => Some(Mode::Plan),
            "/code" => Some(Mode::Code),
            "/auto" => Some(Mode::Auto),
            "/debug" => Some(Mode::Debug),
            "/research" => Some(Mode::Research),
            _ => None,
        }
    }
}

/// How much file writing a mode allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FileWrite {
    /// No file writes at all.
    #[default]
    Denied,
    /// Writes allowed only inside the plan-sandbox (e.g. a scratch dir).
    PlanOnly,
    /// Writes to project files allowed but each mutation is logged.
    Approved,
    /// Unrestricted writes within the working tree.
    Full,
}

/// How much terminal execution a mode allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TerminalExec {
    /// Only non-mutating commands (read, inspect).
    #[default]
    ReadOnly,
    /// Approved commands (build, test) may run.
    Approved,
    /// Any command in the allow-list may run.
    Full,
}

/// How much git mutation a mode allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GitMutation {
    /// No git writes.
    #[default]
    Denied,
    /// Only read-only status/log/diff.
    StatusOnly,
    /// add/commit allowed.
    Approved,
    /// add/commit/push allowed.
    Full,
}

/// How much vault (Obsidian) writing a mode allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VaultWrite {
    /// No vault writes.
    #[default]
    Denied,
    /// Read vaults only.
    Read,
    /// Create/update notes with logging.
    Write,
}

/// How much agent spawning a mode allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentSpawn {
    /// No agent spawning.
    #[default]
    Denied,
    /// Spawn read-only agents.
    Read,
    /// Spawn approved agents.
    Approved,
    /// Spawn fully-autonomous agents.
    Full,
}

/// The full permission profile a mode grants.
///
/// This is the single object the permission system consults. It is
/// deliberately a value type so it can be copied, compared, and persisted
/// with a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PermissionProfile {
    pub file_write: FileWrite,
    pub terminal: TerminalExec,
    pub git_mutation: GitMutation,
    pub vault_write: VaultWrite,
    pub agent_spawn: AgentSpawn,
}
