//! TOML config loading with first-run setup.
//!
//! On first run, a default config is written to `~/.requiem/config.toml`.
//! On subsequent runs, the existing config is loaded (never overwritten).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The root config file for REQUIEM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The model to use for all agents.
    pub model: String,
    /// The OpenAI-compatible API endpoint (e.g. `http://localhost:11434/v1`).
    pub endpoint: String,
    /// Optional API key (can also come from env var `REQUIEM_API_KEY`).
    #[serde(default)]
    pub api_key: String,
    /// Max concurrent requests to the model.
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: u32,
    /// Request timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Max retries on transient failures.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// The max context window in tokens for the model.
    #[serde(default = "default_max_tokens")]
    pub max_context_tokens: u32,

    // ── Agent limits ────────────────────────────────────────────
    /// Maximum depth of the agent hierarchy (master → worker → subagent…).
    #[serde(default = "default_agent_depth")]
    pub max_agent_depth: u32,
    /// Default token budget per agent task.
    #[serde(default = "default_token_budget")]
    pub token_budget: u32,
    /// Maximum number of tasks a single session may queue.
    #[serde(default = "default_max_task_budget")]
    pub max_task_budget: u32,
    /// Whether agents can dynamically scale up/down based on load.
    #[serde(default)]
    pub dynamic_scaling: bool,

    // ── UI / appearance ─────────────────────────────────────────
    /// The theme variant: "default", "high_contrast", or "muted".
    #[serde(default = "default_theme")]
    pub theme: String,
    /// The default operating mode when a session starts.
    #[serde(default = "default_mode")]
    pub default_mode: String,

    // ── Obsidian vaults ─────────────────────────────────────────
    /// Paths to Obsidian vaults. The first entry is the default vault.
    #[serde(default)]
    pub vaults: Vec<String>,
}

fn default_max_concurrency() -> u32 {
    4
}
fn default_timeout_ms() -> u64 {
    120_000
}
fn default_max_retries() -> u32 {
    3
}
fn default_max_tokens() -> u32 {
    128_000
}
fn default_agent_depth() -> u32 {
    4
}
fn default_token_budget() -> u32 {
    50_000
}
fn default_max_task_budget() -> u32 {
    20
}
fn default_theme() -> String {
    "default".into()
}
fn default_mode() -> String {
    "code".into()
}

impl Config {
    /// The path where the config file lives.
    pub fn config_path() -> Result<PathBuf, ConfigError> {
        let home = dirs::home_dir().ok_or(ConfigError::NoHomeDir)?;
        Ok(home.join(".requiem").join("config.toml"))
    }

    /// Load the config, writing a default file on first run.
    pub fn load_or_init() -> Result<Self, ConfigError> {
        let path = Self::config_path()?;
        if path.is_file() {
            let raw =
                std::fs::read_to_string(&path).map_err(|e| ConfigError::Read(e.to_string()))?;
            let config: Config =
                toml::from_str(&raw).map_err(|e| ConfigError::Parse(e.to_string()))?;
            Ok(config)
        } else {
            let config = Self::default();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ConfigError::Write(e.to_string()))?;
            }
            let raw = toml::to_string_pretty(&config)
                .map_err(|e| ConfigError::Serialize(e.to_string()))?;
            std::fs::write(&path, raw).map_err(|e| ConfigError::Write(e.to_string()))?;
            Ok(config)
        }
    }

    /// Whether the config has been initialized (endpoint and model are set
    /// to non-default values, or a config file exists on disk).
    pub fn is_initialized(&self) -> bool {
        let path = Self::config_path().ok();
        if let Some(p) = &path {
            if p.is_file() {
                return true;
            }
        }
        // Also consider it initialized if the endpoint is not the default.
        self.endpoint != "http://localhost:11434/v1" || self.model != "gpt-4o"
    }

    /// Build a `ModelConfig` from this config.
    pub fn to_model_config(&self) -> requiem_model::config::ModelConfig {
        let api_key = if !self.api_key.is_empty() {
            self.api_key.clone()
        } else {
            std::env::var("REQUIEM_API_KEY").unwrap_or_default()
        };
        requiem_model::config::ModelConfig {
            model: self.model.clone(),
            endpoint: self.endpoint.clone(),
            api_key,
            max_concurrency: self.max_concurrency,
            timeout_ms: self.timeout_ms,
            max_retries: self.max_retries,
            max_context_tokens: Some(self.max_context_tokens),
            max_output_tokens: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "gpt-4o".to_string(),
            endpoint: "http://localhost:11434/v1".to_string(),
            api_key: String::new(),
            max_concurrency: 4,
            timeout_ms: 120_000,
            max_retries: 3,
            max_context_tokens: 128_000,
            max_agent_depth: 4,
            token_budget: 50_000,
            max_task_budget: 20,
            dynamic_scaling: false,
            theme: "default".to_string(),
            default_mode: "code".to_string(),
            vaults: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot resolve home directory")]
    NoHomeDir,
    #[error("failed to read config: {0}")]
    Read(String),
    #[error("failed to parse config: {0}")]
    Parse(String),
    #[error("failed to write config: {0}")]
    Write(String),
    #[error("failed to serialize config: {0}")]
    Serialize(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_reasonable_values() {
        let config = Config::default();
        assert_eq!(config.max_concurrency, 4);
        assert_eq!(config.timeout_ms, 120_000);
        assert!(!config.model.is_empty());
    }

    #[test]
    fn to_model_config_resolves_api_key_from_env() {
        let config = Config {
            api_key: String::new(),
            ..Default::default()
        };
        let mc = config.to_model_config();
        assert!(mc.api_key.is_empty()); // no env var set in test
    }
}
