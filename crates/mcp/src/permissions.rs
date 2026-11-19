//! Per-server and per-tool permission checks for MCP.
//!
//! MCP tool calls are subject to REQUIEM's existing
//! [`PermissionGate`](reqiem_permissions::PermissionGate). In
//! addition, each server config can declare per-tool policies
//! (`"auto"`, `"approve"`, `"deny"`) that are checked before the
//! gate.

use crate::config::McpServerConfig;
use crate::errors::McpError;

/// The result of a permission check for an MCP tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpPermission {
    /// The call may proceed without user confirmation.
    Allow,
    /// The call requires explicit user approval before proceeding.
    Approve,
    /// The call is denied by policy.
    Deny,
}

/// The permission policy for one server's tool calls.
pub struct McpPermissionPolicy {
    server_config: McpServerConfig,
}

impl McpPermissionPolicy {
    /// Create a policy from a server config.
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            server_config: config,
        }
    }

    /// Check whether a tool call is permitted.
    ///
    /// The check order is:
    /// 1. Is the tool in the deny list? → `Deny`
    /// 2. Is the tool in the allow list (if non-empty)? If not → `Deny`
    /// 3. What is the per-tool policy? → `Allow` / `Approve` / `Deny`
    pub fn check(&self, tool_name: &str) -> McpPermission {
        if !self.server_config.tool_registered(tool_name) {
            return McpPermission::Deny;
        }
        match self.server_config.tool_policy(tool_name) {
            "deny" => McpPermission::Deny,
            "approve" => McpPermission::Approve,
            _ => McpPermission::Allow,
        }
    }

    /// The server alias.
    pub fn server_name(&self) -> &str {
        &self.server_config.name
    }
}

/// Validate that a tool call is permitted, returning a descriptive
/// error if not.
pub fn enforce_permission(
    policy: &McpPermissionPolicy,
    tool_name: &str,
) -> Result<(), McpError> {
    match policy.check(tool_name) {
        McpPermission::Allow => Ok(()),
        McpPermission::Approve => Err(McpError::ToolCall {
            tool: format!("{}__{tool_name}", policy.server_name()),
            server: policy.server_name().to_string(),
            detail: "tool requires explicit user approval (policy: approve)".to_string(),
        }),
        McpPermission::Deny => Err(McpError::ToolCall {
            tool: format!("{}__{tool_name}", policy.server_name()),
            server: policy.server_name().to_string(),
            detail: "tool is denied by server policy".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpTransport;

    fn test_server() -> McpServerConfig {
        McpServerConfig {
            name: "test".into(),
            description: String::new(),
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec![],
            },
            enabled: true,
            auto_connect: false,
            request_timeout_ms: 120_000,
            max_concurrency: 4,
            env: vec![],
            working_dir: String::new(),
            tool_policies: vec![],
            default_tool_policy: "auto".into(),
            tool_allow: vec![],
            tool_deny: vec![],
            trusted: false,
        }
    }

    #[test]
    fn default_policy_allows() {
        let policy = McpPermissionPolicy::new(test_server());
        assert_eq!(policy.check("any_tool"), McpPermission::Allow);
    }

    #[test]
    fn deny_policy_blocks() {
        let mut server = test_server();
        server.tool_deny = vec!["dangerous".into()];
        let policy = McpPermissionPolicy::new(server);
        assert_eq!(policy.check("dangerous"), McpPermission::Deny);
        assert_eq!(policy.check("safe"), McpPermission::Allow);
    }

    #[test]
    fn per_tool_approve_policy() {
        let mut server = test_server();
        server.tool_policies = vec![crate::config::ToolPolicyEntry {
            tool: "write_file".into(),
            policy: "approve".into(),
        }];
        let policy = McpPermissionPolicy::new(server);
        assert_eq!(policy.check("write_file"), McpPermission::Approve);
        assert_eq!(policy.check("other_tool"), McpPermission::Allow);
    }

    #[test]
    fn enforce_permission_rejects_denied() {
        let mut server = test_server();
        server.tool_deny = vec!["blocked".into()];
        let policy = McpPermissionPolicy::new(server);
        let result = enforce_permission(&policy, "blocked");
        assert!(result.is_err());
    }

    #[test]
    fn enforce_permission_allows_auto() {
        let server = test_server();
        let policy = McpPermissionPolicy::new(server);
        let result = enforce_permission(&policy, "any_tool");
        assert!(result.is_ok());
    }
}
