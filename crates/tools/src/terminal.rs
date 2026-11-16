//! Terminal command execution tool.

use serde_json::Value;

use crate::{Result, Tool};

/// Runs a shell command and returns stdout, stderr, and exit code.
pub struct TerminalTool;

impl Tool for TerminalTool {
    fn name(&self) -> &str {
        "run_terminal"
    }

    fn description(&self) -> &str {
        "Run a shell command and capture its output. \
         Arguments: command (string), directory (optional string)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The shell command to run"},
                "directory": {
                    "type": "string",
                    "description": "Working directory (defaults to current)"
                }
            },
            "required": ["command"]
        })
    }

    fn execute(&self, args: &Value) -> Result<Value> {
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if command.is_empty() {
            return Err(crate::ToolError::InvalidArgument(
                "empty command".to_string(),
            ));
        }
        let dir = args.get("directory").and_then(|v| v.as_str());

        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        if let Some(d) = dir {
            cmd.current_dir(d);
        }

        let output = cmd
            .output()
            .map_err(|e| crate::ToolError::Terminal(e.to_string()))?;
        Ok(serde_json::json!({
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "exit_code": output.status.code().unwrap_or(-1)
        }))
    }
}
