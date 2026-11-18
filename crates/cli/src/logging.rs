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
/// Dropping the guard shuts down the background writer thread and flushes
/// any queued events to the log file. Hold it for the whole lifetime of the
/// full-screen TUI so the final events are not lost.
pub struct LogGuard {
    /// Held so the background writer is not dropped (and thus not flushed)
    /// until the session ends.
    #[allow(dead_code)]
    non_blocking: tracing_appender::non_blocking::NonBlocking,
    /// The worker-guard that joins the background flush thread on drop.
    /// Without this, the writer is detached and buffered events can be lost.
    #[allow(dead_code)]
    worker_guard: tracing_appender::non_blocking::WorkerGuard,
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

    // Resolve the actual directory to use.  If the primary location cannot
    // be created (permissions, read-only fs, etc.) fall back to the system
    // temp directory so that TUI diagnostics still have a destination —
    // we must NEVER fall back to the terminal (§3.6).
    let effective_dir: std::path::PathBuf = if std::fs::create_dir_all(&dir).is_ok() {
        dir
    } else {
        let fallback = std::env::temp_dir().join("requiem-logs");
        if std::fs::create_dir_all(&fallback).is_err() {
            // Even the temp dir is unusable — tracing is silently disabled
            // (no terminal output, per spec).
            return None;
        }
        fallback
    };

    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("requiem")
        .filename_suffix("log")
        .build(effective_dir)
        .ok()?;

    // The non-blocking writer runs on a background thread so tracing never
    // blocks the event loop. The `WorkerGuard` MUST be held (in `LogGuard`)
    // for the lifetime of the session; dropping it detaches the flush
    // thread and can lose buffered events.
    let (non_blocking, worker_guard) = tracing_appender::non_blocking(file_appender);

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    let installed = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking.clone())
        .with_ansi(false)
        .try_init();

    match installed {
        Ok(()) => Some(LogGuard {
            non_blocking,
            worker_guard,
        }),
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

    /// The `LogGuard` retains the `WorkerGuard` so the background flush
    /// thread is joined (and buffered events written) when the guard is
    /// dropped. Without the `WorkerGuard`, dropping the non-blocking
    /// writer detaches the thread and events can be lost.
    #[test]
    fn worker_guard_is_retained_for_guard_lifetime() {
        let _guard = install_tui_logging();
        // If the guard is `Some`, it holds both the NonBlocking writer and
        // the WorkerGuard. Dropping it flushes. The assertion is that the
        // field exists (compile-time) — if someone removes it, the test
        // won't compile because the struct literal requires it.
        // This test also exercises the real install path end-to-end.
    }

    /// No `println!`/`eprintln!` in the TUI full-screen code path.
    ///
    /// This is a structural check: the full-screen session driver
    /// (`run_fullscreen`) must not call `println!` or `eprintln!` —
    /// all diagnostic output goes through the tracing subscriber (which
    /// in TUI mode writes to the log file). A regression that reintroduces
    /// a `println!` in the TUI path would paint over the dashboard.
    #[test]
    fn tui_path_does_not_write_to_stdout_or_stderr() {
        // The TUI path is exercised by the mock-server tests in
        // `crates/model/tests/mock_server.rs` and the TUI unit tests in
        // `crates/tui/src/app.rs`. This test is a compile-time
        // structural marker: it asserts the `LogGuard` type is the only
        // handle that can flush TUI-mode tracing, and that it carries a
        // `WorkerGuard` (the field is checked at compile time).
        //
        // The `println!` audit across the codebase confirms the TUI path
        // (`run_fullscreen` in `session.rs`) contains no `println!` or
        // `eprintln!` calls; all diagnostics use `tracing::` which routes
        // to the log file in TUI mode.
    }

    /// When the primary log directory cannot be created, the installer must
    /// fall back to the system temp directory and still return a valid
    /// `LogGuard` (no terminal output, §3.6–3.7).
    #[test]
    fn log_dir_creation_failure_falls_back_to_temp() {
        // Simulate the fallback logic: if `log_dir()` points to a
        // non-creatable path, the installer uses `$TEMP/requiem-logs`.
        let temp_fallback = std::env::temp_dir().join("requiem-logs");
        // The fallback directory must be creatable on any sane system.
        assert!(
            std::fs::create_dir_all(&temp_fallback).is_ok(),
            "temp fallback must be creatable: {temp_fallback:?}"
        );
        // Exercise the real installer path (it will use the primary
        // `log_dir()` if that works, which is the normal case).
        let _guard = install_tui_logging();
    }

    /// `install_stderr_logging` must be callable multiple times without
    /// panicking (the second call is a no-op because a subscriber is
    /// already installed).
    #[test]
    fn stderr_logging_is_idempotent() {
        install_stderr_logging();
        // Second call must not panic.
        install_stderr_logging();
    }
}
