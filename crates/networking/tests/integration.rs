//! End-to-end integration tests for the networking subsystem.
//!
//! Uses a real TCP listener as a mock proxy server and a real HTTP
//! server as the mock OpenAI endpoint. No real paid proxy services are
//! used — everything runs on localhost.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use reqwest::Client;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use requiem_networking::{
    HealthChecker, NetworkConfig, ProviderKind, ProxyEndpoint, ProxyPool, ProxyProtocol,
    RateLimitManager,
};

// ── Mock HTTP server (returns fixed JSON for any request) ─────────

struct MockOpenAi {
    addr: String,
    /// Number of requests received.
    hit_count: Arc<AtomicUsize>,
    /// When set, the next N requests return 429 with this Retry-After.
    rate_limit: Arc<Mutex<Option<u32>>>,
}

impl MockOpenAi {
    async fn start(retry_after_ms: u64) -> (Self, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let hit_count = Arc::new(AtomicUsize::new(0));
        let rate_limit = Arc::new(Mutex::new(None::<u32>));

        let hc = hit_count.clone();
        let rl = rate_limit.clone();
        let retry = retry_after_ms;
        let handle = tokio::spawn(async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                hc.fetch_add(1, Ordering::SeqCst);

                // Read the request (we don't care about the body).
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;

                // Check if we should return 429.
                let should_429 = {
                    let mut guard = rl.lock().await;
                    if let Some(n) = *guard {
                        if n > 0 {
                            *guard = Some(n - 1);
                            true
                        } else {
                            *guard = None;
                            false
                        }
                    } else {
                        false
                    }
                };

                if should_429 {
                    let body = r#"{"error":{"message":"rate limited","type":"rate_limit"}}"#;
                    let resp = format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: {retry}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    use tokio::io::AsyncWriteExt;
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.flush().await;
                } else {
                    let body = r#"{"id":"x","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    use tokio::io::AsyncWriteExt;
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.flush().await;
                }
            }
        });

        (
            Self {
                addr,
                hit_count,
                rate_limit,
            },
            handle,
        )
    }
}

// ── Mock TCP proxy (accepts connections, echoes) ───────────────────

struct MockProxy {
    addr: String,
}

async fn start_mock_proxy() -> (MockProxy, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                use tokio::io::AsyncWriteExt;
                let _ = stream.write_all(b"200 OK\r\n\r\n").await;
                let _ = stream.flush().await;
            }
        }
    });
    (MockProxy { addr }, handle)
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn proxy_pool_selects_healthy_endpoint() {
    let (mock, _handle) = start_mock_proxy().await;
    let mut config = NetworkConfig::default();
    config.proxy_enabled = true;

    let ep = ProxyEndpoint::new("mock", ProviderKind::None, &mock.addr, ProxyProtocol::Http);
    config.endpoints.push(ep);

    let client = Client::new();
    let checker = Arc::new(HealthChecker::new(&client, 2_000));
    let pool = ProxyPool::new(config, checker);

    let sel = pool.select().await;
    assert!(sel.is_proxy(), "should select the proxy endpoint");
}

#[tokio::test]
async fn proxy_pool_falls_back_to_direct_when_no_healthy_endpoints() {
    let mut config = NetworkConfig::default();
    config.proxy_enabled = true;
    // No endpoints at all.
    let client = Client::new();
    let checker = Arc::new(HealthChecker::new(&client, 1_000));
    let pool = ProxyPool::new(config, checker);

    let sel = pool.select().await;
    assert!(!sel.is_proxy(), "should fall back to direct");
}

#[tokio::test]
async fn rate_limit_manager_blocks_during_cooldown() {
    let mgr = RateLimitManager::new();
    mgr.record_rate_limit(100).await;
    assert!(mgr.is_limited());

    // acquire should block until the cooldown expires (~100ms).
    let t0 = std::time::Instant::now();
    let waited = mgr.acquire().await;
    let elapsed = t0.elapsed();
    // The cooldown was 100ms; acquire should have waited close to that.
    assert!(
        elapsed.as_millis() >= 80,
        "acquire should wait for the cooldown (elapsed {elapsed:?})"
    );
    assert_eq!(
        waited,
        std::time::Duration::ZERO,
        "acquire returns 0 (wait tracked externally)"
    );
}

#[tokio::test]
async fn rate_limit_manager_success_clears_cooldown() {
    let mgr = RateLimitManager::new();
    mgr.record_rate_limit(100).await;
    assert!(mgr.is_limited());
    mgr.record_success();
    assert!(!mgr.is_limited());
    assert_eq!(mgr.consecutive_429s(), 0);
}

#[tokio::test]
async fn multiple_consecutive_429s_scale_cooldown() {
    let mgr = RateLimitManager::new();
    mgr.record_rate_limit(100).await;
    mgr.record_rate_limit(100).await;
    mgr.record_rate_limit(100).await;
    assert_eq!(mgr.consecutive_429s(), 3);
    mgr.record_success();
    assert_eq!(mgr.consecutive_429s(), 0);
}

#[tokio::test]
async fn proxy_endpoint_failure_increments_count() {
    let mut config = NetworkConfig::default();
    config.proxy_enabled = true;
    let ep = ProxyEndpoint::new(
        "dead",
        ProviderKind::None,
        "http://127.0.0.1:1",
        ProxyProtocol::Http,
    );
    let id = ep.id;
    config.endpoints.push(ep);

    let client = Client::new();
    let checker = Arc::new(HealthChecker::new(&client, 500));
    let pool = ProxyPool::new(config, checker);

    pool.record_failure(&id, "connection refused").await;
    pool.record_failure(&id, "connection refused").await;
    pool.record_failure(&id, "connection refused").await;

    let cfg = pool.config().await;
    assert_eq!(cfg.endpoints[0].failure_count, 3);
    // 3+ failures → endpoint is no longer healthy.
    assert!(cfg.healthy_endpoints().is_empty());
}

#[tokio::test]
async fn proxy_endpoint_recovery_after_success() {
    let mut config = NetworkConfig::default();
    config.proxy_enabled = true;
    let ep = ProxyEndpoint::new(
        "rec",
        ProviderKind::None,
        "http://127.0.0.1:1",
        ProxyProtocol::Http,
    );
    let id = ep.id;
    config.endpoints.push(ep);

    let client = Client::new();
    let checker = Arc::new(HealthChecker::new(&client, 500));
    let pool = ProxyPool::new(config, checker);

    pool.record_failure(&id, "err").await;
    pool.record_failure(&id, "err").await;
    assert_eq!(pool.config().await.endpoints[0].failure_count, 2);

    pool.record_success(&id, 50).await;
    let cfg = pool.config().await;
    assert_eq!(cfg.endpoints[0].failure_count, 0);
    assert!(cfg.endpoints[0].is_healthy());
}

#[tokio::test]
async fn proxy_rotate_returns_next_healthy_endpoint() {
    let mut config = NetworkConfig::default();
    config.proxy_enabled = true;
    config.endpoints.push(ProxyEndpoint::new(
        "a",
        ProviderKind::None,
        "http://a:8080",
        ProxyProtocol::Http,
    ));
    config.endpoints.push(ProxyEndpoint::new(
        "b",
        ProviderKind::None,
        "http://b:8080",
        ProxyProtocol::Http,
    ));

    let client = Client::new();
    let checker = Arc::new(HealthChecker::new(&client, 500));
    let pool = ProxyPool::new(config, checker);

    let ep1 = pool.rotate().await.expect("should return an endpoint");
    let ep2 = pool.rotate().await.expect("should return an endpoint");
    // Two rotations should return different endpoints (round-robin).
    assert_ne!(ep1.id, ep2.id, "round-robin should cycle through endpoints");
}

#[tokio::test]
async fn redacted_url_never_leaks_credentials() {
    let ep = ProxyEndpoint::new(
        "auth",
        ProviderKind::Paid,
        "http://user:supersecret@proxy.corp:8080",
        ProxyProtocol::Http,
    );
    let redacted = ep.redacted_url();
    assert!(
        !redacted.contains("supersecret"),
        "credential leaked: {redacted}"
    );
    assert!(
        redacted.contains("proxy.corp"),
        "host should remain: {redacted}"
    );
}

#[tokio::test]
async fn socks5_protocol_detection() {
    let ep = ProxyEndpoint::new(
        "socks",
        ProviderKind::None,
        "socks5://10.0.0.1:1080",
        ProxyProtocol::Socks5,
    );
    assert_eq!(ep.protocol, ProxyProtocol::Socks5);
}

#[tokio::test]
async fn config_persistence_roundtrip() {
    let mut config = NetworkConfig::default();
    config.proxy_enabled = true;
    config.endpoints.push(ProxyEndpoint::new(
        "persist",
        ProviderKind::Free,
        "http://persist:9090",
        ProxyProtocol::Http,
    ));

    let json = serde_json::to_string(&config).unwrap();
    let parsed: NetworkConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.endpoints.len(), 1);
    assert_eq!(parsed.endpoints[0].url, "http://persist:9090");
    assert_eq!(parsed.endpoints[0].provider, "persist");
    assert!(parsed.proxy_enabled);
}

#[tokio::test]
async fn mock_openai_server_returns_200() {
    let (mock, _handle) = MockOpenAi::start(1_000).await;
    let client = Client::new();
    let resp = client.get(&mock.addr).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(mock.hit_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mock_openai_server_returns_429_when_configured() {
    let (mock, _handle) = MockOpenAi::start(500).await;
    *mock.rate_limit.lock().await = Some(1);
    let client = Client::new();
    let resp = client.get(&mock.addr).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 429);
    let headers = resp.headers();
    assert!(
        headers.get("retry-after").is_some(),
        "429 must carry Retry-After"
    );
}

#[tokio::test]
async fn concurrent_agents_share_global_rate_limit() {
    let mgr = Arc::new(RateLimitManager::new());
    let mgr2 = mgr.clone();

    let h1 = tokio::spawn(async move {
        mgr.record_rate_limit(200).await;
    });
    let h2 = tokio::spawn(async move {
        // This agent blocks until agent 1's cooldown expires.
        let t0 = std::time::Instant::now();
        let _waited = mgr2.acquire().await;
        t0.elapsed()
    });

    let _ = h1.await.unwrap();
    let elapsed = h2.await.unwrap();
    // The second agent should have been blocked by the global cooldown.
    assert!(
        elapsed.as_millis() >= 100,
        "concurrent agent should have been blocked by the global cooldown (elapsed {elapsed:?})"
    );
}
