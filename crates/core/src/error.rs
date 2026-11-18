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

    /// Credentials are missing, expired, or invalid.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// The API key is not configured.
    #[error("no API key configured")]
    MissingCredentials,

    /// DNS resolution failed.
    #[error("dns failure: {0}")]
    Dns(String),

    /// A TLS handshake or certificate verification failed.
    #[error("tls failure: {0}")]
    Tls(String),

    /// The model name is not available at the configured endpoint.
    #[error("model unavailable: {0}")]
    ModelUnavailable(String),

    /// The provider reports a quota or billing limit has been hit.
    #[error("quota exhausted: {0}")]
    QuotaExhausted(String),

    /// The provider's tool-calling schema is incompatible with the request.
    #[error("tool-calling error: {0}")]
    ToolCallingIncompatible(String),

    /// A failure occurred mid-stream after the HTTP connection was
    /// established (SSE dropped, malformed chunk, etc.).
    #[error("streaming failure: {0}")]
    StreamingFailure(String),

    /// An upstream gateway (load balancer, proxy) returned an error.
    #[error("gateway failure [{status}]: {message}")]
    GatewayFailure { status: u16, message: String },

    /// The provider circuit breaker is open: repeated permanent failures
    /// have tripped it and new requests are refused until a cooldown
    /// elapses. Not retryable — retrying just re-trips the breaker.
    #[error("provider circuit open: {0}")]
    CircuitOpen(String),

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
    ///
    /// TLS/certificate failures, malformed requests, bad credentials and
    /// invalid configuration are permanent: retrying them forever only
    /// delays the actionable error. Only genuinely transient transport and
    /// upstream conditions are retried (the retry policy additionally caps
    /// the attempt count).
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Connection(_)
            | Self::Dns(_)
            | Self::RateLimited { .. }
            | Self::Timeout(_)
            | Self::GatewayFailure { .. } => true,
            // A tripped circuit breaker is a *decision*, not a transient
            // fault: retrying immediately would just re-trip it. The
            // breaker's own cooldown/half-open logic handles recovery.
            Self::CircuitOpen(_) => false,
            // A stream that failed after data was delivered cannot be
            // transparently replayed without duplicating output.
            Self::StreamingFailure(_) => false,
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
            Self::Auth(_) => "auth",
            Self::MissingCredentials => "missing_credentials",
            Self::Dns(_) => "dns",
            Self::Tls(_) => "tls",
            Self::ModelUnavailable(_) => "model_unavailable",
            Self::QuotaExhausted(_) => "quota_exhausted",
            Self::ToolCallingIncompatible(_) => "tool_calling",
            Self::StreamingFailure(_) => "streaming",
            Self::GatewayFailure { .. } => "gateway",
            Self::CircuitOpen(_) => "circuit_open",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_are_retryable() {
        assert!(RequiemError::Connection("x".into()).is_retryable());
        assert!(RequiemError::Dns("x".into()).is_retryable());
        assert!(RequiemError::Timeout(10).is_retryable());
        assert!(RequiemError::RateLimited { retry_after_ms: 1 }.is_retryable());
        assert!(RequiemError::GatewayFailure {
            status: 502,
            message: "x".into()
        }
        .is_retryable());
        assert!(RequiemError::Model {
            status: Some(503),
            message: "x".into()
        }
        .is_retryable());
    }

    #[test]
    fn permanent_errors_are_not_retryable() {
        // A TLS certificate failure will not fix itself by retrying.
        assert!(!RequiemError::Tls("certificate".into()).is_retryable());
        assert!(!RequiemError::Auth("bad key".into()).is_retryable());
        assert!(!RequiemError::MissingCredentials.is_retryable());
        assert!(!RequiemError::Config("bad".into()).is_retryable());
        assert!(!RequiemError::ModelUnavailable("m".into()).is_retryable());
        assert!(!RequiemError::QuotaExhausted("q".into()).is_retryable());
        assert!(!RequiemError::ToolCallingIncompatible("t".into()).is_retryable());
        assert!(!RequiemError::InvalidResponse("r".into()).is_retryable());
        assert!(!RequiemError::StreamingFailure("s".into()).is_retryable());
        assert!(!RequiemError::ContextLength {
            requested: 1,
            limit: Some(1)
        }
        .is_retryable());
    }

    #[test]
    fn classes_are_stable() {
        assert_eq!(RequiemError::Dns("x".into()).class(), "dns");
        assert_eq!(RequiemError::Tls("x".into()).class(), "tls");
        assert_eq!(
            RequiemError::GatewayFailure {
                status: 502,
                message: "x".into()
            }
            .class(),
            "gateway"
        );
        assert_eq!(
            RequiemError::StreamingFailure("x".into()).class(),
            "streaming"
        );
    }
}
