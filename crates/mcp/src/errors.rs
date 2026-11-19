//! MCP-specific error taxonomy.
//!
//! These errors are mapped to [`RequiemError`](reqiem_core::error::RequiemError)
//! at the boundary so that the agent runtime, retry logic and TUI error
//! cards see a single unified error type.

use thiserror::Error;

/// Errors that can occur during MCP server lifecycle or tool execution.
#[derive(Debug, Error)]
pub enum McpError {
    /// The MCP server process failed to start or exited unexpectedly.
    #[error("mcp server '{server}' failed to start: {detail}")]
    Startup {
        /// The server alias.
        server: String,
        /// Why startup failed.
        detail: String,
    },

    /// The MCP server process terminated while in use.
    #[error("mcp server '{server}' exited: {detail}")]
    Terminated {
        /// The server alias.
        server: String,
        /// Exit information.
        detail: String,
    },

    /// The initialize handshake or a subsequent request timed out.
    #[error("mcp request to '{server}' timed out after {timeout_ms}ms")]
    Timeout {
        /// The server alias.
        server: String,
        /// The configured timeout in ms.
        timeout_ms: u64,
    },

    /// An MCP tool call returned a protocol error (e.g. tool not found,
    /// invalid arguments, server-side failure).
    #[error("mcp tool call '{tool}' on '{server}' failed: {detail}")]
    ToolCall {
        /// The namespaced tool identifier (`server__tool`).
        tool: String,
        /// The server alias.
        server: String,
        /// The error detail.
        detail: String,
    },

    /// Authentication or credentials for the MCP server are missing or
    /// invalid.
    #[error("mcp auth failed for '{server}': {detail}")]
    Auth {
        /// The server alias.
        server: String,
        /// The auth error.
        detail: String,
    },

    /// The server's capabilities do not include what the client needs
    /// (e.g. no tools listed when tools were expected).
    #[error("mcp server '{server}' capability mismatch: {detail}")]
    Capability {
        /// The server alias.
        server: String,
        /// What was missing.
        detail: String,
    },

    /// A transport-level I/O error (pipe broken, HTTP 5xx, …).
    #[error("mcp transport error for '{server}': {detail}")]
    Transport {
        /// The server alias.
        server: String,
        /// The I/O detail.
        detail: String,
    },

    /// The configured transport command was not found or not executable.
    #[error("mcp command '{command}' not found or not executable")]
    CommandNotFound {
        /// The command string that was not found.
        command: String,
    },

    /// A configuration error (malformed TOML, missing required field).
    #[error("mcp config error: {0}")]
    Config(String),

    /// The request was cancelled before completion.
    #[error("mcp request cancelled: {0}")]
    Cancelled(String),

    /// Output exceeded the configured size limit.
    #[error("mcp output from '{server}' exceeded the size limit: {detail}")]
    OutputLimit {
        /// The server alias.
        server: String,
        /// The limit message.
        detail: String,
    },
}

impl From<McpError> for requiem_core::error::RequiemError {
    fn from(e: McpError) -> Self {
        match &e {
            McpError::Startup { .. }
            | McpError::Terminated { .. }
            | McpError::Transport { .. } => {
                Self::Connection(e.to_string())
            }
            McpError::Timeout { timeout_ms, .. } => Self::Timeout(*timeout_ms),
            McpError::Auth { .. } => Self::Auth(e.to_string()),
            McpError::ToolCall { tool, detail, .. } => Self::Tool {
                tool: tool.clone(),
                message: detail.clone(),
            },
            McpError::Capability { .. } => Self::Config(e.to_string()),
            McpError::CommandNotFound { .. } => Self::Io(e.to_string()),
            McpError::Config(_) => Self::Config(e.to_string()),
            McpError::Cancelled(_) => Self::Cancelled,
            McpError::OutputLimit { .. } => Self::InvalidResponse(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use requiem_core::error::RequiemError;

    #[test]
    fn startup_maps_to_connection() {
        let e = McpError::Startup {
            server: "test".into(),
            detail: "binary not found".into(),
        };
        assert!(matches!(RequiemError::from(e), RequiemError::Connection(_)));
    }

    #[test]
    fn timeout_maps_to_timeout() {
        let e = McpError::Timeout {
            server: "test".into(),
            timeout_ms: 5000,
        };
        assert!(matches!(
            RequiemError::from(e),
            RequiemError::Timeout(5000)
        ));
    }

    #[test]
    fn tool_call_maps_to_tool() {
        let e = McpError::ToolCall {
            tool: "ghidra__decompile".into(),
            server: "ghidra".into(),
            detail: "function not found".into(),
        };
        assert!(matches!(
            RequiemError::from(e),
            RequiemError::Tool { tool, .. } if tool == "ghidra__decompile"
        ));
    }

    #[test]
    fn auth_maps_to_auth() {
        let e = McpError::Auth {
            server: "test".into(),
            detail: "401".into(),
        };
        assert!(matches!(RequiemError::from(e), RequiemError::Auth(_)));
    }

    #[test]
    fn cancelled_maps_to_cancelled() {
        let e = McpError::Cancelled("user stopped".into());
        assert!(matches!(RequiemError::from(e), RequiemError::Cancelled));
    }

    #[test]
    fn command_not_found_maps_to_io() {
        let e = McpError::CommandNotFound {
            command: "npx".into(),
        };
        assert!(matches!(RequiemError::from(e), RequiemError::Io(_)));
    }
}
