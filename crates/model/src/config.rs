//! Model configuration.
//!
//! All agents share exactly one `ModelConfig`. The endpoint, API key, and
//! model identifier are the *only* model-level settings; the harness never
//! substitutes a different model when a request fails.

use serde::{Deserialize, Serialize};

/// The single, globally configured model route.
///
/// This is the canonical model the entire REQUIEM runtime uses. It is loaded
/// from the TOML config file and/or environment variables; it is never
/// invented at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// The base URL of the 9router endpoint.
    ///
    /// Must point at an API root that exposes `/v1/chat/completions`.
    /// A dashboard URL is NOT a valid endpoint — users must configure the
    /// API base explicitly.
    pub endpoint: String,
    /// The API key / bearer token.
    ///
    /// May also be supplied via the `REQUIEM_API_KEY` environment variable,
    /// which takes precedence over the value stored here.
    pub api_key: String,
    /// The model identifier (e.g. `"gpt-4o"`, `"claude-3-5-sonnet"`,
    /// or a 9router route name).
    pub model: String,
    /// Per-request timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Maximum number of concurrent in-flight requests to the model.
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,
    /// Maximum retry attempts for a single logical request.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Optional context-window cap the harness enforces client-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
    /// Optional output-token cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

fn default_timeout_ms() -> u64 {
    120_000
}
fn default_concurrency() -> u32 {
    4
}
fn default_max_retries() -> u32 {
    3
}

impl ModelConfig {
    /// The fully-resolved API key after consulting the environment.
    ///
    /// If `REQUIEM_API_KEY` is set, it wins over the configured value. This
    /// keeps secrets out of the TOML file on shared machines.
    pub fn resolved_api_key(&self) -> String {
        std::env::var("REQUIEM_API_KEY").unwrap_or_else(|_| self.api_key.clone())
    }

    /// Validate the configuration before starting the runtime.
    ///
    /// Returns a human-readable error if the config is unusable.
    pub fn validate(&self) -> Result<(), String> {
        if self.endpoint.trim().is_empty() {
            return Err("model.endpoint must be a non-empty URL".into());
        }
        if !self.endpoint.starts_with("http://") && !self.endpoint.starts_with("https://") {
            return Err("model.endpoint must start with http:// or https://".into());
        }
        if self.model.trim().is_empty() {
            return Err("model.model must be a non-empty identifier".into());
        }
        if self.max_concurrency == 0 {
            return Err("model.max_concurrency must be at least 1".into());
        }
        Ok(())
    }

    /// Build a chat-completions URL from this config.
    pub fn completions_url(&self) -> String {
        let trimmed = self.endpoint.trim_end_matches('/');
        if trimmed.ends_with("/v1") {
            format!("{trimmed}/chat/completions")
        } else {
            format!("{trimmed}/v1/chat/completions")
        }
    }
}
