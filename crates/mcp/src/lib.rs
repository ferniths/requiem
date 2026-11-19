//! REQUIEM MCP — generic Model Context Protocol client.
//!
//! This crate provides a first-class MCP integration that lets REQUIEM
//! connect to *any* compatible MCP server (Ghidra, IDA Pro, filesystem,
//! browser, database, doc servers, …) and use their tools within the
//! existing multi-agent orchestration.
//!
//! ## Architecture
//!
//! * [`config`] — TOML-deserializable server definitions (transports,
//!   auth, per-server timeouts, allow/deny lists, concurrency limits).
//! * [`server`] — per-server lifecycle state machine (Disconnected →
//!   Starting → Connecting → … → Connected → Degraded / Failed →
//!   Stopping).
//! * [`client`] — the MCP client: init handshake, capability negotiation,
//!   tool/resource/prompt discovery, tool invocation.
//! * [`manager`] — the [`McpManager`] that owns every configured server,
//!   handles dynamic refresh, per-server request limits, and clean
//!   shutdown.
//! * [`tools`] — `McpTool` (implements [`Tool`](reqiem_tools::Tool)) that
//!   wraps one discovered MCP tool; namespaced as `server__tool`.
//! * [`resources`] / [`prompts`] — resource/prompt discovery and access.
//! * [`lifecycle`] — convenience helpers for connect/disconnect/reconnect.
//! * [`permissions`] — per-server and per-tool permission checks that
//!   plug into REQUIEM's existing [`PermissionGate`](reqiem_permissions::PermissionGate).
//! * [`errors`] — MCP-specific error taxonomy, mapping to
//!   [`RequiemError`](reqiem_core::error::RequiemError) where sensible.

pub mod client;
pub mod config;
pub mod errors;
pub mod lifecycle;
pub mod manager;
pub mod permissions;
pub mod prompts;
pub mod resources;
pub mod server;
pub mod tools;
pub mod transport;

// Re-export the types most callers need at the crate root.
pub use client::McpClient;
pub use config::{McpConfig, McpServerConfig, McpTransport};
pub use errors::McpError;
pub use lifecycle::{McpLifecycle, McpStatus};
pub use manager::McpManager;
pub use permissions::McpPermissionPolicy;
pub use prompts::McpPrompt;
pub use resources::McpResource;
pub use server::{McpServer, ServerState};
pub use tools::McpTool;
pub use transport::{stdio, http};
