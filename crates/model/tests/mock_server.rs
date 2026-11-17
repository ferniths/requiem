//! Integration tests for the model client using a hand-rolled mock HTTP server.
//!
//! A tokio TCP listener accepts connections, records every request (method,
//! path, headers, body), and returns a canned OpenAI-compatible response.
//! Bodies can be a single static payload or an ordered sequence, enabling
//! deterministic multi-turn agent-loop tests without external dependencies.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
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

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
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

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
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
    assert!(matches!(
        err,
        requiem_core::error::RequiemError::Model {
            status: Some(401),
            ..
        }
    ));
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
    let json: serde_json::Value =
        serde_json::from_str(&last.body).expect("body is valid JSON");
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
    let args_str = tc["function"]["arguments"].as_str().expect("arguments is a string");
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
    let json: serde_json::Value =
        serde_json::from_str(&last.body).expect("body is valid JSON");
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
