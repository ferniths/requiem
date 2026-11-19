//! Terminal command execution tool.
//!
//! Cross-platform: on Windows commands run through `cmd /C` (in a new
//! process group, terminated via the group ID), on Unix through `sh -c`
//! (in a new process group, terminated via the group PID). Descendant
//! processes are killed together with the shell so a stuck `make` or
//! `cargo build` cannot orphan child processes.

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

/// Read a pipe with a hard byte cap: accumulate up to `max_bytes`, then
/// drain the rest of the stream without further allocation. This keeps
/// memory bounded even when a command produces gigabytes of output.
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(
    mut pipe: R,
    max_bytes: usize,
) -> (String, bool) {
    use tokio::io::AsyncReadExt;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let mut truncated = false;
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() >= max_bytes {
                    truncated = true;
                    // Drain the remainder without retaining it so the
                    // pipe's kernel buffer can empty and the child can
                    // finish writing (avoids a deadlock on a full pipe).
                    while pipe.read(&mut chunk).await.map(|n| n > 0).unwrap_or(false) {}
                    break;
                }
                // Only retain the portion that fits under the cap; the
                // rest of this chunk is discarded, and the remainder of
                // the stream is drained so the pipe can empty.
                let room = max_bytes - buf.len();
                if n > room {
                    buf.extend_from_slice(&chunk[..room]);
                    truncated = true;
                    // Drain the rest of the stream (the unread portion of
                    // this chunk is already in memory and can be dropped;
                    // the pipe still has more data to flush).
                    while pipe.read(&mut chunk).await.map(|n| n > 0).unwrap_or(false) {}
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    (text, truncated)
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
            // CREATE_NEW_PROCESS_GROUP: cmd and its descendants form a
            // separate process group so we can kill the whole tree.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        }
        #[cfg(unix)]
        unsafe {
            // Put the shell in its own process group: PID == PGID.
            // This lets us signal the whole group (kill -PGID) rather
            // than just the shell process.
            cmd.pre_exec(move || {
                if libc::setgid(0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::setsid();
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| crate::ToolError::Terminal(format!("failed to spawn: {e}")))?;

        let started = std::time::Instant::now();

        // Save the PID before it's consumed by the async read block.
        let pid = match child.id() {
            Some(p) => p,
            None => {
                let _ = child.kill().await;
                return Err(crate::ToolError::Terminal(
                    "failed to get child PID".to_string(),
                ));
            }
        };

        // Take the pipes out so we can read them independently of child.wait().
        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();

        // Read both streams concurrently with a hard byte cap, then wait
        // for the child. The cap keeps memory bounded even when a command
        // produces gigabytes of output; the drain loop prevents deadlock
        // on a full pipe.
        let cap = MAX_OUTPUT_CHARS;
        let read_fut = async {
            let (so_result, se_result) = tokio::join!(
                async {
                    if let Some(p) = stdout_pipe.take() {
                        read_capped(p, cap).await
                    } else {
                        (String::new(), false)
                    }
                },
                async {
                    if let Some(p) = stderr_pipe.take() {
                        read_capped(p, cap).await
                    } else {
                        (String::new(), false)
                    }
                }
            );
            let status = child.wait().await;
            (so_result, se_result, status)
        };

        let ((stdout_raw, stdout_truncated), (stderr_raw, stderr_truncated), status) =
            match tokio::time::timeout(Duration::from_millis(timeout_ms), read_fut).await {
                Ok(((so_raw, _so_trunc), (se_raw, _se_trunc), status)) => {
                    // The capped reader already enforces the byte limit;
                    // apply the display truncation (head + ellipsis) for
                    // the model-facing result.
                    let so = truncate_output(&so_raw);
                    let se = truncate_output(&se_raw);
                    (so, se, status)
                }
                Err(_) => {
                    // Timeout: kill the whole process group (not just the
                    // shell) so descendant make/cargo/node processes are
                    // not orphaned.
                    #[cfg(unix)]
                    {
                        // Signal the entire process group: kill(-pid, SIGTERM)
                        // then SIGKILL after a short grace period.
                        unsafe {
                            libc::kill(-pid as i32, libc::SIGTERM);
                        }
                        // Give the group a moment to exit gracefully.
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        unsafe {
                            libc::kill(-pid as i32, libc::SIGKILL);
                        }
                    }
                    #[cfg(windows)]
                    {
                        // On Windows, kill the process group created with
                        // CREATE_NEW_PROCESS_GROUP. The group ID equals
                        // the process ID for the shell process.
                        //
                        // taskkill /T /F kills the entire process tree.
                        let _ = tokio::process::Command::new("taskkill")
                            .args(["/PID", &pid.to_string(), "/T", "/F"])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status()
                            .await;
                    }
                    return Err(crate::ToolError::Terminal(format!(
                        "command timed out after {timeout_ms} ms and was killed"
                    )));
                }
            };

        let status = status.map_err(|e| crate::ToolError::Terminal(e.to_string()))?;
        let exit_code = status.code().unwrap_or(-1);
        let duration_ms = started.elapsed().as_millis() as u64;

        Ok(serde_json::json!({
            "stdout": stdout_raw,
            "stderr": stderr_raw,
            "exit_code": exit_code,
            "timed_out": false,
            "duration_ms": duration_ms,
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
    async fn result_includes_duration_ms() {
        let tool = TerminalTool;
        let result = tool
            .execute(&serde_json::json!({"command": "echo hi"}))
            .await
            .unwrap();
        assert!(
            result["duration_ms"].is_u64(),
            "duration_ms must be present and numeric, got: {}",
            result["duration_ms"]
        );
        assert!(
            result["timed_out"].as_bool() == Some(false),
            "non-timed-out result must report timed_out=false"
        );
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

    /// Regression: output larger than the cap must not accumulate beyond
    /// `MAX_OUTPUT_CHARS` bytes in memory. The capped reader stops
    /// retaining after the cap and drains the rest, so the child can
    /// finish writing without deadlocking on a full pipe.
    #[tokio::test]
    async fn read_capped_enforces_byte_limit() {
        // Build a synthetic source that yields MAX_OUTPUT_CHARS + 1000
        // bytes, then EOF. `tokio_test::io::Builder` gives us an
        // `AsyncRead` that yields exactly the bytes we queue.
        let payload: Vec<u8> = vec![b'a'; MAX_OUTPUT_CHARS + 1000];
        let source = tokio_test::io::Builder::new().read(&payload).build();

        let (text, truncated) = read_capped(source, MAX_OUTPUT_CHARS).await;
        assert!(
            truncated,
            "reader must report truncation when payload exceeds the cap"
        );
        // The retained text must not exceed the cap (plus a small margin
        // for the UTF-8 lossy conversion of a clean ASCII payload).
        assert!(
            text.len() <= MAX_OUTPUT_CHARS,
            "retained bytes ({}) must not exceed the cap ({})",
            text.len(),
            MAX_OUTPUT_CHARS
        );
        let _ = payload;
    }
}
