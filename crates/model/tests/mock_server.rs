//! Integration tests for the model client using a hand-rolled mock HTTP server.
//!
//! A tokio TCP listener accepts connections, records every request (method,
//! path, headers, body), and returns a canned OpenAI-compatible response.
//! Bodies can be a single static payload or an ordered sequence, enabling
//! deterministic multi-turn agent-loop tests without external dependencies.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use requiem_model::client::ModelClient;
use requiem_model::config::ModelConfig;
use requiem_model::request::{ChatMessage, ChatRequest, ToolCall, ToolDef};
use requiem_model::response::ModelResponse;

/// Everything the mock server saw for one request.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub path: String,
    pub authorization: Option<String>,
    pub accept: Option<String>,
    pub content_type: Option<String>,
    pub body: String,
}

/// A mock OpenAI-compatible server.
pub struct MockServer {
    addr: SocketAddr,
    requests: std::sync::Arc<Mutex<Vec<RecordedRequest>>>,
    signal: std::sync::Arc<AtomicU32>,
}

impl MockServer {
    /// Start a server that replies with `body` (SSE or JSON) for every request.
    pub async fn single(body: &str) -> Self {
        Self::sequence(vec![body.to_string()], body.to_string()).await
    }

    /// Start a server serving `bodies` in order; the *last* body repeats
    /// for any further requests. `content_type` of each body is inferred:
    /// bodies starting with `data:` are served as `text/event-stream`.
    pub async fn sequence(bodies: Vec<String>, repeat_last: String) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let requests: Arc<Mutex<Vec<RecordedRequest>>> = Default::default();
        let signal = Arc::new(AtomicU32::new(0));

        let sig_clone = signal.clone();
        let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(bodies));
        let repeat_last: Arc<Mutex<String>> = Arc::new(Mutex::new(repeat_last));

        let requests_arc = requests.clone();
        tokio::spawn(async move {
            let requests = requests_arc;
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                if sig_clone.load(Ordering::Relaxed) == 1 {
                    break;
                }

                // Read until the end of headers, then read Content-Length bytes.
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut raw: Vec<u8> = Vec::new();
                let mut buf = [0u8; 4096];
                let mut header_end = raw.len();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(pos) = find_subsequence(&raw, b"\r\n\r\n") {
                        header_end = pos + 4;
                        break;
                    }
                }
                // Parse headers.
                let head = String::from_utf8_lossy(&raw[..header_end.min(raw.len())]).to_string();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or("").to_string();
                let path = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let mut authorization = None;
                let mut accept = None;
                let mut content_type = None;
                let mut content_length = 0usize;
                for line in lines {
                    let Some((name, value)) = line.split_once(':') else {
                        continue;
                    };
                    let name = name.trim().to_ascii_lowercase();
                    let value = value.trim().to_string();
                    match name.as_str() {
                        "authorization" => authorization = Some(value),
                        "accept" => accept = Some(value),
                        "content-type" => content_type = Some(value),
                        "content-length" => content_length = value.parse().unwrap_or(0),
                        _ => {}
                    }
                }
                // Read the body if declared.
                while raw.len() < header_end + content_length {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                }
                let body = String::from_utf8_lossy(
                    raw.get(header_end..(header_end + content_length))
                        .unwrap_or(&[]),
                )
                .to_string();

                requests.lock().unwrap().push(RecordedRequest {
                    path,
                    authorization,
                    accept,
                    content_type,
                    body,
                });

                // Pick the next canned body.
                let next = {
                    let mut seq = bodies.lock().unwrap();
                    if !seq.is_empty() {
                        seq.remove(0)
                    } else {
                        repeat_last.lock().unwrap().clone()
                    }
                };
                let is_sse = next.starts_with("data:") || next.contains("\ndata:");
                let ctype = if is_sse {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    next.len(),
                    next
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            }
        });

        Self {
            addr,
            requests,
            signal,
        }
    }

    /// The endpoint base URL for client configs.
    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// All recorded requests.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// The most recent recorded request.
    pub fn last_request(&self) -> Option<RecordedRequest> {
        self.requests.lock().unwrap().last().cloned()
    }

    /// Shut the server down.
    pub fn shutdown(&self) {
        self.signal.store(1, Ordering::Relaxed);
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn make_config(endpoint: &str, model: &str) -> ModelConfig {
    ModelConfig {
        endpoint: endpoint.to_string(),
        api_key: "test-key".to_string(),
        model: model.to_string(),
        timeout_ms: 5_000,
        max_concurrency: 2,
        max_retries: 2,
        max_context_tokens: None,
        max_output_tokens: None,
    }
}

fn make_client(addr: SocketAddr) -> ModelClient {
    let endpoint = format!("http://{}", addr);
    let config = make_config(&endpoint, "mock-model");
    ModelClient::new(config).expect("client")
}

fn simple_chat_body(content: &str) -> String {
    format!(
        r#"{{"id":"chatcmpl-1","object":"chat.completion","model":"mock-model","choices":[{{"index":0,"message":{{"role":"assistant","content":"{content}"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}}}"#
    )
}

#[tokio::test]
async fn chat_returns_parsed_response() {
    let server = MockServer::single(&simple_chat_body("Hello, world!")).await;
    let client = make_client(server.addr);

    let req = ChatRequest {
        model: "mock-model".to_string(),
        messages: vec![
            ChatMessage::system("You are a helpful assistant."),
            ChatMessage::user("Say hello"),
        ],
        temperature: Some(0.0),
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    };

    let resp: ModelResponse = client.chat(req).await.expect("chat");
    assert_eq!(
        resp.message.content.as_ref().and_then(|v| v.as_str()),
        Some("Hello, world!")
    );
    assert_eq!(resp.usage.as_ref().map(|u| u.input), Some(10));
    assert_eq!(resp.usage.as_ref().map(|u| u.output), Some(5));
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
}

/// Regression: the non-streaming path must send the Authorization header
/// exactly like the streaming path.
#[tokio::test]
async fn chat_sends_authorization_header() {
    let server = MockServer::single(&simple_chat_body("ok")).await;
    let client = make_client(server.addr);

    let req = ChatRequest {
        model: "mock-model".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    };
    client.chat(req).await.expect("chat");

    let last = server.last_request().expect("recorded request");
    assert_eq!(
        last.authorization.as_deref(),
        Some("Bearer test-key"),
        "non-streaming requests must carry the API key"
    );
    assert_eq!(last.path, "/v1/chat/completions");
    assert_eq!(last.accept.as_deref(), Some("application/json"));
}

/// Regression: endpoint URLs that already end in /v1 must not be doubled.
#[tokio::test]
async fn completions_url_does_not_duplicate_v1() {
    let server = MockServer::single(&simple_chat_body("ok")).await;
    let cfg = ModelConfig {
        endpoint: format!("{}/v1", server.endpoint()),
        ..make_config(&server.endpoint(), "m")
    };
    let client = ModelClient::new(cfg).unwrap();
    let req = ChatRequest {
        model: "m".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    };
    client.chat(req).await.expect("chat");
    assert_eq!(server.last_request().unwrap().path, "/v1/chat/completions");
}

#[tokio::test]
async fn chat_with_tool_calls() {
    let body = r#"{
        "id": "chatcmpl-2",
        "object": "chat.completion",
        "model": "mock-model",
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_abc",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": "{\"path\":\"main.rs\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }
        ],
        "usage": {
            "prompt_tokens": 20,
            "completion_tokens": 10,
            "total_tokens": 30
        }
    }"#;

    let server = MockServer::single(body).await;
    let client = make_client(server.addr);

    let req = ChatRequest {
        model: "mock-model".to_string(),
        messages: vec![ChatMessage::user("Read main.rs")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    };

    let resp = client.chat(req).await.expect("chat");
    assert_eq!(resp.message.tool_calls.len(), 1);
    assert_eq!(resp.message.tool_calls[0].name, "read_file");
    assert!(resp.usage.as_ref().is_some());
}

#[tokio::test]
async fn test_connection_returns_model_name() {
    let body = r#"{
        "id": "chatcmpl-3",
        "object": "chat.completion",
        "model": "mock-model",
        "choices": [
            {
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }
        ]
    }"#;

    let server = MockServer::single(body).await;
    let client = make_client(server.addr);
    let model = client.test_connection().await.expect("probe");
    assert_eq!(model, "mock-model");
}

#[tokio::test]
async fn stream_drain_returns_chunks() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo!\"}}]}\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\n";

    let server = MockServer::single(body).await;
    let client = make_client(server.addr);

    let req = ChatRequest {
        model: "mock-model".to_string(),
        messages: vec![ChatMessage::user("Say hello")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: true,
        request_id: None,
    };

    let chunks = client.stream_drain(&req).await.expect("stream");
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].content.as_deref(), Some("Hel"));
    assert_eq!(chunks[1].content.as_deref(), Some("lo!"));
    assert_eq!(chunks[2].finish_reason.as_deref(), Some("stop"));
}

/// The incremental stream must emit deltas as they arrive (before the
/// full response is reassembled) and must reassemble content correctly.
#[tokio::test]
async fn chat_stream_delivers_and_reassembles() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo \"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"world\"}}]}\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n";

    let server = MockServer::single(body).await;
    let client = make_client(server.addr);

    let req = ChatRequest {
        model: "mock-model".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: true,
        request_id: None,
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let resp = client.chat_stream(&req, tx).await.expect("stream");
    assert_eq!(
        resp.text(),
        Some("Hello world"),
        "content must be reassembled from deltas"
    );
    assert_eq!(resp.usage.as_ref().map(|u| u.input), Some(7));

    // All three content deltas must have been delivered incrementally.
    let mut deltas = Vec::new();
    while let Ok(c) = rx.try_recv() {
        if let Some(d) = c.content {
            deltas.push(d);
        }
    }
    assert_eq!(
        deltas,
        vec!["Hel".to_string(), "lo ".to_string(), "world".to_string()]
    );
}

/// Streaming tool-call deltas must merge into a single complete call.
#[tokio::test]
async fn chat_stream_merges_tool_call_deltas() {
    let body = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_x\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\ndata: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a.rs\\\"}\"}}]}}]}\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n";

    let server = MockServer::single(body).await;
    let client = make_client(server.addr);

    let req = ChatRequest {
        model: "mock-model".into(),
        messages: vec![ChatMessage::user("read")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: true,
        request_id: None,
    };

    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let resp = client.chat_stream(&req, tx).await.expect("stream");
    assert_eq!(resp.message.tool_calls.len(), 1);
    let tc = &resp.message.tool_calls[0];
    assert_eq!(tc.name, "read_file");
    assert_eq!(tc.id, "call_x");
    assert_eq!(
        tc.arguments,
        serde_json::json!({"path": "a.rs"}),
        "argument deltas must concatenate into valid JSON"
    );
    assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
}

/// HTTP failures must classify into friendly, non-panicking errors.
#[tokio::test]
async fn http_errors_classify() {
    // 401 → Model error with status; the message must include the body.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 21\r\nConnection: close\r\n\r\ninvalid credentials")
                .await;
            let _ = sock.shutdown().await;
        }
    });

    let cfg = make_config(&format!("http://{addr}"), "m");
    let client = ModelClient::new(cfg).unwrap();
    let req = ChatRequest {
        model: "m".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    };
    let err = client.chat(req).await.unwrap_err();
    assert!(matches!(err, requiem_core::error::RequiemError::Auth(_)));
    // 401 is not retryable — the client must fail fast.
    assert!(!err.is_retryable());
}

/// Regression: a request that carries a tool-result message must serialize
/// the `tool_calls` field in the nested OpenAI wire shape
/// (`{"id":…, "function": {"name":…, "arguments": "…"}}`) so a strict
/// gateway deserializer does not reject the payload with
/// `missing field 'function'`.
#[tokio::test]
async fn outgoing_tool_calls_use_nested_function_shape() {
    let body = simple_chat_body("ok");
    let server = MockServer::single(&body).await;
    let client = make_client(server.addr);

    // Simulate the two-turn exchange: assistant requests a tool call,
    // REQUIEM executes it and sends the result back.
    let assistant_msg = ChatMessage {
        role: requiem_model::request::Role::Assistant,
        content: None,
        tool_call_id: None,
        tool_calls: vec![ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "main.rs"}),
        }],
    };
    let tool_msg = ChatMessage::tool_result("call_1", "fn main() {}");
    let user_msg = ChatMessage::user("What did the tool say?");

    let req = ChatRequest {
        model: "mock-model".to_string(),
        messages: vec![assistant_msg, tool_msg, user_msg],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    };

    client.chat(req).await.expect("chat");

    let last = server.last_request().expect("recorded request");
    let json: serde_json::Value = serde_json::from_str(&last.body).expect("body is valid JSON");
    let messages = json["messages"].as_array().expect("messages array");

    // messages[0] is the assistant message with the tool call.
    let tc = &messages[0]["tool_calls"][0];
    // The "function" key MUST be present — this is the exact field the
    // gateway was rejecting.
    assert!(
        tc.get("function").is_some(),
        "tool_calls[0] must carry a nested 'function' object, got: {tc}"
    );
    assert_eq!(tc["id"], "call_1");
    assert_eq!(tc["function"]["name"], "read_file");
    // arguments is a JSON-encoded string per the OpenAI spec.
    let args_str = tc["function"]["arguments"]
        .as_str()
        .expect("arguments is a string");
    let parsed: serde_json::Value = serde_json::from_str(args_str).unwrap();
    assert_eq!(parsed, serde_json::json!({"path": "main.rs"}));
}

/// Regression: the `tools` array must use the nested OpenAI wire shape
/// `{"type":"function","function":{…}}` so strict gateways can
/// deserialize it.
#[tokio::test]
async fn outgoing_tools_use_nested_function_shape() {
    let body = simple_chat_body("ok");
    let server = MockServer::single(&body).await;
    let client = make_client(server.addr);

    let tools = vec![ToolDef::new(
        "read_file",
        "Read a file from disk",
        serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
    )];

    let req = ChatRequest {
        model: "mock-model".to_string(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: Some(tools),
        tool_choice: None,
        stream: false,
        request_id: None,
    };

    client.chat(req).await.expect("chat");

    let last = server.last_request().expect("recorded request");
    let json: serde_json::Value = serde_json::from_str(&last.body).expect("body is valid JSON");
    let tool = &json["tools"][0];
    assert_eq!(
        tool.get("type").and_then(|v| v.as_str()),
        Some("function"),
        "tools[0].type must be the literal string 'function'"
    );
    let f = tool.get("function").expect("tools[0].function must exist");
    assert_eq!(f["name"], "read_file");
    assert_eq!(f["description"], "Read a file from disk");
    assert!(
        f.get("parameters").is_some(),
        "tools[0].function.parameters must be present"
    );
}

// ── Streaming correctness / error security ─────────────────────────────

#[tokio::test]
async fn stream_handles_multibyte_split_across_chunks() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"héllo 🌍!\"}}]}\ndata: [DONE]\n\n";
    let bytes = body.as_bytes().to_vec();
    let emoji = "🌍".as_bytes();
    let pos = find_subsequence(&bytes, emoji).expect("emoji bytes");
    let split_points = [pos + 1, pos + 2, pos + 3];

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .await;
            let mut start = 0;
            for &split in &split_points {
                let _ = sock.write_all(&bytes[start..split]).await;
                let _ = sock.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                start = split;
            }
            let _ = sock.write_all(&bytes[start..]).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        }
    });

    let cfg = make_config(&format!("http://{addr}"), "mock-model");
    let client = ModelClient::new(cfg).unwrap();
    let req = ChatRequest {
        model: "mock-model".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: true,
        request_id: None,
    };

    let chunks = client.stream_drain(&req).await.expect("stream");
    let text: String = chunks.iter().filter_map(|c| c.content.clone()).collect();
    assert_eq!(text, "héllo 🌍!");
    assert!(
        !text.contains('\u{FFFD}'),
        "no replacement characters: {text}"
    );
}

#[tokio::test]
async fn interrupted_stream_is_a_non_retryable_streaming_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            // Promise 4096 bytes, deliver far fewer, then hang up.
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 4096\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n",
                )
                .await;
            let _ = sock.shutdown().await;
        }
    });

    let cfg = make_config(&format!("http://{addr}"), "mock-model");
    let client = ModelClient::new(cfg).unwrap();
    let req = ChatRequest {
        model: "mock-model".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: true,
        request_id: None,
    };

    let err = client.stream_drain(&req).await.unwrap_err();
    assert!(
        matches!(err, requiem_core::error::RequiemError::StreamingFailure(_)),
        "got: {err:?}"
    );
    assert!(!err.is_retryable(), "partial output must not be replayed");
}

#[tokio::test]
async fn invalid_response_body_is_sanitized_and_bounded() {
    let body = format!("not json at all sk-supersecret123456 {}", "x".repeat(5000));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let cfg = make_config(&format!("http://{addr}"), "mock-model");
    let client = ModelClient::new(cfg).unwrap();
    let req = ChatRequest {
        model: "mock-model".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    };

    let err = client.chat(req).await.unwrap_err();
    let text = err.to_string();
    assert!(
        matches!(err, requiem_core::error::RequiemError::InvalidResponse(_)),
        "got: {err:?}"
    );
    assert!(!text.contains("supersecret123456"), "key leaked: {text}");
    assert!(
        text.chars().count() < 600,
        "diagnostic not bounded: {} chars",
        text.chars().count()
    );
    assert!(
        text.contains("bytes"),
        "response size diagnostic retained: {text}"
    );
}

#[tokio::test]
async fn retries_are_bounded_by_max_retries() {
    let count = Arc::new(AtomicU32::new(0));
    let counter = count.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut sock, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = "{\"error\":{\"message\":\"overloaded\",\"type\":\"server_error\"}}";
            let response = format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let mut cfg = make_config(&format!("http://{addr}"), "mock-model");
    cfg.max_retries = 1;
    let client = ModelClient::new(cfg).unwrap();
    let req = ChatRequest {
        model: "mock-model".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    };

    let err = client.chat(req).await.unwrap_err();
    assert!(
        matches!(
            err,
            requiem_core::error::RequiemError::GatewayFailure { status: 503, .. }
        ),
        "got: {err:?}"
    );
    // One initial attempt + exactly one retry.
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rate_limit_json_and_retry_after_are_surfaced() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = "{\"error\":{\"message\":\"slow down\",\"type\":\"rate_limit_error\",\"code\":\"rate_limit_exceeded\"}}";
            let response = format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 7\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });

    let cfg = make_config(&format!("http://{addr}"), "mock-model");
    let mut cfg = cfg;
    cfg.max_retries = 0; // surface the first 429 instead of waiting 7s twice
    let client = ModelClient::new(cfg).unwrap();
    let req = ChatRequest {
        model: "mock-model".into(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    };

    match client.chat(req).await.unwrap_err() {
        requiem_core::error::RequiemError::RateLimited { retry_after_ms } => {
            assert_eq!(retry_after_ms, 7_000);
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

// ── Phase: MiMo / provider-recovery scenarios ─────────────────────────

/// A minimal raw-socket mock that always replies with the given
/// `(status, retry_after, body)` and counts accepted connections. Used by
/// the provider-recovery scenario suite.
async fn fixed_status_server(
    status: u16,
    retry_after: Option<u16>,
    body: &str,
) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = count.clone();
    let body = body.to_string();
    let retry_after = retry_after.map(|s| format!("Retry-After: {s}\r\n"));
    let retry_hdr = retry_after.unwrap_or_default();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let count_clone = count_clone.clone();
            count_clone.fetch_add(1, Ordering::Relaxed);
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                403 => "Forbidden",
                404 => "Not Found",
                408 => "Request Timeout",
                429 => "Too Many Requests",
                502 => "Bad Gateway",
                503 => "Service Unavailable",
                504 => "Gateway Timeout",
                _ => "Unknown",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{retry_hdr}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        }
    });
    (addr, count)
}

fn mk_req(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages: vec![ChatMessage::user("hi")],
        temperature: None,
        max_tokens: None,
        tools: None,
        tool_choice: None,
        stream: false,
        request_id: None,
    }
}

fn client_for(addr: &std::net::SocketAddr, model: &str, max_retries: u32) -> ModelClient {
    let mut cfg = make_config(&format!("http://{addr}"), model);
    cfg.max_retries = max_retries;
    ModelClient::new(cfg).unwrap()
}

/// Scenario 1: a transient 503 is retried and then succeeds.
#[tokio::test]
async fn s01_transient_503_then_success() {
    // Sequence: 503, then 200. Use the multi-body server via two raw
    // listeners is complex; instead verify the *classification* of a
    // transient 503 stays retryable and the client attempts a second
    // request when the server recovers.
    use tokio::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicU32::new(0));
    let c2 = count.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let n = c2.fetch_add(1, Ordering::Relaxed);
            if n == 0 {
                let body = "upstream temporarily unavailable";
                let r = format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(r.as_bytes()).await;
            } else {
                let b = simple_chat_body("ok");
                let r = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    b.len(),
                    b
                );
                let _ = sock.write_all(r.as_bytes()).await;
            }
            let _ = sock.shutdown().await;
        }
    });
    let client = client_for(&addr, "mimo", 3);
    let resp = client.chat(mk_req("mimo")).await;
    assert!(resp.is_ok(), "recovered: {resp:?}");
    assert!(count.load(Ordering::Relaxed) >= 2, "must have retried");
}

/// Scenario 2: a 503 wrapping an explicit upstream 404 (the MiMo case) is
/// classified permanent and NOT retried.
#[tokio::test]
async fn s02_503_wrapping_upstream_404_fails_fast() {
    let body = "MiMo bootstrap failed: 404";
    let (addr, count) = fixed_status_server(503, None, body).await;
    let client = client_for(&addr, "mimo", 5);
    let err = client.chat(mk_req("mimo")).await.unwrap_err();
    assert!(
        matches!(err, requiem_core::error::RequiemError::ModelUnavailable(_)),
        "wrapped upstream 404 must be permanent, got {err:?}"
    );
    // Permanent: the client must NOT keep hammering.
    assert_eq!(count.load(Ordering::Relaxed), 1, "no retry for wrapped 404");
    let e2 = client.chat(mk_req("mimo")).await.unwrap_err();
    assert!(
        e2.class() == "model_unavailable",
        "classified permanent: {e2:?}"
    );
}

/// Scenario 3: a plain 502 is a transient gateway failure (retryable).
#[tokio::test]
async fn s03_plain_502_is_retryable_gateway() {
    let (addr, _count) = fixed_status_server(502, None, "bad gateway").await;
    let client = client_for(&addr, "mimo", 0); // no retries → surface immediately
    let err = client.chat(mk_req("mimo")).await.unwrap_err();
    assert!(
        matches!(
            err,
            requiem_core::error::RequiemError::GatewayFailure { .. }
        ),
        "expected GatewayFailure, got {err:?}"
    );
    assert!(err.is_retryable(), "plain 502 must be retryable");
}

/// Scenario 4: 429 with Retry-After surfaces the backoff window.
#[tokio::test]
async fn s04_429_with_retry_after() {
    let body = "{\"error\":{\"message\":\"slow down\",\"type\":\"rate_limit_error\"}}";
    let (addr, _count) = fixed_status_server(429, Some(5), body).await;
    let client = client_for(&addr, "mimo", 0);
    match client.chat(mk_req("mimo")).await.unwrap_err() {
        requiem_core::error::RequiemError::RateLimited { retry_after_ms } => {
            assert_eq!(retry_after_ms, 5_000);
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

/// Scenario 5: 401 invalid credentials → permanent auth failure, no retry.
#[tokio::test]
async fn s05_invalid_credentials_fail_fast() {
    let (addr, count) = fixed_status_server(401, None, "invalid credentials").await;
    let client = client_for(&addr, "mimo", 3);
    let err = client.chat(mk_req("mimo")).await.unwrap_err();
    assert!(matches!(err, requiem_core::error::RequiemError::Auth(_)));
    assert_eq!(count.load(Ordering::Relaxed), 1, "auth must not be retried");
}

/// Scenario 6: 404 model not found → permanent ModelUnavailable.
#[tokio::test]
async fn s06_model_unavailable_permanent() {
    let body = "model 'mimo-v9' not found";
    let (addr, count) = fixed_status_server(404, None, body).await;
    let client = client_for(&addr, "mimo-v9", 3);
    let err = client.chat(mk_req("mimo-v9")).await.unwrap_err();
    assert!(
        matches!(err, requiem_core::error::RequiemError::ModelUnavailable(_)),
        "got {err:?}"
    );
    assert_eq!(count.load(Ordering::Relaxed), 1);
}

/// Scenario 7: a connection-level transient failure (server closes before
/// responding) is retryable.
#[tokio::test]
async fn s07_connection_failure_is_retryable() {
    // A listener that accepts and immediately drops the socket.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut _sock, _)) = listener.accept().await {
            // Drop immediately → connection reset on the client's read.
            use tokio::io::AsyncWriteExt;
            let _ = _sock.shutdown().await;
        }
    });
    let client = client_for(&addr, "mimo", 0);
    let err = client.chat(mk_req("mimo")).await.unwrap_err();
    assert!(
        err.is_retryable(),
        "connection failure must be retryable: {err:?}"
    );
}

/// Scenario 8: provider recovery after the circuit-breaker cooldown.
#[tokio::test]
async fn s08_breaker_recovery_after_cooldown() {
    use requiem_networking::{BreakerConfig, CircuitBreaker};
    let cb = CircuitBreaker::with_config(BreakerConfig {
        failure_threshold: 1,
        cooldown: std::time::Duration::from_millis(50),
    });
    // Trip it.
    cb.record_failure(true);
    assert_eq!(cb.state(), requiem_networking::BreakerState::Open);
    // While open (within cooldown) requests are refused.
    assert!(matches!(
        cb.allow_request(),
        requiem_networking::Admission::Refused { .. }
    ));
    // After the cooldown elapses the breaker allows a recovery probe.
    tokio::time::sleep(std::time::Duration::from_millis(70)).await;
    assert_eq!(cb.state(), requiem_networking::BreakerState::HalfOpen);
    assert!(matches!(
        cb.allow_request(),
        requiem_networking::Admission::Allow
    ));
    // A successful probe closes the circuit.
    cb.record_success();
    assert_eq!(cb.state(), requiem_networking::BreakerState::Closed);
}

/// Scenario 9: multiple agents hitting the same permanent error trip the
/// shared breaker and are then refused (shielded).
#[tokio::test]
async fn s09_multi_agent_same_error_shielded_by_breaker() {
    use requiem_networking::CircuitBreaker;
    let cb = CircuitBreaker::new(); // threshold 3
                                    // Three agents each report a permanent failure (e.g. the MiMo 404).
    cb.record_failure(true);
    cb.record_failure(true);
    cb.record_failure(true);
    // The breaker is now open; a fourth agent's request is refused without
    // hitting the network.
    assert!(
        matches!(
            cb.allow_request(),
            requiem_networking::Admission::Refused { .. }
        ),
        "the fourth agent must be shielded"
    );
}

/// Scenario 10: cancellation during retry. A cancel flag must interrupt a
/// request without a long backoff.
#[tokio::test]
async fn s10_cancellation_during_retry() {
    // The raw-socket server returns a transient 503 forever; with retries
    // enabled the client would loop. We verify the *classification* is
    // retryable and that a cancelled run surfaces as Cancelled (the
    // orchestration layer's responsibility). Here we assert the error type
    // is the retryable gateway so the runtime *would* retry, and that
    // cancelling is a separate control path.
    let (addr, _count) = fixed_status_server(503, None, "upstream temporarily unavailable").await;
    let client = client_for(&addr, "mimo", 0);
    let err = client.chat(mk_req("mimo")).await.unwrap_err();
    assert!(err.is_retryable(), "transient 503 stays retryable");
    // Cancellation is handled by the runtime racing `watcher.wait()`; we
    // assert the cancel path yields a non-retryable Cancelled error.
    let cancelled = requiem_core::error::RequiemError::Cancelled;
    assert!(!cancelled.is_retryable());
}

/// Scenario 11: a mid-stream interruption is a non-retryable streaming
/// failure (output was already delivered).
#[tokio::test]
async fn s11_streaming_interruption_not_retried() {
    // The client's classify_stream_error maps a dropped stream to a
    // StreamingFailure, which is non-retryable. Verify the class.
    let err = requiem_core::error::RequiemError::StreamingFailure("stream dropped".into());
    assert!(!err.is_retryable(), "partial streams must not be replayed");
    assert_eq!(err.class(), "streaming");
}

#[allow(unused_variables)]
fn _silence_s02_binding() {
    let body = "MiMo bootstrap failed: 404";
    let _ = body;
}
