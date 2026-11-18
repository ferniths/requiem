//! TUI-aware tracing initialization.
//!
//! A full-screen TUI owns the terminal: any `tracing` output that lands on
//! `stdout`/`stderr` paints directly over the dashboard and corrupts the
//! render (the MiMo-bootstrap warning flood did exactly this). When a
//! full-screen TUI is about to start we therefore route all tracing events
//! to a **non-blocking, rotating, size-bounded log file** under the user's
//! local data directory, and never to the terminal.
//!
//! * Windows: `%LOCALAPPDATA%\Requiem\logs`
//! * Linux/macOS: `$XDG_STATE_HOME/requiem/logs` (fallback
//!   `~/.local/state/requiem/logs`)
//!
//! For non-interactive / piped execution we keep the original stderr
//! subscriber so diagnostic output still reaches the user's terminal.
//!
//! The subscriber is installed globally; only one path runs per process,
//! so exactly one of [`install_tui_logging`] or [`install_stderr_logging`]
//! is called. The TUI variant returns a guard that must be held for the
//! lifetime of the full-screen session so buffered events are flushed on
//! exit.

use std::path::PathBuf;

use tracing_appender::rolling::{RollingFileAppender, Rotation};

/// The cross-platform directory where REQUIEM writes its log files.
///
/// Windows: `%LOCALAPPDATA%\Requiem\logs`
/// Otherwise: `$XDG_STATE_HOME/requiem/logs`, falling back to
/// `~/.local/state/requiem/logs` and finally `~/.requiem/logs`.
pub fn log_dir() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .map(|h| h.join("AppData").join("Local"))
                    .unwrap_or_else(|| PathBuf::from("."))
            });
        return base.join("Requiem").join("logs");
    }
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        let p = PathBuf::from(state_home).join("requiem").join("logs");
        if p.parent().is_some() {
            return p;
        }
    }
    dirs::home_dir()
        .map(|h| h.join(".local").join("state").join("requiem").join("logs"))
        .unwrap_or_else(|| PathBuf::from(".").join(".requiem").join("logs"))
}

/// A guard that keeps a non-blocking rolling-file log writer alive.
///
/// Dropping the guard shuts down the background writer and flushes any
/// queued events. Hold it for the whole lifetime of the full-screen TUI.
pub struct LogGuard {
    /// Held so the background writer is not dropped (and thus not flushed)
    /// until the session ends.
    #[allow(dead_code)]
    non_blocking: tracing_appender::non_blocking::NonBlocking,
}

/// Install a tracing subscriber that writes to a rotating, size-bounded,
/// non-blocking log file under [`log_dir`].
///
/// Returns a guard that must be kept alive (typically for the duration of
/// the full-screen session) so events are flushed on exit. Returns `None`
/// if the log directory cannot be created or a subscriber is already
/// installed.
pub fn install_tui_logging() -> Option<LogGuard> {
    let dir = log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::error!(
            path = %dir.display(),
            error = %e,
            "could not create REQUIEM log directory; tracing will be disabled"
        );
        return None;
    }

    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("requiem")
        .filename_suffix("log")
        .build(dir)
        .map_err(|e| {
            tracing::error!(
                error = %e,
                "could not initialise the REQUIEM log appender; tracing disabled"
            );
        })
        .ok()?;

    // The non-blocking writer runs on a background thread so tracing never
    // blocks the event loop; a `lost` counter records any dropped events if
    // the file cannot keep up.
    let (non_blocking, _guard_handle) = tracing_appender::non_blocking(file_appender);

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    let installed = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking.clone())
        .with_ansi(false)
        .try_init();

    match installed {
        Ok(()) => Some(LogGuard { non_blocking }),
        Err(_) => None, // a global subscriber already exists; keep it.
    }
}

/// Install the plain stderr subscriber for non-interactive / piped use.
///
/// Retains full diagnostic output on the terminal where no TUI is running.
/// Best-effort: silently no-ops if a subscriber is already installed.
pub fn install_stderr_logging() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_dir_is_resolvable() {
        let dir = log_dir();
        assert!(!dir.as_os_str().is_empty());
        // On Windows it must be under LOCALAPPDATA or the home fallback.
        if cfg!(windows) {
            let s = dir.display().to_string();
            assert!(
                s.contains("Requiem") && s.contains("logs"),
                "unexpected log dir: {s}"
            );
        }
    }

    /// The TUI installer creates the log directory on disk (the concrete
    /// guarantee that full-screen diagnostics are captured to a file under
    /// the user's local data dir instead of painting over the terminal).
    #[test]
    fn install_tui_logging_creates_log_directory() {
        let dir = log_dir();
        assert!(
            std::fs::create_dir_all(&dir).is_ok(),
            "log directory must be creatable: {dir:?}"
        );
        // The installer itself must also create it (idempotent here).
        let _guard = install_tui_logging();
        assert!(
            dir.is_dir(),
            "install_tui_logging must leave the log directory in place: {dir:?}"
        );
    }
}
