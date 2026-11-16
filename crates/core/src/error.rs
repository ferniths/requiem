//! Central error types for REQUIEM.

use thiserror::Error;

/// The unified error type for all REQUIEM operations.
///
/// Modules return this type (wrapped in `anyhow::Result` at boundaries where
/// context is useful). The taxonomy is deliberately small so that recovery
/// logic (retries, reassignment, blockers) can match on it.
#[derive(Debug, Error)]
pub enum RequiemError {
    /// The configured 9router endpoint was unreachable.
    #[error("connection failed: {0}")]
    Connection(String),

    /// The model returned an HTTP or protocol error.
    #[error("model error [{}]: {}", status.map(|s| s.to_string()).unwrap_or_else(|| "-".into()), message)]
    Model {
        /// HTTP status code, if known.
        status: Option<u16>,
        /// Human-readable error body or status line.
        message: String,
    },

    /// The model returned a response that could not be parsed or did not
    /// match the expected shape (e.g. tool-call arguments not valid JSON).
    #[error("invalid model response: {0}")]
    InvalidResponse(String),

    /// The request exceeded the model's context window.
    #[error("context length exceeded (requested {requested}, limit {limit:?})")]
    ContextLength {
        /// Number of tokens that were in the request.
        requested: u32,
        /// The limit reported by the provider, if any.
        limit: Option<u32>,
    },

    /// A rate limit was hit; the caller should back off and retry.
    #[error("rate limited; retry in {retry_after_ms}ms")]
    RateLimited {
        /// Server-reported retry window, or an estimate.
        retry_after_ms: u64,
    },

    /// An operation timed out.
    #[error("timeout after {0}ms")]
    Timeout(u64),

    /// The task graph or agent state is inconsistent.
    #[error("invalid state: {0}")]
    InvalidState(String),

    /// A permission policy blocked the requested tool call.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// A filesystem or I/O failure.
    #[error("io: {0}")]
    Io(String),

    /// A structured error from a tool's own execution.
    #[error("tool error ({tool}): {message}")]
    Tool {
        /// The tool that failed.
        tool: String,
        /// The tool's error message.
        message: String,
    },

    /// The session could not be loaded or saved.
    #[error("session error: {0}")]
    Session(String),

    /// Configuration is missing or invalid.
    #[error("configuration error: {0}")]
    Config(String),

    /// An Obsidian vault operation failed.
    #[error("vault error: {0}")]
    Vault(String),

    /// Cancellation was requested.
    #[error("cancelled")]
    Cancelled,
}

impl RequiemError {
    /// Whether this error class is worth retrying (transient).
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Connection(_) | Self::RateLimited { .. } | Self::Timeout(_) => true,
            Self::Model { status, .. } => status.is_some_and(|s| s >= 500 || s == 429),
            _ => false,
        }
    }

    /// A short, machine-oriented classification for token accounting and
    /// failure loops.
    pub fn class(&self) -> &'static str {
        match self {
            Self::Connection(_) => "connection",
            Self::Model { .. } => "model",
            Self::InvalidResponse(_) => "invalid_response",
            Self::ContextLength { .. } => "context_length",
            Self::RateLimited { .. } => "rate_limited",
            Self::Timeout(_) => "timeout",
            Self::InvalidState(_) => "invalid_state",
            Self::PermissionDenied(_) => "permission",
            Self::Io(_) => "io",
            Self::Tool { .. } => "tool",
            Self::Session(_) => "session",
            Self::Config(_) => "config",
            Self::Vault(_) => "vault",
            Self::Cancelled => "cancelled",
        }
    }
}
impl From<serde_json::Error> for RequiemError {
    fn from(e: serde_json::Error) -> Self {
        Self::InvalidResponse(e.to_string())
    }
}
