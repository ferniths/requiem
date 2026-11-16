//! File read/write/edit/delete tools.

use std::path::Path;

use serde_json::Value;

use crate::Result;
use crate::Tool;

/// Reads a file and returns its contents as a JSON object with
/// `path`, `content`, and `size` fields.
pub struct FileReadTool;

impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a text file and return its contents. Arguments: path (string)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path to the file"}
            },
            "required": ["path"]
        })
    }

    fn execute(&self, args: &Value) -> Result<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let path = Path::new(path_str);
        let content =
            std::fs::read_to_string(path).map_err(|e| crate::ToolError::File(e.to_string()))?;
        let size = content.len();
        Ok(serde_json::json!({
            "path": path_str,
            "content": content,
            "size": size
        }))
    }
}

/// Writes a file (creates or overwrites). Returns `path` and `written` bytes.
pub struct FileWriteTool;

impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating it (and parent dirs) if needed. \
         Arguments: path (string), content (string)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path to the file"},
                "content": {"type": "string", "description": "Full content to write"}
            },
            "required": ["path", "content"]
        })
    }

    fn execute(&self, args: &Value) -> Result<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let path = Path::new(path_str);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| crate::ToolError::File(e.to_string()))?;
        }
        std::fs::write(path, content).map_err(|e| crate::ToolError::File(e.to_string()))?;
        Ok(serde_json::json!({
            "path": path_str,
            "written": content.len()
        }))
    }
}

/// Edits a file by replacing a single occurrence of `old` with `new`.
/// Returns the number of replacements made.
pub struct FileEditTool;

impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Replace the first occurrence of old_text with new_text in a file. \
         Arguments: path (string), old_text (string), new_text (string)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path to the file"},
                "old_text": {"type": "string", "description": "Exact text to find"},
                "new_text": {"type": "string", "description": "Replacement text"}
            },
            "required": ["path", "old_text", "new_text"]
        })
    }

    fn execute(&self, args: &Value) -> Result<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let old_text = args.get("old_text").and_then(|v| v.as_str()).unwrap_or("");
        let new_text = args.get("new_text").and_then(|v| v.as_str()).unwrap_or("");
        let path = Path::new(path_str);
        let content =
            std::fs::read_to_string(path).map_err(|e| crate::ToolError::File(e.to_string()))?;
        let new_content = content.replace(old_text, new_text);
        if new_content == content {
            return Err(crate::ToolError::EditNotFound(old_text.to_string()));
        }
        std::fs::write(path, &new_content).map_err(|e| crate::ToolError::File(e.to_string()))?;
        Ok(serde_json::json!({
            "path": path_str,
            "replaced": true
        }))
    }
}

/// Deletes a file. Returns `deleted: true` on success.
pub struct FileDeleteTool;

impl Tool for FileDeleteTool {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn description(&self) -> &str {
        "Delete a file. Arguments: path (string)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path to the file"}
            },
            "required": ["path"]
        })
    }

    fn execute(&self, args: &Value) -> Result<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let path = Path::new(path_str);
        std::fs::remove_file(path).map_err(|e| crate::ToolError::File(e.to_string()))?;
        Ok(serde_json::json!({
            "path": path_str,
            "deleted": true
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let path_str = path.to_str().unwrap();

        let write = FileWriteTool;
        let result = write
            .execute(&serde_json::json!({"path": path_str, "content": "hello"}))
            .unwrap();
        assert_eq!(result["path"], path_str);

        let read = FileReadTool;
        let result = read
            .execute(&serde_json::json!({"path": path_str}))
            .unwrap();
        assert_eq!(result["content"], "hello");
    }

    #[test]
    fn edit_replaces_occurrence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit.txt");
        let path_str = path.to_str().unwrap();
        std::fs::write(&path, "old line\n").unwrap();

        let edit = FileEditTool;
        let result = edit
            .execute(&serde_json::json!({
                "path": path_str,
                "old_text": "old line",
                "new_text": "new line"
            }))
            .unwrap();
        assert_eq!(result["replaced"], true);

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "new line\n");
    }

    #[test]
    fn edit_not_found_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nofound.txt");
        let path_str = path.to_str().unwrap();
        std::fs::write(&path, "content").unwrap();

        let edit = FileEditTool;
        let result = edit.execute(&serde_json::json!({
            "path": path_str,
            "old_text": "does not exist",
            "new_text": "replacement"
        }));
        assert!(result.is_err());
    }
}
