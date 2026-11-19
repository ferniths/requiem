//! The MCP client: wraps an `rmcp` peer and exposes typed methods for
//! tool/resource/prompt discovery and invocation.
//!
//! Each [`McpClient`] is bound to one server. The manager owns one
//! client per configured server and hands out `Arc` clones so that
//! nested agents can share the same underlying connection.

use std::time::Instant;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, GetPromptRequestParams,
    ReadResourceRequestParams,
};
use tracing::Instrument;

use crate::config::McpServerConfig;
use crate::errors::McpError;
use crate::resources::McpResource;
use crate::prompts::McpPrompt;

/// A single connected MCP client.
///
/// The `service` field is the `rmcp` running service (which owns the
/// peer and keeps the connection alive). It is stored behind a
/// `tokio::sync::Mutex` because the service is not `Sync` —
/// concurrent tool calls on the same server are serialized through
/// the JSON-RPC stream.
pub struct McpClient {
    /// The server alias.
    pub server_name: String,
    /// The `rmcp` running service (keeps the connection alive).
    pub service: std::sync::Arc<tokio::sync::Mutex<rmcp::service::RunningService<rmcp::service::RoleClient, ()>>>,
    /// The resolved server config (with secrets already substituted).
    pub config: McpServerConfig,
}

impl McpClient {
    /// List all tools the server exposes.
    ///
    /// Returns the raw `rmcp` tool list; the manager maps these into
    /// REQUIEM's `ToolDef` format.
    pub async fn list_tools(&self) -> Result<Vec<rmcp::model::Tool>, McpError> {
        let peer = self.service.lock().await;
        let result = peer
            .list_tools(None)
            .instrument(tracing::debug_span!("mcp.list_tools", server = %self.server_name))
            .await
            .map_err(|e| McpError::ToolCall {
                tool: "tools/list".into(),
                server: self.server_name.clone(),
                detail: e.to_string(),
            })?;
        Ok(result.tools)
    }

    /// Call a tool by its bare name (without the `server__` prefix).
    ///
    /// The arguments must be a JSON object matching the tool's
    /// `inputSchema`.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        let started = Instant::now();
        let peer = self.service.lock().await;

        // Convert the arguments Value to the JsonObject type rmcp expects.
        let json_object = arguments
            .as_object()
            .cloned()
            .unwrap_or_default();

        let result = peer
            .call_tool(
                CallToolRequestParams::new(tool_name.to_string()).with_arguments(
                    json_object,
                ),
            )
            .instrument(tracing::debug_span!(
                "mcp.call_tool",
                server = %self.server_name,
                tool = tool_name
            ))
            .await
            .map_err(|e| {
                tracing::warn!(
                    server = %self.server_name,
                    tool = tool_name,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    error = %e,
                    "mcp tool call failed"
                );
                McpError::ToolCall {
                    tool: format!("{}__{tool_name}", self.server_name),
                    server: self.server_name.clone(),
                    detail: e.to_string(),
                }
            })?;

        // Normalize the result: extract the text content parts and
        // serialize them as a JSON value the agent runtime can
        // consume directly.
        let elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(normalize_tool_result(&result, elapsed_ms))
    }

    /// List all resources the server exposes.
    pub async fn list_resources(&self) -> Result<Vec<McpResource>, McpError> {
        let peer = self.service.lock().await;
        let result = peer
            .list_resources(None)
            .instrument(tracing::debug_span!("mcp.list_resources", server = %self.server_name))
            .await
            .map_err(|e| McpError::ToolCall {
                tool: "resources/list".into(),
                server: self.server_name.clone(),
                detail: e.to_string(),
            })?;
        Ok(result.resources.into_iter().map(McpResource::from).collect())
    }

    /// Read a resource by URI.
    pub async fn read_resource(&self, uri: &str) -> Result<serde_json::Value, McpError> {
        let peer = self.service.lock().await;
        let result = peer
            .read_resource(ReadResourceRequestParams::new(uri))
            .instrument(tracing::debug_span!("mcp.read_resource", server = %self.server_name, uri))
            .await
            .map_err(|e| McpError::ToolCall {
                tool: format!("resources/read:{uri}"),
                server: self.server_name.clone(),
                detail: e.to_string(),
            })?;
        Ok(serde_json::json!({
            "contents": result.contents,
        }))
    }

    /// List all prompts the server exposes.
    pub async fn list_prompts(&self) -> Result<Vec<McpPrompt>, McpError> {
        let peer = self.service.lock().await;
        let result = peer
            .list_prompts(None)
            .instrument(tracing::debug_span!("mcp.list_prompts", server = %self.server_name))
            .await
            .map_err(|e| McpError::ToolCall {
                tool: "prompts/list".into(),
                server: self.server_name.clone(),
                detail: e.to_string(),
            })?;
        Ok(result.prompts.into_iter().map(McpPrompt::from).collect())
    }

    /// Get a prompt by name, optionally with arguments.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        let peer = self.service.lock().await;
        let json_object = arguments
            .as_object()
            .cloned()
            .unwrap_or_default();
        let result = peer
            .get_prompt(
                GetPromptRequestParams::new(name.to_string()).with_arguments(
                    json_object,
                ),
            )
            .instrument(tracing::debug_span!("mcp.get_prompt", server = %self.server_name, prompt = name))
            .await
            .map_err(|e| McpError::ToolCall {
                tool: format!("prompts/get:{name}"),
                server: self.server_name.clone(),
                detail: e.to_string(),
            })?;
        Ok(serde_json::json!({
            "description": result.description,
            "messages": result.messages,
        }))
    }

    /// The server's peer info (name, version) from the initialize
    /// handshake.
    pub async fn peer_info(&self) -> Option<std::sync::Arc<rmcp::model::ServerPeerInfo>> {
        let svc = self.service.lock().await;
        svc.peer_info()
    }
}

/// Normalize an MCP tool result into a `serde_json::Value` that the
/// agent runtime can return to the model as a tool-result string.
///
/// The MCP `CallToolResult` has a `content` array of content parts
/// (text, image, resource). This function extracts the text parts and
/// serializes them into a structured JSON value, preserving any
/// structured content that the server attached.
fn normalize_tool_result(
    result: &CallToolResult,
    elapsed_ms: u64,
) -> serde_json::Value {
    let mut text_parts: Vec<String> = Vec::new();
    let mut resource_parts: Vec<serde_json::Value> = Vec::new();

    for content in &result.content {
        match content {
            ContentBlock::Text(tc) => text_parts.push(tc.text.clone()),
            ContentBlock::Image(_) => {
                text_parts.push("[image content omitted]".into());
            }
            ContentBlock::Audio(_) => {
                text_parts.push("[audio content omitted]".into());
            }
            ContentBlock::Resource(r) => {
                let text = r.get_text();
                resource_parts.push(serde_json::json!({
                    "text": text,
                }));
            }
            ContentBlock::ResourceLink(rl) => {
                resource_parts.push(serde_json::json!({
                    "uri": rl.uri,
                    "name": rl.name,
                }));
            }
            _ => {}
        }
    }

    let text = if text_parts.is_empty() {
        "(no text content)".to_string()
    } else {
        text_parts.join("\n")
    };

    let mut output = serde_json::json!({
        "text": text,
        "is_error": result.is_error.unwrap_or(false),
        "elapsed_ms": elapsed_ms,
    });

    if !resource_parts.is_empty() {
        output["resources"] = serde_json::json!(resource_parts);
    }

    // Preserve structured content if the server attached it.
    if let Some(structured) = &result.structured_content {
        output["structured"] = structured.clone();
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{TextContent, CallToolResult};

    #[test]
    fn normalize_empty_content() {
        let result = CallToolResult::success(vec![]);
        let output = normalize_tool_result(&result, 42);
        assert_eq!(output["text"], "(no text content)");
        assert_eq!(output["elapsed_ms"], 42);
        assert_eq!(output["is_error"], false);
    }

    #[test]
    fn normalize_text_content() {
        let result = CallToolResult::success(vec![ContentBlock::Text(
            TextContent::new("hello"),
        )]);
        let output = normalize_tool_result(&result, 10);
        assert_eq!(output["text"], "hello");
        assert_eq!(output["is_error"], false);
    }

    #[test]
    fn normalize_structured_content() {
        let mut result = CallToolResult::success(vec![ContentBlock::Text(
            TextContent::new("done"),
        )]);
        result.structured_content = Some(serde_json::json!({"key": "value"}));
        let output = normalize_tool_result(&result, 5);
        assert_eq!(output["structured"]["key"], "value");
    }
}
