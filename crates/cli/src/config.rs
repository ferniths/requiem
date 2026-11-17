//! TOML config loading.
//!
//! The config lives at `~/.requiem/config.toml`. Loading never writes:
//! [`Config::load`] reads the file and errors with
//! [`ConfigError::NotInitialized`] when it is missing, and
//! [`Config::load_or_init`] additionally materializes a default file on
//! very first run. A freshly created default is *not* marked initialized —
//! [`Config::is_initialized`] is what drives the setup wizard, so the
//! wizard actually launches on first run instead of being skipped.
//!
//! Migration: configs written before the `initialized` field existed
//! deserialize with `initialized = false`, so the wizard runs once more,
//! then flips the flag when saved.

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

    // ── First-run marker ────────────────────────────────────────
    /// Whether the user has completed the setup wizard. A config file
    /// existing on disk is NOT enough — the wizard writes this flag when
    /// the user confirms the final step.
    #[serde(default)]
    pub initialized: bool,
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

    /// Load the config from disk without writing anything.
    ///
    /// Returns [`ConfigError::NotInitialized`] when the config file does
    /// not exist yet.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path()?;
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ConfigError::NotInitialized)
            }
            Err(e) => return Err(ConfigError::Read(e.to_string())),
        };
        let config: Config = toml::from_str(&raw).map_err(|e| ConfigError::Parse(e.to_string()))?;
        Ok(config)
    }

    /// Load the config, writing a default file when none exists yet.
    ///
    /// The freshly written default is NOT initialized, so
    /// [`Config::is_initialized`] is false and the setup wizard launches
    /// on the next interactive run.
    pub fn load_or_init() -> Result<Self, ConfigError> {
        match Self::load() {
            Ok(config) => Ok(config),
            Err(ConfigError::NotInitialized) => {
                let config = Self::default();
                config.save()?;
                Ok(config)
            }
            Err(e) => Err(e),
        }
    }

    /// Persist the config to disk (creating parent directories).
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::Write(e.to_string()))?;
        }
        let raw =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        std::fs::write(&path, raw).map_err(|e| ConfigError::Write(e.to_string()))?;
        Ok(())
    }

    /// Whether the user has completed setup.
    ///
    /// This is the semantic flag written by the wizard — not a file-exists
    /// heuristic, which used to defeat the first-run wizard entirely (the
    /// mere act of loading created the file).
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Parse a config from raw TOML (used by the wizard to re-load an
    /// existing config defensively and by tests).
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        toml::from_str(raw).map_err(|e| ConfigError::Parse(e.to_string()))
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
            initialized: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot resolve home directory")]
    NoHomeDir,
    #[error("not initialized yet (no config file)")]
    NotInitialized,
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
    fn default_config_is_not_initialized() {
        // The wizard must launch on first run: a fresh default config is
        // never marked initialized.
        assert!(!Config::default().is_initialized());
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

    #[test]
    fn initialized_flag_survives_roundtrip() {
        let config = Config {
            initialized: true,
            model: "my-model".into(),
            ..Default::default()
        };
        let raw = toml::to_string_pretty(&config).unwrap();
        let parsed = Config::parse(&raw).unwrap();
        assert!(parsed.is_initialized());
        assert_eq!(parsed.model, "my-model");
    }

    #[test]
    fn legacy_config_without_flag_is_not_initialized() {
        // A config written before the field existed: wizard re-runs once.
        let legacy = r#"
model = "old-model"
endpoint = "http://localhost:8080/v1"
"#;
        let parsed = Config::parse(legacy).unwrap();
        assert_eq!(parsed.model, "old-model");
        assert!(!parsed.is_initialized());
    }

    #[test]
    fn api_key_is_roundtripped_but_never_logged() {
        let config = Config {
            api_key: "sk-super-secret".into(),
            ..Default::default()
        };
        let raw = toml::to_string_pretty(&config).unwrap();
        let parsed = Config::parse(&raw).unwrap();
        assert_eq!(parsed.api_key, "sk-super-secret");
    }

    #[test]
    fn vaults_roundtrip_in_order() {
        let config = Config {
            vaults: vec!["C:/notes/main".into(), "C:/notes/work".into()],
            initialized: true,
            ..Default::default()
        };
        let raw = toml::to_string_pretty(&config).unwrap();
        let parsed = Config::parse(&raw).unwrap();
        assert_eq!(parsed.vaults.len(), 2);
        assert_eq!(parsed.vaults[0], "C:/notes/main");
    }
}
