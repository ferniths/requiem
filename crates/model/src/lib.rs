//! The 9router model client.
//!
//! This crate provides the ONLY model-access layer in REQUIEM. Every agent,
//! at every nesting depth, routes requests through the same configured
//! model and endpoint. The client implements:
//!
//! - streaming chat completions
//! - tool / function calling
//! - structured error classification (retryable vs not)
//! - exponential-backoff retries
//! - request-timeout handling
//! - token-usage reporting (reported vs estimated)
//!
//! It is OpenAI-compatible: it speaks the `POST /v1/chat/completions`
//! protocol. The endpoint and model are user-configured — never hardcoded.

pub mod client;
pub mod config;
pub mod request;
pub mod response;
pub mod retry;

pub use client::ModelClient;
pub use config::ModelConfig;
pub use request::{ChatMessage, Role as MsgRole, ToolCall, ToolDef};
pub use response::{ModelResponse, TokenUsage};
pub use retry::RetryPolicy;
