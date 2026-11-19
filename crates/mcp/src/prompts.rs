//! MCP prompt discovery and access.

use serde::{Deserialize, Serialize};

/// A prompt exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpPrompt {
    /// The prompt name.
    pub name: String,
    /// An optional description.
    #[serde(default)]
    pub description: String,
    /// The argument schema (JSON Schema object) for this prompt.
    #[serde(default)]
    pub arguments: serde_json::Value,
}

impl From<rmcp::model::Prompt> for McpPrompt {
    fn from(p: rmcp::model::Prompt) -> Self {
        Self {
            name: p.name,
            description: p.description.unwrap_or_default(),
            arguments: serde_json::json!(p.arguments.unwrap_or_default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_rmcp_prompt() {
        let p = rmcp::model::Prompt::new("code-review", Some("Generate a code review prompt"), None);
        let mcp = McpPrompt::from(p);
        assert_eq!(mcp.name, "code-review");
        assert_eq!(mcp.description, "Generate a code review prompt");
    }
}
