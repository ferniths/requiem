//! Git operations tool: status, diff, log, add, commit.

use serde_json::Value;

use crate::{Result, Tool};

/// Git operations. Arguments: action (string), plus optional
/// `message` (for commit), `paths` (array, for add).
pub struct GitTool;

#[async_trait::async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "run_git"
    }

    fn description(&self) -> &str {
        "Run a git command. Arguments: action (string — one of \
         status, diff, log, add, commit, branch, remote), \
         message (string, for commit), paths (array of strings, for add)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "add", "commit", "branch", "remote"],
                    "description": "The git subcommand to run"
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (required for commit)"
                },
                "paths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "File paths to stage (for add)"
                },
                "n": {
                    "type": "integer",
                    "description": "Number of log entries (default 5)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: &Value) -> Result<Value> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if action.is_empty() {
            return Err(crate::ToolError::InvalidArgument(
                "action is required".to_string(),
            ));
        }

        let mut cmd = std::process::Command::new("git");
        cmd.arg(action);

        match action {
            "status" => {}
            "diff" => {
                cmd.arg("--stat");
            }
            "log" => {
                let n = args.get("n").and_then(|v| v.as_u64()).unwrap_or(5);
                cmd.args(["-n", &n.to_string(), "--oneline"]);
            }
            "add" => {
                if let Some(paths) = args.get("paths").and_then(|v| v.as_array()) {
                    for p in paths {
                        if let Some(s) = p.as_str() {
                            cmd.arg(s);
                        }
                    }
                } else {
                    cmd.arg("-A");
                }
            }
            "commit" => {
                let msg = args
                    .get("message")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        crate::ToolError::InvalidArgument(
                            "commit requires a non-empty message".to_string(),
                        )
                    })?;
                cmd.args(["-m", msg]);
            }
            "branch" => {
                cmd.arg("-a");
            }
            "remote" => {
                cmd.args(["-v"]);
            }
            other => {
                return Err(crate::ToolError::InvalidArgument(format!(
                    "unknown git action: {other}"
                )))
            }
        }

        let output = cmd
            .output()
            .map_err(|e| crate::ToolError::Terminal(e.to_string()))?;
        Ok(serde_json::json!({
            "action": action,
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "exit_code": output.status.code().unwrap_or(-1)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_valid_json() {
        let tool = GitTool;
        let schema = tool.schema();
        assert!(schema.get("properties").is_some());
        assert_eq!(tool.name(), "run_git");
    }

    #[tokio::test]
    async fn execute_rejects_missing_action() {
        let tool = GitTool;
        let args = serde_json::json!({});
        let result = tool.execute(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_unknown_action() {
        let tool = GitTool;
        let args = serde_json::json!({"action": "frobinate"});
        let result = tool.execute(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn commit_requires_message() {
        let tool = GitTool;
        let args = serde_json::json!({"action": "commit"});
        let result = tool.execute(&args).await;
        assert!(result.is_err());
    }
}
