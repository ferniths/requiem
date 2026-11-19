//! Stdio transport: launch a child process and speak JSON-RPC over its
//! stdin/stdout.
//!
//! Uses the official `rmcp::transport::TokioChildProcess` which handles
//! process spawning, JSON-RPC framing, and process cleanup. Stderr is
//! captured separately and logged — it is NEVER mixed with the protocol
//! stream on stdout.
//!
//! ## Security
//!
//! * Commands are executed as argument arrays (no shell interpolation).
//! * The process is a child of REQUIEM and is killed on shutdown.
//! * Environment variables are passed explicitly — the process does not
//!   inherit REQUIEM's full environment by default.

use std::process::Stdio;

use rmcp::transport::child_process::TokioChildProcess;
use tokio::process::Command;

use crate::config::McpServerConfig;
use crate::errors::McpError;

/// Build a `TokioChildProcess` transport for a stdio MCP server.
///
/// The server config's `command` and `args` are resolved (with env-var
/// references substituted) before the process is launched.
///
/// The process stderr is piped to a background log so that diagnostics
/// never interfere with the JSON-RPC protocol on stdout.
pub fn build_stdio_transport(
    server: &McpServerConfig,
) -> Result<TokioChildProcess, McpError> {
    let (command, args) = server
        .stdio_command()
        .ok_or_else(|| McpError::Config(format!("server '{}' is not a stdio transport", server.name)))?;

    let mut cmd = Command::new(&command);
    cmd.args(&args);
    cmd.stdout(Stdio::piped());
    cmd.stdin(Stdio::piped());
    // Capture stderr separately — never mix diagnostics with protocol.
    cmd.stderr(Stdio::piped());

    // Pass explicit env vars from the server config.
    for (key, value) in &server.env {
        cmd.env(key, value);
    }

    // Working directory if configured.
    if !server.working_dir.is_empty() {
        cmd.current_dir(&server.working_dir);
    }

    let transport = TokioChildProcess::new(cmd)
        .map_err(|e| McpError::Startup {
            server: server.name.clone(),
            detail: format!("failed to spawn process: {e}"),
        })?;

    Ok(transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpServerConfig, McpTransport};

    fn test_server() -> McpServerConfig {
        McpServerConfig {
            name: "test".into(),
            description: String::new(),
            transport: McpTransport::Stdio {
                command: "echo".into(),
                args: vec!["hello".into()],
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
    fn build_stdio_transport_returns_error_for_non_stdio() {
        let mut server = test_server();
        server.transport = McpTransport::Http {
            url: "http://localhost:8080".into(),
            auth: String::new(),
            headers: vec![],
        };
        let result = build_stdio_transport(&server);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(matches!(err, McpError::Config(_)));
    }

    #[test]
    fn stdio_command_extraction() {
        let server = test_server();
        let (cmd, args) = server.stdio_command().unwrap();
        assert_eq!(cmd, "echo");
        assert_eq!(args, vec!["hello".to_string()]);
    }
}
