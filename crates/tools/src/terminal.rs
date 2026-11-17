//! Terminal command execution tool.
//!
//! Cross-platform: on Windows commands run through `cmd /C`, on Unix
//! through `sh -c`. Commands run with a timeout and are killed if they
//! exceed it, so a stuck process cannot hang an agent run.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

use crate::{Result, Tool};

/// Default wall-clock limit for a command.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// Hard ceiling on captured output (per stream), so a chatty command
/// cannot flood the model's context.
const MAX_OUTPUT_CHARS: usize = 20_000;

/// Runs a shell command and returns stdout, stderr, and exit code.
pub struct TerminalTool;

/// Truncate output to `MAX_OUTPUT_CHARS`, keeping the head and marking
/// the cut with an ellipsis line.
fn truncate_output(s: &str) -> (String, bool) {
    if s.chars().count() <= MAX_OUTPUT_CHARS {
        return (s.to_string(), false);
    }
    let head: String = s.chars().take(MAX_OUTPUT_CHARS).collect();
    (format!("{head}\n… (output truncated)"), true)
}

#[async_trait::async_trait]
impl Tool for TerminalTool {
    fn name(&self) -> &str {
        "run_terminal"
    }

    fn description(&self) -> &str {
        "Run a shell command and capture its output. \
         Arguments: command (string), directory (optional string), \
         timeout_ms (optional integer, default 120000). On Windows the \
         command runs via `cmd /C`; on Unix via `sh -c`."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The shell command to run"},
                "directory": {
                    "type": "string",
                    "description": "Working directory (defaults to current)"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Wall-clock timeout in milliseconds (default 120000)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: &Value) -> Result<Value> {
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if command.trim().is_empty() {
            return Err(crate::ToolError::InvalidArgument(
                "empty command".to_string(),
            ));
        }
        let dir = args.get("directory").and_then(|v| v.as_str());
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .filter(|t| *t > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MS);

        if let Some(d) = dir {
            if !std::path::Path::new(d).is_dir() {
                return Err(crate::ToolError::Terminal(format!(
                    "directory does not exist: {d}"
                )));
            }
        }

        // Shell selection is platform-specific: `sh` does not exist on
        // stock Windows, and `cmd` does not exist on Unix.
        #[cfg(windows)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/C", command]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", command]);
            c
        };

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(d) = dir {
            cmd.current_dir(d);
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: child processes must not flash a console.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd
            .spawn()
            .map_err(|e| crate::ToolError::Terminal(format!("failed to spawn: {e}")))?;

        let output =
            match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
                .await
            {
                Ok(Ok(out)) => out,
                Ok(Err(e)) => return Err(crate::ToolError::Terminal(e.to_string())),
                Err(_) => {
                    // The timeout dropped the child (kill_on_drop ensures the
                    // process is terminated).
                    return Err(crate::ToolError::Terminal(format!(
                        "command timed out after {timeout_ms} ms and was killed"
                    )));
                }
            };

        let (stdout, stdout_truncated) = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let (stderr, stderr_truncated) = truncate_output(&String::from_utf8_lossy(&output.stderr));
        Ok(serde_json::json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": output.status.code().unwrap_or(-1),
            "timed_out": false,
            "stdout_truncated": stdout_truncated,
            "stderr_truncated": stderr_truncated
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_simple_command() {
        let tool = TerminalTool;
        let cmd = "echo hello";
        let result = tool
            .execute(&serde_json::json!({"command": cmd}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 0);
        let stdout = result["stdout"].as_str().unwrap().trim();
        assert_eq!(stdout, "hello");
    }

    #[tokio::test]
    async fn captures_nonzero_exit() {
        let tool = TerminalTool;
        let cmd = if cfg!(windows) { "exit /b 3" } else { "exit 3" };
        let result = tool
            .execute(&serde_json::json!({"command": cmd}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], 3);
    }

    #[tokio::test]
    async fn timeout_kills_stuck_command() {
        let tool = TerminalTool;
        let cmd = if cfg!(windows) {
            "ping -n 30 127.0.0.1"
        } else {
            "sleep 30"
        };
        let result = tool
            .execute(&serde_json::json!({"command": cmd, "timeout_ms": 500}))
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "expected timeout error, got: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_empty_command() {
        let tool = TerminalTool;
        let result = tool.execute(&serde_json::json!({"command": "   "})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rejects_missing_directory() {
        let tool = TerminalTool;
        let result = tool
            .execute(&serde_json::json!({
                "command": "echo hi",
                "directory": "Z:/definitely/not/here"
            }))
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("directory does not exist"));
    }

    #[test]
    fn truncate_marks_long_output() {
        let long = "x".repeat(MAX_OUTPUT_CHARS + 10);
        let (out, truncated) = truncate_output(&long);
        assert!(truncated);
        assert!(out.contains("output truncated"));
        assert!(out.chars().count() < MAX_OUTPUT_CHARS + 30);

        let (out2, truncated2) = truncate_output("short");
        assert_eq!(out2, "short");
        assert!(!truncated2);
    }
}
