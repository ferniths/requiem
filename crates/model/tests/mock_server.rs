//! Integration tests for the model client using a hand-rolled mock HTTP server.
//!
//! A tokio TCP listener accepts one connection, reads the request, and returns
//! a canned OpenAI-compatible JSON response. This exercises the full HTTP path
//! of `ModelClient::chat` without any external dependency.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use requiem_model::client::ModelClient;
use requiem_model::config::ModelConfig;
use requiem_model::request::{ChatMessage, ChatRequest};
use requiem_model::response::ModelResponse;

/// Spin up a mock HTTP server that returns `body` for every request.
/// Returns the bound address and a shutdown signal.
async fn start_mock_server(body: &str) -> (SocketAddr, Arc<AtomicU32>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let signal = Arc::new(AtomicU32::new(0));
    let sig_clone = signal.clone();
    let body_bytes = body.to_string();

    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            if sig_clone.load(Ordering::Relaxed) == 1 {
                break;
            }
            // Read and discard the request so the socket is fully consumed
            // before we send the response.
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;

            use tokio::io::AsyncWriteExt;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body_bytes.len(),
                body_bytes
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        }
    });

    (addr, signal)
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

#[tokio::test]
async fn chat_returns_parsed_response() {
    let body = r#"{
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": "mock-model",
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello, world!"
                },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    }"#;

    let (addr, sig) = start_mock_server(body).await;
    let client = make_client(addr);

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

    sig.store(1, Ordering::Relaxed);
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

    let (addr, sig) = start_mock_server(body).await;
    let client = make_client(addr);

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

    sig.store(1, Ordering::Relaxed);
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

    let (addr, sig) = start_mock_server(body).await;
    let client = make_client(addr);
    let model = client.test_connection().await.expect("probe");
    assert_eq!(model, "mock-model");

    sig.store(1, Ordering::Relaxed);
}

#[tokio::test]
async fn stream_drain_returns_chunks() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo!\"}}]}\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\n";

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let signal = Arc::new(AtomicU32::new(0));
    let sig_clone = signal.clone();
    let sse_body = body.to_string();

    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            if sig_clone.load(Ordering::Relaxed) == 1 {
                break;
            }
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;

            use tokio::io::AsyncWriteExt;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse_body.len(),
                sse_body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
            let _ = sock.shutdown().await;
        }
    });

    let endpoint = format!("http://{}", addr);
    let config = make_config(&endpoint, "mock-model");
    let client = ModelClient::new(config).expect("client");

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

    signal.store(1, Ordering::Relaxed);
}
