//! File read/write/edit/delete tools.

use std::path::Path;

use serde_json::Value;

use crate::Result;
use crate::Tool;

/// Reads a file and returns its contents as a JSON object with
/// `path`, `content`, and `size` fields.
pub struct FileReadTool;

#[async_trait::async_trait]
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

    async fn execute(&self, args: &Value) -> Result<Value> {
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

#[async_trait::async_trait]
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

    async fn execute(&self, args: &Value) -> Result<Value> {
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

/// Edits a file by replacing the **first** occurrence of `old_text` with
/// `new_text`. Returns the number of replacements made (always 0 or 1).
pub struct FileEditTool;

#[async_trait::async_trait]
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

    async fn execute(&self, args: &Value) -> Result<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let old_text = args.get("old_text").and_then(|v| v.as_str()).unwrap_or("");
        let new_text = args.get("new_text").and_then(|v| v.as_str()).unwrap_or("");
        let path = Path::new(path_str);

        if old_text.is_empty() {
            return Err(crate::ToolError::InvalidArgument(
                "old_text must not be empty".to_string(),
            ));
        }
        if old_text == new_text {
            return Err(crate::ToolError::InvalidArgument(
                "old_text and new_text are identical; nothing to replace".to_string(),
            ));
        }

        let content =
            std::fs::read_to_string(path).map_err(|e| crate::ToolError::File(e.to_string()))?;

        // Replace exactly the first occurrence (byte offset), preserving
        // every other occurrence in the file.
        let new_content = match content.find(old_text) {
            Some(pos) => {
                let mut out =
                    String::with_capacity(content.len() + new_text.len() - old_text.len());
                out.push_str(&content[..pos]);
                out.push_str(new_text);
                out.push_str(&content[pos + old_text.len()..]);
                out
            }
            None => return Err(crate::ToolError::EditNotFound(old_text.to_string())),
        };

        std::fs::write(path, &new_content).map_err(|e| crate::ToolError::File(e.to_string()))?;
        Ok(serde_json::json!({
            "path": path_str,
            "replaced": true,
            "replacements": 1
        }))
    }
}

/// Deletes a file. Returns `deleted: true` on success.
pub struct FileDeleteTool;

#[async_trait::async_trait]
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

    async fn execute(&self, args: &Value) -> Result<Value> {
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

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let path_str = path.to_str().unwrap();

        let write = FileWriteTool;
        let result = write
            .execute(&serde_json::json!({"path": path_str, "content": "hello"}))
            .await
            .unwrap();
        assert_eq!(result["path"], path_str);

        let read = FileReadTool;
        let result = read
            .execute(&serde_json::json!({"path": path_str}))
            .await
            .unwrap();
        assert_eq!(result["content"], "hello");
    }

    #[tokio::test]
    async fn edit_replaces_first_occurrence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit.txt");
        let path_str = path.to_str().unwrap();
        std::fs::write(&path, "old line\nold line\nold line\n").unwrap();

        let edit = FileEditTool;
        let result = edit
            .execute(&serde_json::json!({
                "path": path_str,
                "old_text": "old line",
                "new_text": "new line"
            }))
            .await
            .unwrap();
        assert_eq!(result["replaced"], true);
        assert_eq!(result["replacements"], 1);

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "new line\nold line\nold line\n");
    }

    #[tokio::test]
    async fn edit_not_found_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nofound.txt");
        let path_str = path.to_str().unwrap();
        std::fs::write(&path, "content").unwrap();

        let edit = FileEditTool;
        let result = edit
            .execute(&serde_json::json!({
                "path": path_str,
                "old_text": "does not exist",
                "new_text": "replacement"
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn edit_rejects_empty_old_text() {
        let dir = tempfile::tempdir().unwrap();
        let path_str = dir.path().join("e.txt").to_str().unwrap().to_string();
        std::fs::write(&path_str, "content").unwrap();

        let edit = FileEditTool;
        let err = edit
            .execute(&serde_json::json!({"path": path_str, "old_text": "", "new_text": "x"}))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ToolError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn edit_rejects_identical_texts() {
        let dir = tempfile::tempdir().unwrap();
        let path_str = dir.path().join("e.txt").to_str().unwrap().to_string();
        std::fs::write(&path_str, "content").unwrap();

        let edit = FileEditTool;
        let err = edit
            .execute(&serde_json::json!({
                "path": path_str,
                "old_text": "same",
                "new_text": "same"
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ToolError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn delete_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path_str = dir.path().join("gone.txt").to_str().unwrap().to_string();
        std::fs::write(&path_str, "bye").unwrap();

        let del = FileDeleteTool;
        let result = del
            .execute(&serde_json::json!({"path": path_str}))
            .await
            .unwrap();
        assert_eq!(result["deleted"], true);
        assert!(!std::path::Path::new(&path_str).exists());
    }
}
