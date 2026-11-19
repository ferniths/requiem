//! Streamable HTTP transport.
//!
//! Connects to a remote MCP server that exposes a Streamable HTTP
//! endpoint (MCP 2025-03-26 spec). Uses the official
//! `rmcp::transport::StreamableHttpClientTransport` which handles the
//! JSON-RPC-over-HTTP framing, session management, and reconnection.
//!
//! ## Security
//!
//! * TLS certificate verification is NEVER disabled.
//! * Bearer tokens and custom headers are resolved from environment
//!   variable references before the transport is constructed.

use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

use crate::config::McpServerConfig;
use crate::errors::McpError;

/// Build a `StreamableHttpClientTransport` for an HTTP MCP server.
///
/// The `auth` field and any `headers` in the server config are resolved
/// (env-var references substituted) before the transport is constructed.
pub fn build_http_transport(
    server: &McpServerConfig,
) -> Result<StreamableHttpClientTransport<reqwest::Client>, McpError> {
    let url = server
        .http_url()
        .ok_or_else(|| McpError::Config(format!("server '{}' is not an HTTP transport", server.name)))?;

    let resolved = server.resolved();

    let config = StreamableHttpClientTransportConfig::with_uri(url)
        .max_concurrent_requests(resolved.max_concurrency as usize);

    // Set auth header if configured (env-ref resolved).
    let config = if let crate::config::McpTransport::Http { auth, headers, .. } = &resolved.transport {
        if !auth.is_empty() {
            let c = config.auth_header(auth.as_str());
            if !headers.is_empty() {
                tracing::warn!(
                    server = %server.name,
                    count = headers.len(),
                    "custom HTTP headers are logged but not yet applied; \
                     bearer auth is the supported mechanism"
                );
            }
            c
        } else {
            if !headers.is_empty() {
                tracing::warn!(
                    server = %server.name,
                    count = headers.len(),
                    "custom HTTP headers are logged but not yet applied; \
                     bearer auth is the supported mechanism"
                );
            }
            config
        }
    } else {
        config
    };

    // The reqwest feature provides a `StreamableHttpClientTransport`
    // type alias that is `WorkerTransport<StreamableHttpClientWorker<reqwest::Client>>`.
    let client = reqwest::Client::new();
    let transport = StreamableHttpClientTransport::with_client(client, config);
    Ok(transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpTransport;

    fn test_server() -> McpServerConfig {
        McpServerConfig {
            name: "test".into(),
            description: String::new(),
            transport: McpTransport::Http {
                url: "http://localhost:8080/mcp".into(),
                auth: String::new(),
                headers: vec![],
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
    fn http_url_extraction() {
        let server = test_server();
        assert_eq!(server.http_url(), Some("http://localhost:8080/mcp".into()));
    }

    #[test]
    fn stdio_server_has_no_http_url() {
        let mut server = test_server();
        server.transport = McpTransport::Stdio {
            command: "npx".into(),
            args: vec!["mcp-server-test".into()],
        };
        assert!(server.http_url().is_none());
    }
}
