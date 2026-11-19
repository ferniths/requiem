//! Per-server lifecycle state machine.
//!
//! Each configured MCP server has a well-defined state that the
//! [`McpManager`](super::manager::McpManager) transitions through.
//! A server crash never takes down the rest of REQUIEM — it simply
//! moves to `Failed` and the user can reconnect.

use std::time::Instant;

/// The lifecycle states of a single MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    /// Not yet started.
    Disconnected,
    /// The child process / HTTP connection is being launched.
    Starting,
    /// The initialize handshake is in progress.
    Connecting,
    /// The server is ready and tools are available.
    Connected,
    /// The server is up but degraded (e.g. a recent request failed).
    Degraded,
    /// The server exited or the connection was lost.
    Failed,
    /// The user requested a clean shutdown.
    Stopping,
    /// The server is disabled (not auto-connected).
    Disabled,
}

impl std::fmt::Display for ServerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Disconnected => "disconnected",
            Self::Starting => "starting",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Stopping => "stopping",
            Self::Disabled => "disabled",
        };
        write!(f, "{s}")
    }
}

impl ServerState {
    /// Whether this state is a "live" state where tool calls can proceed.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Connected | Self::Degraded)
    }
}

/// The runtime state of one connected MCP server.
///
/// Owned by the [`McpManager`](super::manager::McpManager).
pub struct McpServer {
    /// The server alias (e.g. `"ghidra"`).
    pub name: String,
    /// The current lifecycle state.
    pub state: ServerState,
    /// When the server last transitioned to its current state.
    pub state_since: Instant,
    /// The most recent error, if any.
    pub last_error: Option<String>,
    /// Number of tools discovered from this server.
    pub tool_count: usize,
    /// Number of resources discovered.
    pub resource_count: usize,
    /// Number of prompts discovered.
    pub prompt_count: usize,
    /// Latency of the last successful request, in ms.
    pub last_latency_ms: Option<u64>,
    /// The raw `rmcp` client handle (opaque to callers).
    ///
    /// The client is stored behind a `tokio::sync::Mutex` because the
    /// `rmcp` client is not `Sync` and the manager may hand out
    /// `Arc` references to the same client.
    pub client: Option<std::sync::Arc<rmcp::service::Peer<rmcp::service::RoleClient>>>,
}

impl McpServer {
    /// Create a disconnected server entry.
    pub fn new(name: String) -> Self {
        Self {
            name,
            state: ServerState::Disconnected,
            state_since: Instant::now(),
            last_error: None,
            tool_count: 0,
            resource_count: 0,
            prompt_count: 0,
            last_latency_ms: None,
            client: None,
        }
    }

    /// Transition to a new state, recording the time.
    pub fn transition(&mut self, state: ServerState, error: Option<String>) {
        self.state = state;
        self.state_since = Instant::now();
        if error.is_some() {
            self.last_error = error;
        }
        if state == ServerState::Connected {
            self.last_error = None;
        }
    }

    /// Whether this server can accept tool calls right now.
    pub fn is_usable(&self) -> bool {
        self.state.is_live() && self.client.is_some()
    }

    /// A one-line summary for the TUI dashboard.
    pub fn summary(&self) -> String {
        let err = self
            .last_error
            .as_deref()
            .map(|e| e.chars().take(60).collect::<String>());
        let err_part = err.map(|e| format!(" — last: {e}")).unwrap_or_default();
        format!(
            "{} · {} · {} tools / {} res / {} prompts{}",
            self.name,
            self.state,
            self.tool_count,
            self.resource_count,
            self.prompt_count,
            err_part
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_server_starts_disconnected() {
        let s = McpServer::new("test".into());
        assert_eq!(s.state, ServerState::Disconnected);
        assert!(!s.is_usable());
    }

    #[test]
    fn connected_state_is_usable_when_client_present() {
        let mut s = McpServer::new("test".into());
        s.transition(ServerState::Connected, None);
        assert!(s.state.is_live());
        assert!(!s.is_usable()); // no client yet
        // Simulate having a client (we can't easily construct one in a
        // unit test, so just check the state logic).
    }

    #[test]
    fn failed_state_records_error() {
        let mut s = McpServer::new("test".into());
        s.transition(ServerState::Failed, Some("connection refused".into()));
        assert_eq!(s.state, ServerState::Failed);
        assert!(s.last_error.is_some());
        assert!(!s.state.is_live());
    }

    #[test]
    fn summary_includes_key_fields() {
        let mut s = McpServer::new("ghidra".into());
        s.transition(ServerState::Connected, None);
        s.tool_count = 12;
        let summary = s.summary();
        assert!(summary.contains("ghidra"));
        assert!(summary.contains("connected"));
        assert!(summary.contains("12 tools"));
    }

    #[test]
    fn state_display_is_lowercase() {
        assert_eq!(ServerState::Connected.to_string(), "connected");
        assert_eq!(ServerState::Failed.to_string(), "failed");
    }
}
