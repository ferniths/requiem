//! Model response parsing and token-usage reporting.

use serde::{Deserialize, Serialize};

use crate::request::{ChatMessage, ToolCall};
use requiem_core::token::CountKind;

/// Token usage for a single model call.
///
/// `kind` distinguishes API-reported counts from client-side estimates so
/// the UI never presents an estimate as a fact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    /// Input tokens consumed by this call.
    pub input: u32,
    /// Output tokens produced by this call.
    pub output: u32,
    /// Whether these numbers are authoritative or estimated.
    pub kind: CountKind,
}

impl TokenUsage {
    /// An empty usage (no tokens).
    pub fn empty() -> Self {
        Self::default()
    }

    /// The reported or estimated total.
    pub fn total(&self) -> u32 {
        self.input + self.output
    }

    /// Whether this usage carries real API-reported counts.
    pub fn is_reported(&self) -> bool {
        self.kind == CountKind::Reported
    }
}

/// The accumulated response for a single (possibly streaming) model call.
///
/// For a streaming call, `finish_reason` is set only on the final chunk.
/// For a non-streaming call it is set immediately.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelResponse {
    /// The fully-assembled assistant message.
    pub message: ChatMessage,
    /// Why the model stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Token usage, if the API reported it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    /// The model that actually served the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl ModelResponse {
    /// Convenience: whether this response requested a tool call.
    pub fn has_tool_calls(&self) -> bool {
        !self.message.tool_calls.is_empty()
    }

    /// The tool calls, if any.
    pub fn tool_calls(&self) -> &[ToolCall] {
        &self.message.tool_calls
    }

    /// The assistant text content, if present.
    pub fn text(&self) -> Option<&str> {
        match &self.message.content {
            Some(serde_json::Value::String(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Whether this response hit a context-length error.
    pub fn is_context_error(&self) -> bool {
        matches!(self.finish_reason.as_deref(), Some("length"))
    }
}

/// A single chunk from a streaming response.
///
/// The 9router/OpenAI streaming protocol sends one JSON object per chunk:
/// `{"choices":[{"delta":{"content":"..."}}], "usage":{...}}`
/// The final chunk carries the `usage` field.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StreamChunk {
    /// Incremental content for this chunk.
    pub content: Option<String>,
    /// Incremental tool-call delta.
    pub tool_call_delta: Option<ToolCallDelta>,
    /// The finish reason, present only on the final chunk.
    pub finish_reason: Option<String>,
    /// Token usage, present only on the final chunk.
    pub usage: Option<TokenUsage>,
}

/// An incremental update to a tool call during streaming.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCallDelta {
    /// The tool call index in the response.
    pub index: u32,
    /// The call id (usually only on the first delta).
    pub id: Option<String>,
    /// The tool name (usually only on the first delta).
    pub name: Option<String>,
    /// A partial argument string.
    pub arguments: Option<String>,
}
