//! The 9router model client.
//!
//! Owns an HTTP client, enforces the retry policy, and exposes streaming
//! and non-streaming chat completions. All agents share a single
//! `ModelClient` (behind an `Arc`) so concurrency is bounded and token
//! accounting is centralized.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::sleep;

use crate::config::ModelConfig;
use crate::request::{ChatMessage, ChatRequest};
use crate::response::{ModelResponse, StreamChunk, TokenUsage, ToolCallDelta};
use crate::retry::{from_max_retries, RetryPolicy};
use requiem_core::error::RequiemError;

/// A shared, concurrency-bounded model client.
#[derive(Clone)]
pub struct ModelClient {
    config: ModelConfig,
    http: reqwest::Client,
    retry: RetryPolicy,
    /// Permits equal to `max_concurrency`. Acquired before each request,
    /// released after. This is race-free: the semaphore's internal
    /// accounting is atomic and the acquire blocks until a permit is
    /// available (no TOCTOU window).
    semaphore: Arc<Semaphore>,
    /// Retained for `in_flight()` reporting.
    in_flight: Arc<AtomicU32>,
    max_concurrency: u32,
}

impl ModelClient {
    /// Create a client from a validated config.
    pub fn new(config: ModelConfig) -> Result<Self, RequiemError> {
        config.validate().map_err(RequiemError::Config)?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|e| RequiemError::Connection(e.to_string()))?;
        let semaphore = Arc::new(Semaphore::new(
            config
                .max_concurrency
                .max(1)
                .try_into()
                .unwrap_or(u32::MAX as usize),
        ));
        Ok(Self {
            config: config.clone(),
            http,
            retry: from_max_retries(config.max_retries),
            in_flight: Arc::new(AtomicU32::new(0)),
            max_concurrency: config.max_concurrency,
            semaphore,
        })
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    pub fn endpoint(&self) -> &str {
        &self.config.endpoint
    }

    /// The configured max context window in tokens, if set.
    pub fn max_context_tokens(&self) -> Option<u32> {
        self.config.max_context_tokens
    }

    pub fn in_flight(&self) -> u32 {
        self.in_flight.load(Ordering::Relaxed)
    }

    pub fn can_admit(&self) -> bool {
        self.in_flight.load(Ordering::Relaxed) < self.max_concurrency
    }

    /// Acquire a concurrency slot (blocks until one is available).
    /// The returned permit must be held for the duration of the request
    /// to keep the slot reserved; dropping it releases the slot.
    async fn acquire_slot(&self) -> OwnedSemaphorePermit {
        let permit = self.semaphore.clone().acquire_owned().await;
        if let Ok(permit) = permit {
            self.in_flight.fetch_add(1, Ordering::SeqCst);
            permit
        } else {
            // Semaphore was closed — should not happen in practice.
            loop {
                sleep(std::time::Duration::from_millis(25)).await;
            }
        }
    }

    /// Release a concurrency slot.
    fn release_slot(&self, _permit: OwnedSemaphorePermit) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    /// A quick connectivity probe. Used by `/model test` and first-run setup.
    pub async fn test_connection(&self) -> Result<String, RequiemError> {
        let req = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage::system("You are a connectivity probe."),
                ChatMessage::user("Reply with the single word: ok"),
            ],
            temperature: Some(0.0),
            max_tokens: Some(8),
            tools: None,
            tool_choice: None,
            stream: false,
            request_id: None,
        };
        let resp = self.chat(req).await?;
        Ok(resp.model.unwrap_or_else(|| self.config.model.clone()))
    }

    /// A non-streaming chat completion with retries on transient errors.
    pub async fn chat(&self, req: ChatRequest) -> Result<ModelResponse, RequiemError> {
        let _guard = self.acquire_slot().await;
        let res = self.chat_inner(&req).await;
        self.release_slot(_guard);
        res
    }

    async fn chat_inner(&self, req: &ChatRequest) -> Result<ModelResponse, RequiemError> {
        let mut attempt = 0u32;
        loop {
            match self.do_chat(req).await {
                Ok(r) => return Ok(r),
                Err(e) if e.is_retryable() && self.retry.should_retry(attempt) => {
                    let delay = crate::retry::delay_for_error(&self.retry, attempt, &e);
                    attempt += 1;
                    tracing::warn!(
                        "model request failed (retry {attempt}), backoff {delay:?}: {e}"
                    );
                    sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// A streaming chat completion, drained to completion.
    ///
    /// Connects (retrying transient failures), reads the SSE stream to the
    /// end, and returns the parsed chunks in arrival order. A mid-stream
    /// HTTP failure is returned as the `Err`.
    pub async fn stream_drain(&self, req: &ChatRequest) -> Result<Vec<StreamChunk>, RequiemError> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();
        let response = self.chat_stream(req, tx).await?;
        let mut chunks = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            chunks.push(chunk);
        }
        let _ = response; // reassembled response; chunks carry the details
        Ok(chunks)
    }

    /// A single HTTP chat request (non-streaming body parse).
    async fn do_chat(&self, req: &ChatRequest) -> Result<ModelResponse, RequiemError> {
        let body = req.to_json_bytes()?;
        let url = self.config.completions_url();
        let http_req = self
            .authorize(
                self.http
                    .post(&url)
                    .header(ACCEPT, HeaderValue::from_static("application/json"))
                    .header(CONTENT_TYPE, HeaderValue::from_static("application/json")),
            )
            .body(body)
            .send();

        let resp = match http_req.await {
            Ok(r) => r,
            Err(e) => return Err(RequiemError::Connection(e.to_string())),
        };

        let status = resp.status();
        if !status.is_success() {
            let retry_after: Option<String> = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(
                status.as_u16(),
                &text,
                retry_after.as_deref(),
            ));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| RequiemError::Connection(e.to_string()))?;
        parse_chat_response(&text).ok_or_else(|| {
            RequiemError::InvalidResponse(format!("failed to parse model response: {text}"))
        })
    }

    /// Attach the bearer token when a key is configured. Every outbound
    /// request — streaming or not — goes through this so credentials can
    /// never be dropped on one code path only.
    fn authorize(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let key = self.config.resolved_api_key();
        if !key.is_empty() {
            req.header(AUTHORIZATION, format!("Bearer {key}"))
        } else {
            req
        }
    }

    /// A streaming chat completion with incremental delivery.
    ///
    /// Parses the SSE stream chunk-by-chunk as bytes arrive (no full-body
    /// buffering) and pushes every parsed `StreamChunk` through `tx`. When
    /// the stream ends the fully reassembled `ModelResponse` is returned:
    /// content is concatenated, tool-call deltas are merged by index, and
    /// usage is taken from the final chunk when the endpoint reports it.
    pub async fn chat_stream(
        &self,
        req: &ChatRequest,
        tx: tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ModelResponse, RequiemError> {
        let _guard = self.acquire_slot().await;
        let result = self.chat_stream_inner(req, &tx).await;
        self.release_slot(_guard);
        result
    }

    async fn chat_stream_inner(
        &self,
        req: &ChatRequest,
        tx: &tokio::sync::mpsc::UnboundedSender<StreamChunk>,
    ) -> Result<ModelResponse, RequiemError> {
        let mut sreq = req.clone();
        sreq.stream = true;
        let body = sreq.to_json_bytes()?;

        let mut attempt = 0u32;
        let resp = loop {
            match self.connect_stream(&body).await {
                Ok(r) => break r,
                Err(e) if e.is_retryable() && self.retry.should_retry(attempt) => {
                    let delay = crate::retry::delay_for_error(&self.retry, attempt, &e);
                    attempt += 1;
                    tracing::warn!("stream connect failed (retry {attempt}): {e}");
                    sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        };

        // Incremental SSE parse over the byte stream.
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();

        // Reassembly state.
        let mut content = String::new();
        let mut tool_calls: std::collections::BTreeMap<u32, crate::request::ToolCall> =
            std::collections::BTreeMap::new();
        let mut finish_reason: Option<String> = None;
        let mut usage: Option<TokenUsage> = None;

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| RequiemError::Connection(e.to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            // SSE events are separated by newlines; a line may still be
            // incomplete, so only consume whole lines.
            while let Some(pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=pos).collect();
                let line = line.trim();
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                let data = line.strip_prefix("data:").unwrap_or(line).trim_start();
                if data == "[DONE]" || data == "done" {
                    break;
                }
                if let Some(chunk) = parse_sse_chunk(data) {
                    if let Some(c) = &chunk.content {
                        content.push_str(c);
                    }
                    if let Some(delta) = &chunk.tool_call_delta {
                        let entry = tool_calls.entry(delta.index).or_insert_with(|| {
                            crate::request::ToolCall {
                                id: String::new(),
                                name: String::new(),
                                arguments: serde_json::Value::String(String::new()),
                            }
                        });
                        if let Some(id) = &delta.id {
                            entry.id = id.clone();
                        }
                        if let Some(name) = &delta.name {
                            entry.name = name.clone();
                        }
                        if let Some(args) = &delta.arguments {
                            if let serde_json::Value::String(acc) = &mut entry.arguments {
                                acc.push_str(args);
                            } else {
                                entry.arguments = serde_json::Value::String(args.clone());
                            }
                        }
                    }
                    if let Some(fr) = &chunk.finish_reason {
                        finish_reason = Some(fr.clone());
                    }
                    if let Some(u) = chunk.usage {
                        usage = Some(u);
                    }
                    // Deliver without requiring the consumer to keep up:
                    // unbounded means the UI can never block the agent
                    // runtime (a closed channel just drops the chunk).
                    let _ = tx.send(chunk);
                }
            }
        }

        let calls: Vec<crate::request::ToolCall> = tool_calls
            .into_iter()
            .filter(|(_, tc)| !tc.name.is_empty())
            .map(|(idx, mut tc)| {
                if tc.id.is_empty() {
                    tc.id = format!("call_{idx}");
                }
                tc.arguments = normalize_arguments(tc.arguments);
                tc
            })
            .collect();

        Ok(ModelResponse {
            message: crate::request::ChatMessage {
                role: crate::request::Role::Assistant,
                content: if content.is_empty() && !calls.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::String(content))
                },
                tool_call_id: None,
                tool_calls: calls,
            },
            finish_reason,
            usage,
            model: Some(self.config.model.clone()),
        })
    }

    async fn connect_stream(&self, body: &[u8]) -> Result<reqwest::Response, RequiemError> {
        let url = self.config.completions_url();
        let req = self
            .authorize(
                self.http
                    .post(&url)
                    .header(ACCEPT, "text/event-stream")
                    .header(CONTENT_TYPE, HeaderValue::from_static("application/json")),
            )
            .body(body.to_vec());
        let resp = req
            .send()
            .await
            .map_err(|e| RequiemError::Connection(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let retry_after: Option<String> = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_error(status, &text, retry_after.as_deref()));
        }
        Ok(resp)
    }
}

fn classify_http_error(status: u16, body: &str, retry_after_header: Option<&str>) -> RequiemError {
    let snippet: String = body.chars().take(300).collect();
    match status {
        400 => {
            // Distinguish sub-types within 400.
            if body.contains("model")
                && (body.contains("not found")
                    || body.contains("does not exist")
                    || body.contains("unsupported"))
            {
                RequiemError::ModelUnavailable(snippet)
            } else if body.contains("tool")
                && (body.contains("schema")
                    || body.contains("format")
                    || body.contains("incompatible"))
            {
                RequiemError::ToolCallingIncompatible(snippet)
            } else if body.contains("quota")
                || body.contains("billing")
                || body.contains("exceeded")
            {
                RequiemError::QuotaExhausted(snippet)
            } else {
                RequiemError::Model {
                    status: Some(400),
                    message: snippet,
                }
            }
        }
        401 => RequiemError::Auth(format!("invalid credentials: {snippet}")),
        403 => RequiemError::Auth(format!("access forbidden: {snippet}")),
        404 => {
            if body.contains("model") || body.contains("not found") {
                RequiemError::ModelUnavailable(snippet)
            } else {
                RequiemError::Model {
                    status: Some(404),
                    message: snippet,
                }
            }
        }
        408 => RequiemError::Timeout(0),
        413 => RequiemError::ContextLength {
            requested: 0,
            limit: None,
        },
        429 => {
            let retry_after_ms = parse_retry_after(retry_after_header).unwrap_or(1000);
            RequiemError::RateLimited { retry_after_ms }
        }
        409 => RequiemError::StreamingFailure(snippet),
        502..=504 => RequiemError::GatewayFailure {
            status,
            message: snippet,
        },
        500..=599 => RequiemError::Model {
            status: Some(status),
            message: snippet,
        },
        _ => RequiemError::Model {
            status: Some(status),
            message: snippet,
        },
    }
}

/// Parse the `Retry-After` header value into milliseconds.
///
/// Supports both the `delta-seconds` integer form (RFC 9110 §12.2.3) and
/// the `HTTP-date` form. Returns `None` when the header is absent or
/// unparseable.
fn parse_retry_after(header: Option<&str>) -> Option<u64> {
    let value = header?;
    // Try delta-seconds first (most common).
    if let Ok(secs) = value.parse::<u64>() {
        return Some(secs.saturating_mul(1000));
    }
    // Try HTTP-date: parse as RFC 2822 / RFC 1123 format.
    if let Some(date) = parse_http_date(value) {
        let now = std::time::SystemTime::now();
        let now_unix = now
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if date > now_unix {
            return Some((date - now_unix) * 1000);
        }
    }
    None
}

/// Parse an HTTP-date (RFC 7231) into Unix seconds.
fn parse_http_date(s: &str) -> Option<u64> {
    use chrono::DateTime;

    // RFC 7231 IMF-fixdate: "Wed, 21 Oct 2015 07:28:00 GMT"
    let fmt = "%a, %d %b %Y %H:%M:%S %Z";
    if let Ok(dt) = DateTime::parse_from_str(s, fmt) {
        return Some(dt.timestamp().max(0) as u64);
    }
    // RFC 7231 rfc1123 variant without day-of-week.
    let fmt2 = "%d %b %Y %H:%M:%S %Z";
    if let Ok(dt) = DateTime::parse_from_str(s, fmt2) {
        return Some(dt.timestamp().max(0) as u64);
    }
    // RFC 850 (obsolete): "Wednesday, 21-Oct-15 07:28:00 GMT"
    let fmt3 = "%A, %d-%b-%y %H:%M:%S %Z";
    if let Ok(dt) = DateTime::parse_from_str(s, fmt3) {
        return Some(dt.timestamp().max(0) as u64);
    }
    // ISO 8601 (used by some providers): "2024-01-15T12:00:00Z"
    if let Ok(dt) = DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ") {
        return Some(dt.timestamp().max(0) as u64);
    }
    None
}

/// Normalize tool-call arguments: the OpenAI wire format carries them as
/// a JSON-encoded *string*, but tool implementations expect the parsed
/// object. Strings that parse as JSON objects/arrays are converted; any
/// other value is passed through unchanged.
pub(crate) fn normalize_arguments(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) if !s.trim().is_empty() => {
            serde_json::from_str::<serde_json::Value>(&s).unwrap_or(serde_json::Value::String(s))
        }
        other => other,
    }
}

fn parse_chat_response(text: &str) -> Option<ModelResponse> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let choices = v.get("choices")?.as_array()?;
    let choice = choices.first()?;
    let msg = choice.get("message")?;
    let role = msg.get("role")?.as_str()?;
    let role = match role {
        "system" => crate::request::Role::System,
        "user" => crate::request::Role::User,
        "tool" => crate::request::Role::Tool,
        _ => crate::request::Role::Assistant,
    };
    let content = msg.get("content").cloned();
    let mut tool_calls = Vec::new();
    if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tcs {
            let id = tc
                .get("id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let func = tc.get("function");
            let name = func
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = func
                .and_then(|f| f.get("arguments"))
                .cloned()
                .unwrap_or(serde_json::Value::String(String::new()));
            tool_calls.push(crate::request::ToolCall {
                id,
                name,
                arguments: normalize_arguments(args),
            });
        }
    }
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());
    let usage = v.get("usage").map(|u| {
        let input = u.get("prompt_tokens").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
        let output = u
            .get("completion_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as u32;
        TokenUsage {
            input,
            output,
            kind: requiem_core::token::CountKind::Reported,
        }
    });
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());
    Some(ModelResponse {
        message: ChatMessage {
            role,
            content,
            tool_call_id: None,
            tool_calls,
        },
        finish_reason,
        usage,
        model,
    })
}

fn parse_sse_chunk(data: &str) -> Option<StreamChunk> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    let choice = v.get("choices")?.as_array()?.first()?;
    let delta = choice.get("delta");
    let content = delta
        .and_then(|d| d.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    let mut tool_call_delta = None;
    if let Some(tcs) = delta
        .and_then(|d| d.get("tool_calls"))
        .and_then(|t| t.as_array())
    {
        if let Some(tc) = tcs.first() {
            let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string());
            let args = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .map(|s| s.to_string());
            tool_call_delta = Some(ToolCallDelta {
                index: idx,
                id: tc.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()),
                name,
                arguments: args,
            });
        }
    }
    let finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());
    let usage = v.get("usage").map(|u| {
        let input = u.get("prompt_tokens").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
        let output = u
            .get("completion_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as u32;
        TokenUsage {
            input,
            output,
            kind: requiem_core::token::CountKind::Reported,
        }
    });
    Some(StreamChunk {
        content,
        tool_call_delta,
        finish_reason,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_retry_after_delta_seconds() {
        assert_eq!(parse_retry_after(Some("30")), Some(30_000));
        assert_eq!(parse_retry_after(Some("1")), Some(1000));
        assert_eq!(parse_retry_after(Some("0")), Some(0));
    }

    #[test]
    fn parse_retry_after_absent() {
        assert_eq!(parse_retry_after(None), None);
    }

    #[test]
    fn parse_retry_after_invalid() {
        assert_eq!(parse_retry_after(Some("abc")), None);
    }

    #[test]
    fn classify_429_with_retry_after() {
        let err = classify_http_error(429, "rate limited", Some("60"));
        match err {
            RequiemError::RateLimited { retry_after_ms } => {
                assert_eq!(retry_after_ms, 60_000);
            }
            _ => panic!("expected RateLimited, got {err:?}"),
        }
    }

    #[test]
    fn classify_429_without_retry_after() {
        let err = classify_http_error(429, "rate limited", None);
        match err {
            RequiemError::RateLimited { retry_after_ms } => {
                assert_eq!(retry_after_ms, 1000, "falls back to 1s");
            }
            _ => panic!("expected RateLimited, got {err:?}"),
        }
    }

    #[test]
    fn classify_401_is_auth() {
        let err = classify_http_error(401, "bad key", None);
        assert!(matches!(err, RequiemError::Auth(_)));
    }

    #[test]
    fn classify_403_is_auth() {
        let err = classify_http_error(403, "forbidden", None);
        assert!(matches!(err, RequiemError::Auth(_)));
    }

    #[test]
    fn classify_502_is_gateway() {
        let err = classify_http_error(502, "bad gateway", None);
        assert!(matches!(err, RequiemError::GatewayFailure { .. }));
    }

    #[test]
    fn classify_503_is_gateway() {
        let err = classify_http_error(503, "unavailable", None);
        assert!(matches!(err, RequiemError::GatewayFailure { .. }));
    }

    #[test]
    fn classify_504_is_gateway() {
        let err = classify_http_error(504, "gateway timeout", None);
        assert!(matches!(err, RequiemError::GatewayFailure { .. }));
    }

    #[test]
    fn classify_400_model_not_found() {
        let err = classify_http_error(400, "model gpt-9 does not exist", None);
        assert!(matches!(err, RequiemError::ModelUnavailable(_)));
    }

    #[test]
    fn classify_400_quota() {
        let err = classify_http_error(400, "quota exceeded", None);
        assert!(matches!(err, RequiemError::QuotaExhausted(_)));
    }
}
