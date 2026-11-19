//! MCP resource discovery and access.

use serde::{Deserialize, Serialize};

/// A resource exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpResource {
    /// The resource URI.
    pub uri: String,
    /// A human-readable name.
    #[serde(default)]
    pub name: String,
    /// The MIME type (e.g. `"application/json"`).
    #[serde(default)]
    pub mime_type: String,
    /// An optional description.
    #[serde(default)]
    pub description: String,
}

impl From<rmcp::model::Resource> for McpResource {
    fn from(r: rmcp::model::Resource) -> Self {
        Self {
            uri: r.uri,
            name: r.name,
            mime_type: r.mime_type.unwrap_or_default(),
            description: r.description.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_rmcp_resource() {
        let r = rmcp::model::Resource::new("demo://resource/test", "test-resource");
        let mcp = McpResource::from(r);
        assert_eq!(mcp.uri, "demo://resource/test");
        assert_eq!(mcp.name, "test-resource");
    }
}
