//! Obsidian vault tool.
//!
//! Exposes vault read/write/audit operations as a single `obsidian` action
//! tool so the agent runtime can write research findings to the user's
//! vault(s). Supports multiple vaults from the harness configuration; the
//! first configured vault is the default target.

use std::path::PathBuf;

use serde_json::Value;

use crate::{Result, Tool, ToolError};
use requiem_obsidian::Vault;

/// A tool that reads and writes notes in one or more Obsidian vaults.
///
/// Construct with [`ObsidianTool::new`] for a single vault, or
/// [`ObsidianTool::with_vaults`] for the multi-vault configuration
/// (first entry = default).
pub struct ObsidianTool {
    /// Vaults in priority order; index 0 is the default.
    vaults: Vec<Vault>,
}

impl ObsidianTool {
    /// Build a tool for a single vault (kept for simple setups).
    pub fn new(vault_root: PathBuf) -> Self {
        Self {
            vaults: vec![Vault::new(vault_root)],
        }
    }

    /// Build a tool for multiple vaults. The first entry becomes the
    /// default target for calls that do not name a vault.
    pub fn with_vaults(roots: Vec<PathBuf>) -> Self {
        let vaults: Vec<Vault> = roots.into_iter().map(Vault::new).collect();
        if vaults.is_empty() {
            // Defensive: a no-root tool degrades to a clear error at
            // execute time rather than panicking.
            tracing::warn!("ObsidianTool built with zero vaults; all operations will fail");
        }
        Self { vaults }
    }

    /// The configured vault names (directory names), in priority order.
    pub fn vault_names(&self) -> Vec<String> {
        self.vaults
            .iter()
            .map(|v| {
                v.root()
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| v.root().to_string_lossy().to_string())
            })
            .collect()
    }

    /// Resolve the vault to operate on: by name when `vault` is given,
    /// otherwise the default (first) vault.
    fn resolve_vault(&self, args: &Value) -> Result<(usize, &Vault)> {
        if self.vaults.is_empty() {
            return Err(ToolError::File(
                "no Obsidian vault is configured; run `requiem setup`".to_string(),
            ));
        }
        let requested = args.get("vault").and_then(|v| v.as_str()).unwrap_or("");
        if requested.is_empty() {
            return Ok((0, &self.vaults[0]));
        }
        for (i, v) in self.vaults.iter().enumerate() {
            let name = v
                .root()
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // Match by vault name or by full path.
            if name == requested || v.root().to_string_lossy() == requested {
                return Ok((i, v));
            }
        }
        Err(ToolError::InvalidArgument(format!(
            "unknown vault '{requested}'; configured vaults: {}",
            self.vault_names().join(", ")
        )))
    }

    fn handle_read(&self, args: &Value) -> Result<Value> {
        let (_, vault) = self.resolve_vault(args)?;
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            return Err(ToolError::InvalidArgument("path is required".into()));
        }
        match vault.read_note(path) {
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
        let (_, vault) = self.resolve_vault(args)?;
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            return Err(ToolError::InvalidArgument("path is required".into()));
        }
        match vault.write_note(path, content) {
            Ok(entry) => Ok(serde_json::json!({
                "action": "write",
                "path": path,
                "status": "written",
                "change_log_entry": entry,
            })),
            Err(e) => Err(ToolError::File(e.to_string())),
        }
    }

    fn handle_list(&self, args: &Value) -> Result<Value> {
        let (idx, vault) = self.resolve_vault(args)?;
        let root = vault.root().to_path_buf();
        let notes: Vec<String> = vault
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
            "vault": self.vault_names().get(idx).cloned().unwrap_or_default(),
            "count": notes.len(),
            "notes": notes,
        }))
    }

    fn handle_audit(&self, args: &Value) -> Result<Value> {
        let (_, vault) = self.resolve_vault(args)?;
        let result = requiem_obsidian::audit::audit(vault);
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

#[async_trait::async_trait]
impl Tool for ObsidianTool {
    fn name(&self) -> &str {
        "obsidian"
    }

    fn description(&self) -> &str {
        "Read, write, list, or audit notes in an Obsidian vault. \
         Actions: read (path), write (path + content), list, audit. \
         Optional `vault` argument selects a configured vault by name \
         (defaults to the first configured vault). \
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
                },
                "vault": {
                    "type": "string",
                    "description": "Vault name (defaults to the first configured vault)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: &Value) -> Result<Value> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "read" => self.handle_read(args),
            "write" => self.handle_write(args),
            "list" => self.handle_list(args),
            "audit" => self.handle_audit(args),
            _ => Err(ToolError::InvalidArgument(format!(
                "unknown obsidian action: {action}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_vault_root(tag: &str) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        // Keep the tempdir alive by leaking it (test only).
        let path: PathBuf = dir.path().join(tag);
        std::fs::create_dir_all(&path).unwrap();
        std::mem::forget(dir);
        path
    }

    fn test_tool() -> ObsidianTool {
        ObsidianTool::new(temp_vault_root("single"))
    }

    #[test]
    fn schema_is_valid_json() {
        let tool = test_tool();
        let schema = tool.schema();
        assert!(!schema.to_string().is_empty());
    }

    #[tokio::test]
    async fn execute_rejects_missing_action() {
        let tool = test_tool();
        let result = tool.execute(&serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn write_then_list() {
        let tool = test_tool();
        tool.execute(&serde_json::json!({
            "action": "write",
            "path": "test.md",
            "content": "# Hello"
        }))
        .await
        .unwrap();
        let list = tool
            .execute(&serde_json::json!({"action": "list"}))
            .await
            .unwrap();
        assert_eq!(list["count"], 1);
    }

    #[tokio::test]
    async fn multi_vault_selects_by_name() {
        let v1 = temp_vault_root("vault-one");
        let v2 = temp_vault_root("vault-two");
        let tool = ObsidianTool::with_vaults(vec![v1.clone(), v2.clone()]);

        // Write to the second vault explicitly.
        tool.execute(&serde_json::json!({
            "action": "write",
            "vault": "vault-two",
            "path": "note.md",
            "content": "in two"
        }))
        .await
        .unwrap();

        // Default vault (first) must NOT contain the note.
        let list_default = tool
            .execute(&serde_json::json!({"action": "list"}))
            .await
            .unwrap();
        assert_eq!(list_default["count"], 0);

        // The named vault must contain it.
        let list_two = tool
            .execute(&serde_json::json!({"action": "list", "vault": "vault-two"}))
            .await
            .unwrap();
        assert_eq!(list_two["count"], 1);
        assert_eq!(list_two["vault"], "vault-two");
        assert!(v2.join("note.md").exists());
        assert!(!v1.join("note.md").exists());
    }

    #[tokio::test]
    async fn unknown_vault_name_is_a_clear_error() {
        let tool = ObsidianTool::with_vaults(vec![temp_vault_root("only")]);
        let err = tool
            .execute(&serde_json::json!({"action": "list", "vault": "nope"}))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown vault 'nope'"), "got: {msg}");
        assert!(msg.contains("only"), "error must list configured vaults");
    }

    #[tokio::test]
    async fn zero_vaults_gives_setup_hint() {
        let tool = ObsidianTool::with_vaults(vec![]);
        let err = tool
            .execute(&serde_json::json!({"action": "list"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("requiem setup"));
    }
}
