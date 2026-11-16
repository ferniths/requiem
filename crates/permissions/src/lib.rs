//! REQUIEM permissions crate.
//!
//! Enforces the `PermissionProfile` that a `Mode` grants. Before any
//! tool call is executed, the permission gate checks whether the current
//! mode allows that operation class.

use requiem_core::mode::{
    AgentSpawn, FileWrite, GitMutation, Mode, PermissionProfile, TerminalExec, VaultWrite,
};

/// The result of a permission check.
pub enum Decision {
    /// The operation is allowed.
    Allow,
    /// The operation is denied by the current mode.
    Deny { reason: String },
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}

/// A gate that consults the current `PermissionProfile` before an
/// operation proceeds.
pub struct PermissionGate {
    profile: PermissionProfile,
}

impl PermissionGate {
    /// Build a gate for a specific mode.
    pub fn for_mode(mode: Mode) -> Self {
        Self {
            profile: mode.permission_profile(),
        }
    }

    /// Build a gate from an explicit profile (for testing or custom modes).
    pub fn for_profile(profile: PermissionProfile) -> Self {
        Self { profile }
    }

    /// Check whether writing a project file is allowed.
    pub fn can_write_file(&self) -> Decision {
        match self.profile.file_write {
            FileWrite::Denied => Decision::Deny {
                reason: "file writes are denied in the current mode".to_string(),
            },
            FileWrite::PlanOnly => Decision::Deny {
                reason: "only plan-sandbox writes are allowed in Plan mode".to_string(),
            },
            FileWrite::Approved | FileWrite::Full => Decision::Allow,
        }
    }

    /// Check whether a terminal command may run.
    pub fn can_run_terminal(&self, is_mutation: bool) -> Decision {
        match self.profile.terminal {
            TerminalExec::ReadOnly => {
                if is_mutation {
                    Decision::Deny {
                        reason: "mutating terminal commands are denied in Read-Only mode"
                            .to_string(),
                    }
                } else {
                    Decision::Allow
                }
            }
            TerminalExec::Approved | TerminalExec::Full => Decision::Allow,
        }
    }

    /// Check whether a git mutation is allowed.
    pub fn can_mutate_git(&self) -> Decision {
        match self.profile.git_mutation {
            GitMutation::Denied => Decision::Deny {
                reason: "git mutations are denied in the current mode".to_string(),
            },
            GitMutation::StatusOnly => Decision::Deny {
                reason: "only read-only git status is allowed in Status-Only mode".to_string(),
            },
            GitMutation::Approved | GitMutation::Full => Decision::Allow,
        }
    }

    /// Check whether a vault (Obsidian) write is allowed.
    pub fn can_write_vault(&self) -> Decision {
        match self.profile.vault_write {
            VaultWrite::Denied => Decision::Deny {
                reason: "vault writes are denied in the current mode".to_string(),
            },
            VaultWrite::Read | VaultWrite::Write => Decision::Allow,
        }
    }

    /// Check whether an agent may be spawned.
    pub fn can_spawn_agent(&self) -> Decision {
        match self.profile.agent_spawn {
            AgentSpawn::Denied => Decision::Deny {
                reason: "agent spawning is denied in the current mode".to_string(),
            },
            AgentSpawn::Read | AgentSpawn::Approved | AgentSpawn::Full => Decision::Allow,
        }
    }

    /// The current profile (for display).
    pub fn profile(&self) -> &PermissionProfile {
        &self.profile
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_mode_denies_writes() {
        let gate = PermissionGate::for_mode(Mode::Ask);
        assert!(!gate.can_write_file().is_allowed());
        assert!(!gate.can_mutate_git().is_allowed());
        assert!(!gate.can_spawn_agent().is_allowed());
        assert!(gate.can_run_terminal(false).is_allowed());
    }

    #[test]
    fn auto_mode_allows_all() {
        let gate = PermissionGate::for_mode(Mode::Auto);
        assert!(gate.can_write_file().is_allowed());
        assert!(gate.can_run_terminal(true).is_allowed());
        assert!(gate.can_mutate_git().is_allowed());
        assert!(gate.can_write_vault().is_allowed());
        assert!(gate.can_spawn_agent().is_allowed());
    }

    #[test]
    fn code_mode_allows_writes_and_git() {
        let gate = PermissionGate::for_mode(Mode::Code);
        assert!(gate.can_write_file().is_allowed());
        assert!(gate.can_mutate_git().is_allowed());
        assert!(gate.can_spawn_agent().is_allowed());
    }

    #[test]
    fn plan_mode_denies_mutation() {
        let gate = PermissionGate::for_mode(Mode::Plan);
        assert!(!gate.can_write_file().is_allowed());
        assert!(!gate.can_run_terminal(true).is_allowed());
        assert!(gate.can_run_terminal(false).is_allowed());
    }
}
