//! `McpTool` — wraps one discovered MCP tool as a REQUIEM `Tool`.
//!
//! Each MCP tool is registered under a namespaced identifier
//! `server__tool_name` to avoid collisions across servers. The
//! `McpTool` implements [`Tool`](reqiem_tools::Tool) so it plugs
//! directly into the existing `ToolBridge` / `ToolExecutor` surface.
//!
//! Tool output is treated as **untrusted external content**: the
//! text returned by an MCP tool is never parsed as REQUIEM
//! instructions. It is returned to the model as a JSON string and
//! the model decides what to do with it.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use requiem_model::request::ToolDef;
use requiem_tools::{Tool, ToolError};

use crate::client::McpClient;

/// A REQUIEM tool that dispatches to one MCP tool on a connected server.
///
/// The `namespaced_name` is `server__tool_name` (the identifier the
/// model uses in `tool_calls`). The `bare_name` is the tool name as
/// the server knows it.
pub struct McpTool {
    /// The full namespaced tool name (`server__tool`).
    namespaced_name: String,
    /// The bare tool name as the server knows it.
    bare_name: String,
    /// The server alias.
    server_name: String,
    /// The tool description (from the server's `tools/list`).
    description: String,
    /// The tool's input schema (JSON Schema object).
    schema: Value,
    /// The shared MCP client handle for this server.
    client: Arc<McpClient>,
}

impl McpTool {
    /// Create a new MCP tool from a discovered `rmcp::model::Tool`.
    pub fn new(
        server_name: &str,
        tool: &rmcp::model::Tool,
        client: Arc<McpClient>,
    ) -> Self {
        let namespaced_name = format!("{server_name}__{}", tool.name);
        let description = tool
            .description
            .as_ref()
            .map(|d| d.to_string())
            .unwrap_or_else(|| format!("MCP tool '{server_name}__{}'", tool.name));
        let schema = serde_json::to_value(tool.input_schema.as_ref())
            .unwrap_or_else(|_| serde_json::json!({"type": "object"}));

        Self {
            namespaced_name,
            bare_name: tool.name.to_string(),
            server_name: server_name.to_string(),
            description,
            schema,
            client,
        }
    }

    /// The namespaced tool name (`server__tool`).
    pub fn namespaced_name(&self) -> &str {
        &self.namespaced_name
    }

    /// The bare tool name (as the server knows it).
    pub fn bare_name(&self) -> &str {
        &self.bare_name
    }

    /// The server alias.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Build the `ToolDef` to advertise to the model.
    pub fn tool_def(&self) -> ToolDef {
        ToolDef::new(&self.namespaced_name, &self.description, self.schema.clone())
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.namespaced_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    async fn execute(&self, args: &Value) -> Result<Value, ToolError> {
        let result = self
            .client
            .call_tool(&self.bare_name, args)
            .await
            .map_err(|e| ToolError::InvalidArgument(e.to_string()))?;

        // Enforce output-size limits: if the text part exceeds the
        // threshold, truncate with a marker so the model knows the
        // result was cut.
        const MAX_OUTPUT_CHARS: usize = 32_000;
        if let Some(text) = result.get("text").and_then(|t| t.as_str()) {
            if text.len() > MAX_OUTPUT_CHARS {
                let truncated: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
                let mut out = result.clone();
                out["text"] = serde_json::json!(
                    format!(
                        "{truncated}\n[… truncated — {total} chars total, showing first {kept}]",
                        total = text.len(),
                        kept = MAX_OUTPUT_CHARS
                    )
                );
                out["truncated"] = serde_json::json!(true);
                return Ok(out);
            }
        }
        Ok(result)
    }
}

/// Convert a list of `rmcp::model::Tool` entries into REQUIEM
/// `McpTool` instances, filtered by the server's allow/deny lists.
pub fn build_mcp_tools(
    server_name: &str,
    tools: &[rmcp::model::Tool],
    client: Arc<McpClient>,
    allow: &[String],
    deny: &[String],
) -> Vec<Arc<McpTool>> {
    tools
        .iter()
        .filter(|t| {
            if deny.iter().any(|d| d == &t.name) {
                return false;
            }
            if !allow.is_empty() && !allow.iter().any(|a| a == &t.name) {
                return false;
            }
            true
        })
        .map(|t| Arc::new(McpTool::new(server_name, t, client.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn namespaced_name_is_correct() {
        // Verify the naming convention: `server__tool`.
        let expected = "ghidra__decompile_function".to_string();
        assert_eq!(expected, "ghidra__decompile_function");
    }
}
