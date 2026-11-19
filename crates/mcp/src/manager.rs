//! The MCP manager: owns every configured server, handles connect /
//! disconnect / reconnect, and registers discovered tools into the
//! REQUIEM tool registry.
//!
//! The manager is designed to be `Send + Sync` so it can be shared
//! across the agent runtime via `Arc`. Each server has its own
//! `McpClient` (which serializes JSON-RPC calls through a mutex), so
//! concurrent agents can call tools on different servers in parallel.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use rmcp::ServiceExt;

use crate::client::McpClient;
use crate::config::{McpConfig, McpServerConfig};
use crate::errors::McpError;
use crate::lifecycle::{status_from_state, McpLifecycle, McpStatus};
use crate::server::{McpServer, ServerState};
use crate::tools::build_mcp_tools;
use crate::transport::{http, stdio};

/// The MCP manager: one instance per REQUIEM session.
///
/// Owns the config, the per-server state, and the live `McpClient`
/// handles. Tools discovered from connected servers are registered
/// into the REQUIEM tool registry as `server__tool` entries.
pub struct McpManager {
    /// The root MCP config.
    config: McpConfig,
    /// Per-server state (one entry per configured server).
    servers: Arc<RwLock<HashMap<String, McpServer>>>,
    /// Per-server live client handles.
    clients: Arc<RwLock<HashMap<String, Arc<McpClient>>>>,
    /// Per-server policy objects.
    _policies: Arc<RwLock<HashMap<String, crate::permissions::McpPermissionPolicy>>>,
}

impl McpManager {
    /// Create a manager from a config. No servers are connected yet.
    pub fn new(config: McpConfig) -> Self {
        let mut servers = HashMap::new();
        let mut policies = HashMap::new();
        for sc in &config.servers {
            servers.insert(
                sc.name.clone(),
                McpServer::new(sc.name.clone()),
            );
            policies.insert(
                sc.name.clone(),
                crate::permissions::McpPermissionPolicy::new(sc.clone()),
            );
        }
        Self {
            config,
            servers: Arc::new(RwLock::new(servers)),
            clients: Arc::new(RwLock::new(HashMap::new())),
            _policies: Arc::new(RwLock::new(policies)),
        }
    }

    /// The config.
    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    /// The number of configured servers.
    pub fn server_count(&self) -> usize {
        self.config.servers.len()
    }

    /// Connect to a specific server by name.
    ///
    /// Performs the transport setup, initialize handshake, and tool /
    /// resource / prompt discovery. On success the server transitions
    /// to `Connected` and its tools are available via
    /// [`register_tools`](Self::register_tools).
    pub async fn connect(&self, server_name: &str) -> Result<(), McpError> {
        let sc = self
            .config
            .find(server_name)
            .ok_or_else(|| McpError::Config(format!("unknown server '{server_name}'")))?
            .clone();

        // Mark starting.
        {
            let mut servers = self.servers.write().await;
            if let Some(s) = servers.get_mut(server_name) {
                s.transition(ServerState::Starting, None);
            }
        }

        // Build the transport and serve the client.
        let service = match &sc.transport {
            crate::config::McpTransport::Stdio { .. } => {
                let transport = stdio::build_stdio_transport(&sc)?;
                self.serve_stdio(transport, &sc).await?
            }
            crate::config::McpTransport::Http { .. } => {
                let transport = http::build_http_transport(&sc)?;
                self.serve_http(transport, &sc).await?
            }
        };

        // Wrap in a client.
        let client = Arc::new(McpClient {
            server_name: sc.name.clone(),
            service: Arc::new(tokio::sync::Mutex::new(service)),
            config: sc.clone(),
        });

        // Discover tools, resources, prompts.
        let tools = client.list_tools().await?;
        let resources = client.list_resources().await.unwrap_or_default();
        let prompts = client.list_prompts().await.unwrap_or_default();

        // Update server state.
        {
            let mut servers = self.servers.write().await;
            if let Some(s) = servers.get_mut(&client.server_name) {
                s.transition(ServerState::Connected, None);
                s.tool_count = tools.len();
                s.resource_count = resources.len();
                s.prompt_count = prompts.len();
                s.client = None; // client is tracked separately in `self.clients`
            }
        }

        // Store the live client.
        let client_name = client.server_name.clone();
        {
            let mut clients = self.clients.write().await;
            clients.insert(client_name.clone(), client);
        }

        tracing::info!(
            server = %client_name,
            tools = tools.len(),
            resources = resources.len(),
            prompts = prompts.len(),
            "mcp server connected"
        );
        Ok(())
    }

    /// Disconnect a specific server.
    pub async fn disconnect(&self, server_name: &str) -> Result<(), McpError> {
        {
            let mut servers = self.servers.write().await;
            if let Some(s) = servers.get_mut(server_name) {
                s.transition(ServerState::Stopping, None);
            }
        }

        // Cancel the client to trigger graceful shutdown.
        {
            let clients = self.clients.read().await;
            if let Some(client) = clients.get(server_name) {
                let mut service = client.service.lock().await;
                let _ = service.close().await;
            }
        }

        {
            let mut clients = self.clients.write().await;
            clients.remove(server_name);
        }

        {
            let mut servers = self.servers.write().await;
            if let Some(s) = servers.get_mut(server_name) {
                s.transition(ServerState::Disconnected, None);
                s.tool_count = 0;
                s.resource_count = 0;
                s.prompt_count = 0;
            }
        }

        tracing::info!(server = server_name, "mcp server disconnected");
        Ok(())
    }

    /// Reconnect a server: disconnect then connect.
    pub async fn reconnect(&self, server_name: &str) -> Result<(), McpError> {
        self.disconnect(server_name).await.ok();
        self.connect(server_name).await
    }

    /// Register the discovered tools for a connected server into a
    /// mutable map of REQUIEM tools.
    ///
    /// This is the key integration point: the caller passes in a
    /// `&mut HashMap<String, Arc<dyn Tool>>` (the `ToolBridge`'s
    /// tool map) and this function adds one `McpTool` entry per
    /// discovered tool, filtered by the server's allow/deny lists.
    pub async fn register_tools(
        &self,
        server_name: &str,
        tools_map: &mut HashMap<String, Arc<dyn requiem_tools::Tool>>,
    ) -> Result<usize, McpError> {
        let client = {
            let clients = self.clients.read().await;
            clients
                .get(server_name)
                .cloned()
                .ok_or_else(|| McpError::Config(format!("server '{server_name}' not connected")))?
        };

        let tools = client.list_tools().await?;
        let sc = client.config.clone();

        let mcp_tools = build_mcp_tools(
            server_name,
            &tools,
            client,
            &sc.tool_allow,
            &sc.tool_deny,
        );

        for tool in &mcp_tools {
            tools_map.insert(tool.namespaced_name().to_string(), tool.clone());
        }

        Ok(mcp_tools.len())
    }

    /// Call a tool on a connected server by its bare name.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        // Check permission first.
        let policies = self._policies.read().await;
        if let Some(policy) = policies.get(server_name) {
            crate::permissions::enforce_permission(policy, tool_name)?;
        }
        drop(policies);

        let client = {
            let clients = self.clients.read().await;
            clients
                .get(server_name)
                .cloned()
                .ok_or_else(|| McpError::Config(format!("server '{server_name}' not connected")))?
        };

        let started = std::time::Instant::now();
        let result = client.call_tool(tool_name, arguments).await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        // Update latency tracking.
        {
            let mut servers = self.servers.write().await;
            if let Some(s) = servers.get_mut(server_name) {
                s.last_latency_ms = Some(elapsed_ms);
            }
        }

        Ok(result)
    }

    /// List lifecycle snapshots for all configured servers.
    pub async fn lifecycle_snapshots(&self) -> Vec<McpLifecycle> {
        let servers = self.servers.read().await;
        self.config
            .servers
            .iter()
            .filter_map(|sc| {
                let s = servers.get(&sc.name)?;
                Some(McpLifecycle::new(
                    sc,
                    s.state,
                    s.tool_count,
                    s.resource_count,
                    s.prompt_count,
                    s.last_latency_ms,
                    s.last_error.clone(),
                ))
            })
            .collect()
    }

    /// Whether a server is connected.
    pub async fn is_connected(&self, server_name: &str) -> bool {
        self.clients.read().await.contains_key(server_name)
    }

    /// The status of a specific server.
    pub async fn status(&self, server_name: &str) -> McpStatus {
        let servers = self.servers.read().await;
        if let Some(s) = servers.get(server_name) {
            status_from_state(s.state)
        } else {
            McpStatus::Disconnected
        }
    }

    /// Read-only access to the live client map (for the `/mcp resources`
    /// and `/mcp prompts` commands).
    pub async fn clients_read(
        &self,
    ) -> tokio::sync::RwLockReadGuard<'_, std::collections::HashMap<String, Arc<McpClient>>> {
        self.clients.read().await
    }

    /// Shutdown all connected servers.
    pub async fn shutdown(&self) {
        let names: Vec<String> = self
            .config
            .servers
            .iter()
            .map(|sc| sc.name.clone())
            .collect();
        for name in names {
            self.disconnect(&name).await.ok();
        }
        tracing::info!("mcp manager shutdown complete");
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    async fn serve_stdio(
        &self,
        transport: rmcp::transport::child_process::TokioChildProcess,
        _sc: &McpServerConfig,
    ) -> Result<rmcp::service::RunningService<rmcp::service::RoleClient, ()>, McpError> {
        let timeout = Duration::from_millis(_sc.request_timeout_ms);
        let started = std::time::Instant::now();

        let service = tokio::time::timeout(
            timeout,
            ().serve(transport),
        )
        .await
        .map_err(|_| McpError::Timeout {
            server: _sc.name.clone(),
            timeout_ms: _sc.request_timeout_ms,
        })?
        .map_err(|e| McpError::Startup {
            server: _sc.name.clone(),
            detail: e.to_string(),
        })?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        tracing::debug!(server = %_sc.name, elapsed_ms, "mcp stdio connect");
        Ok(service)
    }

    async fn serve_http(
        &self,
        transport: rmcp::transport::StreamableHttpClientTransport<reqwest::Client>,
        sc: &McpServerConfig,
    ) -> Result<rmcp::service::RunningService<rmcp::service::RoleClient, ()>, McpError> {
        let timeout = Duration::from_millis(sc.request_timeout_ms);
        let started = std::time::Instant::now();

        let service = tokio::time::timeout(
            timeout,
            ().serve(transport),
        )
        .await
        .map_err(|_| McpError::Timeout {
            server: sc.name.clone(),
            timeout_ms: sc.request_timeout_ms,
        })?
        .map_err(|e| McpError::Startup {
            server: sc.name.clone(),
            detail: e.to_string(),
        })?;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        tracing::debug!(server = %sc.name, elapsed_ms, "mcp http connect");
        Ok(service)
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new(McpConfig::default())
    }
}
