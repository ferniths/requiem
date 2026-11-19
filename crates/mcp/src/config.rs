//! MCP server and transport configuration.
//!
//! This module defines the TOML-deserializable types that live under the
//! `[mcp]` section of `~/.requiem/config.toml`. Each named server
//! declares how to reach it (stdio command or HTTP URL), what auth to
//! use, per-server timeouts, and per-tool permission policies.
//!
//! ## Secret handling
//!
//! Secrets (API keys, bearer tokens, OAuth client secrets) are NEVER
//! stored in the TOML file directly. Instead, the config uses
//! environment-variable references of the form `${VAR_NAME}`, which
//! are resolved at connect time. This keeps the version-controlled
//! config clean and prevents credential leakage.

use serde::{Deserialize, Serialize};

/// The root MCP configuration block.
///
/// Deserialized from the `[mcp]` section of the REQUIEM config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Named MCP server definitions.
    ///
    /// Each key is a human-readable alias (e.g. `"ghidra"`, `"ida"`,
    /// `"filesystem"`) and each value fully describes how to reach and
    /// authenticate to that server.
    pub servers: Vec<McpServerConfig>,
}

/// A single named MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    /// The alias used in namespaced tool identifiers
    /// (`server__tool_name`).
    pub name: String,

    /// Human-readable description shown in `/mcp list` and the TUI.
    #[serde(default)]
    pub description: String,

    /// The transport to use for this server.
    pub transport: McpTransport,

    /// Whether this server is enabled by default. Disabled servers are
    /// loaded but not auto-connected.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Auto-connect on session start (requires `enabled`).
    #[serde(default)]
    pub auto_connect: bool,

    /// Per-server request timeout in milliseconds.
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,

    /// Maximum number of concurrent in-flight requests to this server.
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,

    /// Environment variables to pass to a stdio child process.
    ///
    /// Values may use the `${VAR}` env-ref syntax.
    #[serde(default)]
    pub env: Vec<(String, String)>,

    /// Working directory for a stdio child process.
    #[serde(default)]
    pub working_dir: String,

    /// Per-tool permission policies. When a tool is not listed here,
    /// the server-level default applies.
    ///
    /// - `"auto"` — no per-call confirmation.
    /// - `"approve"` — ask the user before every call.
    /// - `"deny"` — never allow this tool.
    #[serde(default)]
    pub tool_policies: Vec<ToolPolicyEntry>,

    /// A global default for this server when a tool has no explicit
    /// policy. One of `"auto"`, `"approve"`, or `"deny"`.
    #[serde(default = "default_tool_policy")]
    pub default_tool_policy: String,

    /// Optional allow-list of tool names. When non-empty, ONLY these
    /// tools are registered (all others are filtered out).
    #[serde(default)]
    pub tool_allow: Vec<String>,

    /// Optional deny-list of tool names. Tools in this list are never
    /// registered regardless of `tool_allow`.
    #[serde(default)]
    pub tool_deny: Vec<String>,

    /// Whether the user has explicitly trusted this server. Trusted
    /// servers are launched without the one-time confirmation prompt.
    #[serde(default)]
    pub trusted: bool,
}

/// A per-tool policy entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPolicyEntry {
    /// The tool name (without the `server__` prefix — that is the
    /// namespaced form, and the policy is keyed on the bare name).
    pub tool: String,
    /// The policy: `"auto"`, `"approve"`, or `"deny"`.
    pub policy: String,
}

/// The transport to use for reaching an MCP server.
///
/// Deserialized as either:
/// ```toml
/// [server]
/// transport = { type = "stdio", command = "npx", args = ["mcp-server-git"] }
/// ```
/// or:
/// ```toml
/// [server]
/// transport = { type = "http", url = "http://localhost:8080/mcp", auth = "${GIDRA_API_KEY}" }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpTransport {
    /// Launch a child process and speak JSON-RPC over its stdin/stdout.
    Stdio {
        #[serde(rename = "command")]
        command: String,
        #[serde(default, rename = "args")]
        args: Vec<String>,
    },
    /// Connect to a Streamable HTTP endpoint.
    Http {
        url: String,
        /// Optional bearer token. May use `${VAR}` env-ref.
        #[serde(default)]
        auth: String,
        /// Extra HTTP headers (e.g. `X-Custom-Header: value`).
        #[serde(default)]
        headers: Vec<(String, String)>,
    },
}

fn default_true() -> bool {
    true
}
fn default_request_timeout_ms() -> u64 {
    120_000
}
fn default_concurrency() -> u32 {
    4
}
fn default_tool_policy() -> String {
    "auto".into()
}

/// Resolve a `${VAR}` reference in a string.
///
/// If the entire string is `${VAR}`, the env var value replaces it.
/// Partial references are NOT supported — the whole value must be a
/// single env-ref to avoid silent partial leaks.
pub fn resolve_env_ref(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with("${") && trimmed.ends_with('}') && trimmed.len() >= 4 {
        let var_name = &trimmed[2..trimmed.len() - 1];
        std::env::var(var_name).unwrap_or_default()
    } else {
        trimmed.to_string()
    }
}

impl McpServerConfig {
    /// Resolve all env-var references in the transport and env fields.
    ///
    /// Returns a deep copy with secrets substituted. Used at connect
    /// time; the original config remains clean.
    pub fn resolved(&self) -> Self {
        let mut resolved = self.clone();
        resolved.env = self
            .env
            .iter()
            .map(|(k, v)| (k.clone(), resolve_env_ref(v)))
            .collect();
        match &mut resolved.transport {
            McpTransport::Stdio { .. } => {}
            McpTransport::Http { auth, .. } => *auth = resolve_env_ref(auth),
        }
        resolved
    }

    /// The fully-resolved stdio command and args (after env-ref
    /// substitution).
    pub fn stdio_command(&self) -> Option<(String, Vec<String>)> {
        let McpTransport::Stdio { command, args } = &self.transport else {
            return None;
        };
        Some((command.clone(), args.clone()))
    }

    /// The HTTP URL (after env-ref auth resolution).
    pub fn http_url(&self) -> Option<String> {
        match &self.transport {
            McpTransport::Http { url, .. } => Some(url.clone()),
            _ => None,
        }
    }

    /// Look up the policy for a specific tool by its bare name.
    pub fn tool_policy(&self, tool_name: &str) -> &str {
        self.tool_policies
            .iter()
            .find(|e| e.tool == tool_name)
            .map(|e| e.policy.as_str())
            .unwrap_or(&self.default_tool_policy)
    }

    /// Whether a tool is registered (not in the deny list, and either
    /// in the allow list or the allow list is empty).
    pub fn tool_registered(&self, tool_name: &str) -> bool {
        if self.tool_deny.iter().any(|d| d == tool_name) {
            return false;
        }
        if !self.tool_allow.is_empty() {
            return self.tool_allow.iter().any(|a| a == tool_name);
        }
        true
    }
}

impl McpConfig {
    /// Look up a server by name.
    pub fn find(&self, name: &str) -> Option<&McpServerConfig> {
        self.servers.iter().find(|s| s.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mcp_config_is_empty() {
        let cfg = McpConfig::default();
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn resolve_env_ref_full() {
        std::env::set_var("TEST_MCP_TOKEN", "secret-value");
        assert_eq!(resolve_env_ref("${TEST_MCP_TOKEN}"), "secret-value");
        std::env::remove_var("TEST_MCP_TOKEN");
    }

    #[test]
    fn resolve_env_ref_passthrough() {
        assert_eq!(resolve_env_ref("plain-value"), "plain-value");
    }

    #[test]
    fn tool_registered_default() {
        let server = McpServerConfig {
            name: "test".into(),
            description: String::new(),
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec!["mcp-server-test".into()],
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
        };
        assert!(server.tool_registered("any_tool"));
        assert_eq!(server.tool_policy("any_tool"), "auto");
    }

    #[test]
    fn tool_deny_blocks() {
        let mut server = McpServerConfig {
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
            tool_deny: vec!["dangerous_tool".into()],
            trusted: false,
        };
        assert!(!server.tool_registered("dangerous_tool"));
        assert!(server.tool_registered("safe_tool"));
        server.tool_deny.clear();
        assert!(server.tool_registered("dangerous_tool"));
    }

    #[test]
    fn tool_allow_restricts() {
        let server = McpServerConfig {
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
            tool_allow: vec!["only_this".into()],
            tool_deny: vec![],
            trusted: false,
        };
        assert!(server.tool_registered("only_this"));
        assert!(!server.tool_registered("anything_else"));
    }

    #[test]
    fn per_tool_policy_wins() {
        let server = McpServerConfig {
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
            tool_policies: vec![ToolPolicyEntry {
                tool: "special".into(),
                policy: "deny".into(),
            }],
            default_tool_policy: "auto".into(),
            tool_allow: vec![],
            tool_deny: vec![],
            trusted: false,
        };
        assert_eq!(server.tool_policy("special"), "deny");
        assert_eq!(server.tool_policy("other"), "auto");
    }
}
