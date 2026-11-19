//! Lifecycle convenience helpers.
//!
//! These functions wrap the connect / disconnect / reconnect flows
//! that the [`McpManager`](super::manager::McpManager) and the `/mcp`
//! slash commands use.

use crate::config::McpServerConfig;
use crate::server::ServerState;

/// The status of a server connection attempt, used for TUI reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStatus {
    /// The server is connected and operational.
    Connected,
    /// The server is up but degraded.
    Degraded,
    /// The server failed to connect or exited.
    Failed,
    /// The server is not enabled.
    Disabled,
    /// The server is not yet connected.
    Disconnected,
}

impl std::fmt::Display for McpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Connected => "connected",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
            Self::Disconnected => "disconnected",
        };
        write!(f, "{s}")
    }
}

/// Map a `ServerState` to a `McpStatus` for TUI reporting.
pub fn status_from_state(state: ServerState) -> McpStatus {
    match state {
        ServerState::Connected => McpStatus::Connected,
        ServerState::Degraded => McpStatus::Degraded,
        ServerState::Failed => McpStatus::Failed,
        ServerState::Disabled => McpStatus::Disabled,
        _ => McpStatus::Disconnected,
    }
}

/// A snapshot of a server's lifecycle state, suitable for TUI display.
#[derive(Debug, Clone)]
pub struct McpLifecycle {
    /// The server alias.
    pub name: String,
    /// The current status.
    pub status: McpStatus,
    /// The transport type (human-readable).
    pub transport: String,
    /// Number of tools discovered.
    pub tool_count: usize,
    /// Number of resources discovered.
    pub resource_count: usize,
    /// Number of prompts discovered.
    pub prompt_count: usize,
    /// Last latency in ms.
    pub last_latency_ms: Option<u64>,
    /// Last error, if any.
    pub last_error: Option<String>,
}

impl McpLifecycle {
    /// Build a lifecycle snapshot from a server config and state.
    pub fn new(
        config: &McpServerConfig,
        state: ServerState,
        tool_count: usize,
        resource_count: usize,
        prompt_count: usize,
        last_latency_ms: Option<u64>,
        last_error: Option<String>,
    ) -> Self {
        let transport = match &config.transport {
            crate::config::McpTransport::Stdio { command, args } => {
                let args_str: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                if args_str.is_empty() {
                    format!("stdio: {command}")
                } else {
                    format!("stdio: {command} {}", args_str.join(" "))
                }
            }
            crate::config::McpTransport::Http { url, .. } => {
                format!("http: {url}")
            }
        };

        Self {
            name: config.name.clone(),
            status: status_from_state(state),
            transport,
            tool_count,
            resource_count,
            prompt_count,
            last_latency_ms,
            last_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_from_state_mapping() {
        assert_eq!(status_from_state(ServerState::Connected), McpStatus::Connected);
        assert_eq!(status_from_state(ServerState::Failed), McpStatus::Failed);
        assert_eq!(status_from_state(ServerState::Disconnected), McpStatus::Disconnected);
    }

    #[test]
    fn lifecycle_new_stdio() {
        let config = crate::config::McpServerConfig {
            name: "ghidra".into(),
            description: String::new(),
            transport: crate::config::McpTransport::Stdio {
                command: "python".into(),
                args: vec!["-m".into(), "mcp_server".into()],
            },
            enabled: true,
            auto_connect: true,
            request_timeout_ms: 120_000,
            max_concurrency: 4,
            env: vec![],
            working_dir: String::new(),
            tool_policies: vec![],
            default_tool_policy: "auto".into(),
            tool_allow: vec![],
            tool_deny: vec![],
            trusted: true,
        };
        let lc = McpLifecycle::new(
            &config,
            ServerState::Connected,
            12,
            3,
            5,
            Some(42),
            None,
        );
        assert_eq!(lc.name, "ghidra");
        assert_eq!(lc.status, McpStatus::Connected);
        assert!(lc.transport.contains("python"));
        assert_eq!(lc.tool_count, 12);
    }

    #[test]
    fn status_display() {
        assert_eq!(McpStatus::Connected.to_string(), "connected");
        assert_eq!(McpStatus::Failed.to_string(), "failed");
    }
}
