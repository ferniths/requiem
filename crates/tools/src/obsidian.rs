//! Obsidian vault tool.
//!
//! Exposes vault read/write/audit operations as a single `obsidian` action
//! tool so the agent runtime can write research findings to the user's vault.

use std::path::PathBuf;

use serde_json::Value;

use crate::{Result, Tool, ToolError};
use requiem_obsidian::Vault;

/// A tool that reads and writes notes in an Obsidian vault.
pub struct ObsidianTool {
    vault: Vault,
}

impl ObsidianTool {
    pub fn new(vault_root: PathBuf) -> Self {
        Self {
            vault: Vault::new(vault_root),
        }
    }

    fn handle_read(&self, args: &Value) -> Result<Value> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            return Err(ToolError::InvalidArgument("path is required".into()));
        }
        match self.vault.read_note(path) {
            Ok(content) => Ok(serde_json::json!({
                "action": "read",
                "path": path,
                "content": content,
            })),
            Err(e) => Ok(serde_json::json!({
                "action": "read",
                "path": path,
                "error": e.to_string(),
            })),
        }
    }

    fn handle_write(&self, args: &Value) -> Result<Value> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            return Err(ToolError::InvalidArgument("path is required".into()));
        }
        match self.vault.write_note(path, content) {
            Ok(entry) => Ok(serde_json::json!({
                "action": "write",
                "path": path,
                "status": "written",
                "change_log_entry": entry,
            })),
            Err(e) => Err(ToolError::File(e.to_string())),
        }
    }

    fn handle_list(&self) -> Result<Value> {
        let root = self.vault.root().to_path_buf();
        let notes: Vec<String> = self
            .vault
            .list_notes()
            .iter()
            .filter_map(|p| {
                p.strip_prefix(&root)
                    .ok()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .collect();
        Ok(serde_json::json!({
            "action": "list",
            "count": notes.len(),
            "notes": notes,
        }))
    }

    fn handle_audit(&self) -> Result<Value> {
        let result = requiem_obsidian::audit::audit(&self.vault);
        Ok(serde_json::json!({
            "action": "audit",
            "broken_links": result.broken_links.len(),
            "orphans": result.orphan_notes.len(),
            "duplicates": result.duplicates.len(),
            "broken_link_samples": result.broken_links.iter().take(5).map(|b| &b.source_note).collect::<Vec<_>>(),
            "orphan_samples": result.orphan_notes.iter().take(5).map(|o| &o.path).collect::<Vec<_>>(),
        }))
    }
}

impl Tool for ObsidianTool {
    fn name(&self) -> &str {
        "obsidian"
    }

    fn description(&self) -> &str {
        "Read, write, list, or audit notes in the Obsidian vault. \
         Actions: read (path), write (path + content), list, audit. \
         Writes are atomic and append to the vault change log."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "write", "list", "audit"],
                    "description": "The vault operation to perform"
                },
                "path": {
                    "type": "string",
                    "description": "Note path relative to vault root (e.g. '01 Foundations/note.md')"
                },
                "content": {
                    "type": "string",
                    "description": "Note content (required for write)"
                }
            },
            "required": ["action"]
        })
    }

    fn execute(&self, args: &Value) -> Result<Value> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "read" => self.handle_read(args),
            "write" => self.handle_write(args),
            "list" => self.handle_list(),
            "audit" => self.handle_audit(),
            _ => Err(ToolError::InvalidArgument(format!(
                "unknown obsidian action: {action}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tool() -> ObsidianTool {
        let dir = tempfile::tempdir().unwrap();
        // Keep the tempdir alive by leaking it (test only).
        let path: PathBuf = dir.path().to_path_buf();
        std::mem::forget(dir);
        ObsidianTool::new(path)
    }

    #[test]
    fn schema_is_valid_json() {
        let tool = test_tool();
        let schema = tool.schema();
        assert!(!schema.to_string().is_empty());
    }

    #[test]
    fn execute_rejects_missing_action() {
        let tool = test_tool();
        let result = tool.execute(&serde_json::json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn write_then_list() {
        let tool = test_tool();
        tool.execute(&serde_json::json!({
            "action": "write",
            "path": "test.md",
            "content": "# Hello"
        }))
        .unwrap();
        let list = tool
            .execute(&serde_json::json!({"action": "list"}))
            .unwrap();
        assert_eq!(list["count"], 1);
    }
}
