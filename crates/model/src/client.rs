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
use requiem_networking::ProxyPool;

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
    /// Optional proxy pool: when set, each request selects an endpoint
    /// via the pool's strategy and builds a per-request client with the
    /// matching proxy. When `None` (default), all requests use `self.http`
    /// directly — zero behaviour change for existing setups.
    proxy_pool: Option<Arc<ProxyPool>>,
    /// Wall-clock duration of the most recent model request in milliseconds.
    /// Updated atomically so the dashboard can read it without a lock.
    last_request_ms: Arc<std::sync::atomic::AtomicU64>,
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
            proxy_pool: None,
            last_request_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// Attach a proxy pool. Subsequent requests will select an endpoint
    /// via the pool's strategy and route through the chosen proxy.
    /// When the pool is disabled or has no usable endpoint, requests fall
    /// back to the direct client — no proxy is forced.
    pub fn with_proxy_pool(mut self, pool: Arc<ProxyPool>) -> Self {
        self.proxy_pool = Some(pool);
        self
    }

    /// The proxy pool, if attached.
    pub fn proxy_pool(&self) -> Option<&Arc<ProxyPool>> {
        self.proxy_pool.as_ref()
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

    /// Wall-clock duration of the most recent model request in milliseconds.
    /// Returns 0 if no request has completed yet.
    pub fn last_request_ms(&self) -> u64 {
        self.last_request_ms.load(Ordering::Relaxed)
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
    ///
    /// When a proxy pool is attached:
    /// 1. The global rate-limit coordinator blocks if a 429 cooldown is active.
    /// 2. The pool selects an endpoint for this request.
    /// 3. A per-request client is built with the chosen proxy.
    /// 4. On 429, the cooldown is recorded globally so all agents back off.
    /// 5. On proxy-specific failure, the endpoint's failure count is incremented.
    pub async fn chat(&self, req: ChatRequest) -> Result<ModelResponse, RequiemError> {
        let _guard = self.acquire_slot().await;
        let start = std::time::Instant::now();
        let res = self.chat_inner(&req).await;
        self.last_request_ms
            .store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.release_slot(_guard);
        res
    }

    /// Build the HTTP client for a request, honouring the current proxy
    /// selection. Returns `None` when no proxy is active (use `self.http`).
    async fn http_for_request(
        &self,
    ) -> Option<(reqwest::Client, requiem_networking::EndpointSelection)> {
        let pool = self.proxy_pool.as_ref()?;
        let selection = pool.select().await;
        if let requiem_networking::EndpointSelection::ViaProxy { proxy_url, .. } = &selection {
            let proxy = reqwest::Proxy::all(proxy_url)
                .ok()
                .or_else(|| reqwest::Proxy::http(proxy_url.clone()).ok())?;
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(self.config.timeout_ms))
                .proxy(proxy)
                .build()
                .ok()?;
            Some((client, selection))
        } else {
            None
        }
    }

    async fn chat_inner(&self, req: &ChatRequest) -> Result<ModelResponse, RequiemError> {
        // Global rate-limit gate: block all agents during a shared 429 cooldown.
        if let Some(pool) = &self.proxy_pool {
            pool.rate_limiter().acquire().await;
        }

        let mut attempt = 0u32;
        loop {
            let (client_opt, selection) = match self.http_for_request().await {
                Some((c, s)) => (Some(c), s),
                None => (None, requiem_networking::EndpointSelection::Direct),
            };
            let client = client_opt.as_ref().unwrap_or(&self.http);

            match self.do_chat_with(client, req).await {
                Ok(r) => {
                    // Record success: reset endpoint failure count, clear global cooldown.
                    if let Some(pool) = &self.proxy_pool {
                        if let requiem_networking::EndpointSelection::ViaProxy {
                            endpoint, ..
                        } = &selection
                        {
                            pool.record_success(&endpoint.id, 0).await;
                        }
                        pool.rate_limiter().record_success();
                    }
                    return Ok(r);
                }
                Err(e) if e.is_retryable() && self.retry.should_retry(attempt) => {
                    // Record 429 globally: all agents will back off together.
                    if let Some(pool) = &self.proxy_pool {
                        if let RequiemError::RateLimited { retry_after_ms } = &e {
                            pool.rate_limiter().record_rate_limit(*retry_after_ms).await;
                            if let requiem_networking::EndpointSelection::ViaProxy {
                                endpoint,
                                ..
                            } = &selection
                            {
                                pool.record_rate_limit(&endpoint.id, *retry_after_ms).await;
                            }
                        } else {
                            // Non-429 transport failure through a proxy:
                            // increment the endpoint's failure count.
                            if let requiem_networking::EndpointSelection::ViaProxy {
                                endpoint,
                                ..
                            } = &selection
                            {
                                pool.record_failure(&endpoint.id, &e.to_string()).await;
                            }
                        }
                    }
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
        // Bounded channel: a slow consumer applies backpressure to the
        // network read instead of buffering the whole response in memory.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamChunk>(STREAM_CHANNEL_CAPACITY);
        let collect = async move {
            let mut chunks = Vec::new();
            while let Some(chunk) = rx.recv().await {
                chunks.push(chunk);
            }
            chunks
        };
        // Drive producer and collector together so the bounded channel can
        // never deadlock (the old post-hoc `try_recv` drained nothing).
        let (response, chunks) = tokio::join!(self.chat_stream(req, tx), collect);
        response?;
        Ok(chunks)
    }

    /// A single HTTP chat request using an explicit client (proxy-aware).
    async fn do_chat_with(
        &self,
        client: &reqwest::Client,
        req: &ChatRequest,
    ) -> Result<ModelResponse, RequiemError> {
        let body = req.to_json_bytes()?;
        let url = self.config.completions_url();
        let http_req = self
            .authorize(
                client
                    .post(&url)
                    .header(ACCEPT, HeaderValue::from_static("application/json"))
                    .header(CONTENT_TYPE, HeaderValue::from_static("application/json")),
            )
            .body(body)
            .send();

        let resp = match http_req.await {
            Ok(r) => r,
            Err(e) => return Err(classify_reqwest_error(&e, self.config.timeout_ms)),
        };

        let status = resp.status();
        if !status.is_success() {
            let retry_after: Option<String> = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let request_id = request_id_header(&resp);
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_error_with_id(
                status.as_u16(),
                &text,
                retry_after.as_deref(),
                request_id.as_deref(),
            ));
        }

        let request_id = request_id_header(&resp);
        let text = resp
            .text()
            .await
            .map_err(|e| classify_reqwest_error(&e, self.config.timeout_ms))?;
        parse_chat_response(&text).ok_or_else(|| {
            RequiemError::InvalidResponse(format!(
                "unparseable response (HTTP {}, {} bytes{}){}",
                status.as_u16(),
                text.len(),
                request_id
                    .as_deref()
                    .map(|id| format!(", request id {id}"))
                    .unwrap_or_default(),
                body_diagnostic(&text),
            ))
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
        tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<ModelResponse, RequiemError> {
        let _guard = self.acquire_slot().await;
        let start = std::time::Instant::now();
        let result = self.chat_stream_inner(req, &tx).await;
        self.last_request_ms
            .store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.release_slot(_guard);
        result
    }

    async fn chat_stream_inner(
        &self,
        req: &ChatRequest,
        tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<ModelResponse, RequiemError> {
        // Global rate-limit gate before streaming.
        if let Some(pool) = &self.proxy_pool {
            pool.rate_limiter().acquire().await;
        }

        let mut sreq = req.clone();
        sreq.stream = true;
        let body = sreq.to_json_bytes()?;

        let mut attempt = 0u32;
        let resp = loop {
            let (client_opt, selection) = match self.http_for_request().await {
                Some((c, s)) => (Some(c), s),
                None => (None, requiem_networking::EndpointSelection::Direct),
            };
            let client = client_opt.as_ref().unwrap_or(&self.http);
            match self.connect_stream_with(client, &body).await {
                Ok(r) => {
                    // Record success.
                    if let Some(pool) = &self.proxy_pool {
                        if let requiem_networking::EndpointSelection::ViaProxy {
                            endpoint, ..
                        } = &selection
                        {
                            pool.record_success(&endpoint.id, 0).await;
                        }
                        pool.rate_limiter().record_success();
                    }
                    break r;
                }
                Err(e) if e.is_retryable() && self.retry.should_retry(attempt) => {
                    if let Some(pool) = &self.proxy_pool {
                        if let RequiemError::RateLimited { retry_after_ms } = &e {
                            pool.rate_limiter().record_rate_limit(*retry_after_ms).await;
                            if let requiem_networking::EndpointSelection::ViaProxy {
                                endpoint,
                                ..
                            } = &selection
                            {
                                pool.record_rate_limit(&endpoint.id, *retry_after_ms).await;
                            }
                        } else if let requiem_networking::EndpointSelection::ViaProxy {
                            endpoint,
                            ..
                        } = &selection
                        {
                            pool.record_failure(&endpoint.id, &e.to_string()).await;
                        }
                    }
                    let delay = crate::retry::delay_for_error(&self.retry, attempt, &e);
                    attempt += 1;
                    tracing::warn!("stream connect failed (retry {attempt}): {e}");
                    sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        };

        // Incremental SSE parse over the byte stream. Bytes are decoded
        // through a stateful UTF-8 decoder so a multi-byte character split
        // across two HTTP chunks is never replaced with U+FFFD.
        let mut stream = resp.bytes_stream();
        let mut pending_bytes: Vec<u8> = Vec::new();
        let mut buffer = String::new();
        let mut state = Reassembly::default();
        let mut done = false;

        while !done {
            let Some(chunk) = stream.next().await else {
                break;
            };
            let bytes = chunk.map_err(|e| classify_stream_error(&e))?;
            buffer.push_str(&decode_utf8_chunk(&mut pending_bytes, &bytes));

            // SSE events are newline-delimited; only consume whole lines.
            while let Some(pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=pos).collect();
                let (out, is_done) = state.feed(&line)?;
                if let Some(chunk) = out {
                    // Backpressure: wait for room instead of buffering
                    // without bound. A closed receiver means the consumer
                    // was cancelled — keep parsing for the final response,
                    // but stop trying to deliver.
                    if tx.send(chunk).await.is_err() {
                        tracing::debug!("stream consumer dropped; reassembling silently");
                    }
                }
                if is_done {
                    done = true;
                    break;
                }
            }
        }

        // Flush any incomplete tail: decode whatever UTF-8 is valid and
        // consume a final SSE line that arrived without a trailing newline.
        if !pending_bytes.is_empty() {
            buffer.push_str(&String::from_utf8_lossy(&pending_bytes));
            pending_bytes.clear();
        }
        if !done && !buffer.trim().is_empty() {
            let line = std::mem::take(&mut buffer);
            let (out, _) = state.feed(&line)?;
            if let Some(chunk) = out {
                let _ = tx.send(chunk).await;
            }
        }

        let calls: Vec<crate::request::ToolCall> = state
            .tool_calls
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

        let content = state.content;
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
            finish_reason: state.finish_reason,
            usage: state.usage,
            model: Some(self.config.model.clone()),
        })
    }

    async fn connect_stream_with(
        &self,
        client: &reqwest::Client,
        body: &[u8],
    ) -> Result<reqwest::Response, RequiemError> {
        let url = self.config.completions_url();
        let req = self
            .authorize(
                client
                    .post(&url)
                    .header(ACCEPT, "text/event-stream")
                    .header(CONTENT_TYPE, HeaderValue::from_static("application/json")),
            )
            .body(body.to_vec());
        let resp = req
            .send()
            .await
            .map_err(|e| classify_reqwest_error(&e, self.config.timeout_ms))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let retry_after: Option<String> = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let request_id = request_id_header(&resp);
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_error_with_id(
                status,
                &text,
                retry_after.as_deref(),
                request_id.as_deref(),
            ));
        }
        Ok(resp)
    }
}

/// Capacity of the streaming channel. A slow UI consumer applies
/// backpressure after this many buffered chunks instead of growing the
/// queue without bound.
const STREAM_CHANNEL_CAPACITY: usize = 64;

/// Reassembles an OpenAI-compatible SSE stream while forwarding each parsed
/// chunk to the UI.
#[derive(Default)]
struct Reassembly {
    content: String,
    tool_calls: std::collections::BTreeMap<u32, crate::request::ToolCall>,
    finish_reason: Option<String>,
    usage: Option<TokenUsage>,
}

impl Reassembly {
    /// Feed one raw SSE line. Returns the chunk to forward (if any) and
    /// whether the stream signalled `[DONE]`.
    fn feed(&mut self, line: &str) -> Result<(Option<StreamChunk>, bool), RequiemError> {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            return Ok((None, false));
        }
        let data = line.strip_prefix("data:").unwrap_or(line).trim_start();
        if data == "[DONE]" || data == "done" {
            return Ok((None, true));
        }
        // A provider can send an error object mid-stream instead of a
        // completion chunk. Surface it instead of silently ending with an
        // empty response.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
            if v.get("error").is_some() {
                return Err(RequiemError::StreamingFailure(summarize_error(data)));
            }
        }
        let Some(chunk) = parse_sse_chunk(data) else {
            // Ignore keep-alives and provider noise rather than aborting a
            // healthy stream.
            return Ok((None, false));
        };
        if let Some(c) = &chunk.content {
            self.content.push_str(c);
        }
        if let Some(delta) = &chunk.tool_call_delta {
            let entry =
                self.tool_calls
                    .entry(delta.index)
                    .or_insert_with(|| crate::request::ToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: serde_json::Value::String(String::new()),
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
            self.finish_reason = Some(fr.clone());
        }
        if let Some(u) = chunk.usage {
            self.usage = Some(u);
        }
        Ok((Some(chunk), false))
    }
}

/// Append `new` bytes to `pending` and decode the longest valid UTF-8
/// prefix. An incomplete trailing sequence stays buffered for the next
/// network chunk; genuinely invalid bytes become a single U+FFFD.
fn decode_utf8_chunk(pending: &mut Vec<u8>, new: &[u8]) -> String {
    pending.extend_from_slice(new);
    let mut out = String::with_capacity(pending.len());
    loop {
        match std::str::from_utf8(pending) {
            Ok(s) => {
                out.push_str(s);
                pending.clear();
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                if valid > 0 {
                    out.push_str(std::str::from_utf8(&pending[..valid]).unwrap_or(""));
                    pending.drain(..valid);
                }
                match e.error_len() {
                    Some(n) => {
                        // Definitely invalid: replace and skip.
                        out.push('\u{FFFD}');
                        pending.drain(..n);
                    }
                    // Incomplete tail: wait for the rest of the character.
                    None => break,
                }
            }
        }
    }
    out
}

/// Redact credentials and bound a diagnostic string so provider errors can
/// never leak keys or dump an entire body into the transcript.
fn sanitize_text(input: &str, max: usize) -> String {
    let mut s = input.to_string();
    // Bearer / Authorization material.
    for marker in ["Bearer ", "bearer ", "Basic ", "basic "] {
        let mut search = 0;
        while let Some(rel) = s[search..].find(marker) {
            let pos = search + rel;
            let start = pos + marker.len();
            let end = s[start..]
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';'))
                .map(|i| start + i)
                .unwrap_or(s.len());
            s.replace_range(start..end, "***");
            search = start + 3;
            if search >= s.len() {
                break;
            }
        }
    }
    // Common key shapes.
    for prefix in ["sk-", "sk_", "rq-", "rk-", "ghp_", "xoxb-"] {
        let mut search = 0;
        while let Some(rel) = s[search..].find(prefix) {
            let start = search + rel;
            let end = s[start..]
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | '&'))
                .map(|i| start + i)
                .unwrap_or(s.len());
            s.replace_range(start..end, "<redacted>");
            search = start + "<redacted>".len();
            if search >= s.len() {
                break;
            }
        }
    }
    // URL userinfo: scheme://user:pass@host → scheme://***@host
    if let Some(scheme) = s.find("://") {
        let after = scheme + 3;
        if let Some(at_rel) = s[after..].find('@') {
            let at = after + at_rel;
            let slash = s[after..at].find('/').map(|i| after + i);
            if slash.is_none() {
                s.replace_range(after..at, "***");
            }
        }
    }
    // API key query params.
    for param in ["api_key=", "apikey=", "access_token=", "key="] {
        let mut search = 0;
        while let Some(rel) = s[search..].find(param) {
            let pos = search + rel;
            let start = pos + param.len();
            let end = s[start..]
                .find(|c: char| c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ','))
                .map(|i| start + i)
                .unwrap_or(s.len());
            s.replace_range(start..end, "***");
            search = start + 3;
            if search >= s.len() {
                break;
            }
        }
    }

    if s.chars().count() > max {
        let mut truncated: String = s.chars().take(max).collect();
        truncated.push('…');
        truncated
    } else {
        s
    }
}

/// Extract a bounded, sanitized description of an OpenAI-compatible error
/// body.
fn parse_error_body(body: &str) -> (String, Option<String>, Option<String>) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let err = v.get("error");
        let message = err
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .or_else(|| err.and_then(|e| e.as_str()))
            .or_else(|| v.get("message").and_then(|m| m.as_str()))
            .or_else(|| v.get("detail").and_then(|m| m.as_str()))
            .unwrap_or("");
        let kind = err
            .and_then(|e| e.get("type"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        let code = err.and_then(|e| e.get("code")).and_then(|c| {
            c.as_str()
                .map(|s| s.to_string())
                .or_else(|| c.as_u64().map(|n| n.to_string()))
        });
        if !message.is_empty() || kind.is_some() || code.is_some() {
            return (
                sanitize_text(message, 240),
                kind.map(|s| sanitize_text(&s, 60)),
                code.map(|s| sanitize_text(&s, 60)),
            );
        }
        // Valid JSON but no known shape: never echo the whole object.
        return (
            format!(
                "provider returned a JSON error object ({} bytes)",
                body.len()
            ),
            None,
            None,
        );
    }
    // Non-JSON body: likely an HTML gateway page. Keep only a short,
    // sanitized excerpt and never the whole document.
    (sanitize_text(body.trim(), 200), None, None)
}

/// A one-line summary of an upstream error body, including provider type
/// and code when present.
fn summarize_error(body: &str) -> String {
    let (message, kind, code) = parse_error_body(body);
    let mut out = message;
    if kind.is_some() || code.is_some() {
        let mut meta = Vec::new();
        if let Some(k) = kind {
            meta.push(format!("type: {k}"));
        }
        if let Some(c) = code {
            meta.push(format!("code: {c}"));
        }
        out.push_str(&format!(" ({})", meta.join(", ")));
    }
    sanitize_text(&out, 300)
}

/// Suffix for an invalid-response diagnostic: bounded and sanitized.
fn body_diagnostic(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    let summary = summarize_error(text);
    if summary.is_empty() {
        String::new()
    } else {
        format!(": {summary}")
    }
}

/// Read the provider request id from response headers, if present.
fn request_id_header(resp: &reqwest::Response) -> Option<String> {
    for name in ["x-request-id", "request-id", "x-amzn-requestid"] {
        if let Some(v) = resp.headers().get(name).and_then(|v| v.to_str().ok()) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(sanitize_text(v, 80));
            }
        }
    }
    None
}

#[cfg(test)]
fn classify_http_error(status: u16, body: &str, retry_after_header: Option<&str>) -> RequiemError {
    classify_http_error_with_id(status, body, retry_after_header, None)
}

fn classify_http_error_with_id(
    status: u16,
    body: &str,
    retry_after_header: Option<&str>,
    request_id: Option<&str>,
) -> RequiemError {
    let snippet = summarize_error(body);
    let suffix = request_id
        .map(|id| format!(" [request id: {id}]"))
        .unwrap_or_default();
    // Classification hints: provider type/code first, message as fallback.
    let (_, kind, code) = parse_error_body(body);
    let hints = format!(
        "{} {} {}",
        kind.as_deref().unwrap_or(""),
        code.as_deref().unwrap_or(""),
        snippet
    )
    .to_lowercase();
    let has = |needle: &str| hints.contains(needle);

    match status {
        400 | 422 => {
            if has("model") && (has("not found") || has("does not exist") || has("unsupported")) {
                RequiemError::ModelUnavailable(snippet)
            } else if has("tool")
                && (has("schema") || has("format") || has("incompatible") || has("not supported"))
            {
                RequiemError::ToolCallingIncompatible(snippet)
            } else if has("quota") || has("billing") || has("credit") || has("exceeded") {
                RequiemError::QuotaExhausted(snippet)
            } else {
                RequiemError::Model {
                    status: Some(status),
                    message: format!("{snippet}{suffix}"),
                }
            }
        }
        401 => RequiemError::Auth(format!("invalid credentials: {snippet}{suffix}")),
        402 => RequiemError::QuotaExhausted(snippet),
        403 => RequiemError::Auth(format!("access forbidden: {snippet}{suffix}")),
        404 => {
            if has("model") || has("not found") {
                RequiemError::ModelUnavailable(snippet)
            } else {
                RequiemError::Model {
                    status: Some(404),
                    message: format!("{snippet}{suffix}"),
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
            message: format!("{snippet}{suffix}"),
        },
        _ => RequiemError::Model {
            status: Some(status),
            message: format!("{snippet}{suffix}"),
        },
    }
}

/// Walk an error's source chain and collect lowercase messages.
fn source_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut cursor = err.source();
    while let Some(cause) = cursor {
        out.push_str(" | ");
        out.push_str(&cause.to_string());
        cursor = cause.source();
    }
    out.to_lowercase()
}

/// Classify a `reqwest` transport failure from structured error flags and
/// the source chain — never from a guess about the top-level string.
fn classify_reqwest_error(err: &reqwest::Error, timeout_ms: u64) -> RequiemError {
    let chain = source_chain(err);
    if err.is_timeout() {
        return RequiemError::Timeout(timeout_ms);
    }
    if err.is_connect() {
        if chain.contains("certificate")
            || chain.contains("tls")
            || chain.contains("ssl")
            || chain.contains("handshake")
            || chain.contains("unknown issuer")
            || chain.contains("self-signed")
        {
            return RequiemError::Tls(sanitize_text(&chain, 240));
        }
        if chain.contains("dns")
            || chain.contains("failed to lookup address")
            || chain.contains("name or service not known")
            || chain.contains("nodename nor servname")
            || chain.contains("no such host")
        {
            return RequiemError::Dns(sanitize_text(&chain, 240));
        }
        return RequiemError::Connection(sanitize_text(&chain, 240));
    }
    if err.is_body() || err.is_decode() {
        return RequiemError::StreamingFailure(format!(
            "response interrupted: {}",
            sanitize_text(&chain, 200)
        ));
    }
    if err.is_builder() || err.is_request() {
        // Malformed request or invalid configuration — permanent.
        return RequiemError::Config(sanitize_text(&chain, 240));
    }
    RequiemError::Connection(sanitize_text(&chain, 240))
}

/// Classify a failure while reading the SSE body.
fn classify_stream_error(err: &reqwest::Error) -> RequiemError {
    if err.is_timeout() {
        return RequiemError::Timeout(0);
    }
    RequiemError::StreamingFailure(format!(
        "stream interrupted: {}",
        sanitize_text(&source_chain(err), 200)
    ))
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

    #[test]
    fn classify_402_is_quota() {
        let err = classify_http_error(402, "payment required", None);
        assert!(matches!(err, RequiemError::QuotaExhausted(_)));
    }

    #[test]
    fn classify_json_error_extracts_message_type_code() {
        let body = r#"{"error":{"message":"model gpt-9 not found","type":"invalid_request_error","code":"model_not_found"}}"#;
        let err = classify_http_error(404, body, None);
        match err {
            RequiemError::ModelUnavailable(msg) => {
                assert!(msg.contains("model gpt-9 not found"), "got: {msg}");
                assert!(msg.contains("code: model_not_found"), "got: {msg}");
            }
            other => panic!("expected ModelUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn classify_401_never_echoes_key() {
        let body = r#"{"error":{"message":"Incorrect API key provided: sk-secret123456. Authorization: Bearer sk-secret123456"}}"#;
        let err = classify_http_error(401, body, None);
        let text = err.to_string();
        assert!(!text.contains("secret123456"), "key leaked: {text}");
        assert!(
            text.contains("***") || text.contains("<redacted>"),
            "redacted: {text}"
        );
    }

    #[test]
    fn sanitize_redacts_tokens_and_urls() {
        let s = sanitize_text(
            "Authorization: Bearer abcdef123456 at https://user:pw@example.com/x?api_key=topsecret",
            300,
        );
        assert!(!s.contains("abcdef123456"), "{s}");
        assert!(!s.contains("topsecret"), "{s}");
        assert!(!s.contains("user:pw"), "{s}");
    }

    #[test]
    fn sanitize_is_bounded() {
        let s = sanitize_text(&"x".repeat(10_000), 120);
        assert!(s.chars().count() <= 121, "len: {}", s.chars().count());
    }

    #[test]
    fn summarize_non_json_is_bounded() {
        let body = format!("<html>{}</html>", "noise ".repeat(1000));
        let out = summarize_error(&body);
        assert!(out.chars().count() <= 301);
        assert!(!out.contains("</html>") || out.chars().count() <= 301);
    }

    #[test]
    fn request_id_is_appended_to_gateway_errors() {
        let err = classify_http_error_with_id(502, "bad gateway", None, Some("req-123"));
        let text = err.to_string();
        assert!(text.contains("req-123"), "got: {text}");
    }

    #[test]
    fn decode_utf8_split_multibyte_character() {
        // 🌍 = F0 9F 8C 8D, split across three network chunks.
        let bytes = "a🌍b".as_bytes();
        let mut pending = Vec::new();
        let mut out = String::new();
        for part in [&bytes[..2], &bytes[2..3], &bytes[3..]] {
            out.push_str(&decode_utf8_chunk(&mut pending, part));
        }
        assert!(pending.is_empty());
        assert_eq!(out, "a🌍b");
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn decode_utf8_invalid_bytes_become_one_replacement() {
        let mut pending = Vec::new();
        let out = decode_utf8_chunk(&mut pending, &[0xFF, b'o', b'k']);
        assert_eq!(out, "\u{FFFD}ok");
        assert!(pending.is_empty());
    }

    #[test]
    fn reassembly_accumulates_content_and_tool_calls() {
        let mut r = Reassembly::default();
        let c1 = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}";
        let c2 =
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}";
        let tc1 = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\"}}]}}]}";
        let tc2 = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"p\\\":1}\"}}]}}]}";
        for line in [c1, c2, tc1, tc2] {
            r.feed(line).unwrap();
        }
        assert_eq!(r.content, "Hello");
        assert_eq!(r.finish_reason.as_deref(), Some("stop"));
        assert_eq!(r.tool_calls.len(), 1);
        let tc = r.tool_calls.get(&0).unwrap();
        assert_eq!(tc.name, "read_file");
        assert_eq!(tc.id, "call_a");
        assert_eq!(
            tc.arguments,
            serde_json::Value::String("{\"p\":1}".to_string())
        );
    }

    #[test]
    fn reassembly_honours_done_marker() {
        let mut r = Reassembly::default();
        let (_, done) = r.feed("data: [DONE]").unwrap();
        assert!(done);
    }

    #[test]
    fn reassembly_mid_stream_error_is_reported() {
        let mut r = Reassembly::default();
        let err = r
            .feed("data: {\"error\":{\"message\":\"upstream exploded\",\"type\":\"server_error\"}}")
            .unwrap_err();
        assert!(matches!(err, RequiemError::StreamingFailure(_)));
        assert!(err.to_string().contains("upstream exploded"));
    }
}
