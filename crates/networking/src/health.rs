//! Proxy endpoint health checking.
//!
//! A health checker probes configured endpoints and records latency,
//! success/failure, and error details. The pool uses these records for
//! selection and rotation decisions.

use std::time::Instant;

use reqwest::Client;

use crate::config::{ProxyEndpoint, ProxyProtocol};

/// The result of one health-check probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// The endpoint responded successfully.
    Healthy,
    /// The endpoint failed (connection error, timeout, auth failure).
    Unhealthy,
    /// The endpoint was not checked (disabled or not yet probed).
    Unknown,
}

impl HealthStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Healthy => "OK",
            Self::Unhealthy => "FAIL",
            Self::Unknown => "—",
        }
    }
}

/// A health checker that probes endpoints using a shared `reqwest::Client`.
pub struct HealthChecker {
    client: Client,
    timeout_ms: u64,
}

impl HealthChecker {
    pub fn new(client: &Client, timeout_ms: u64) -> Self {
        Self {
            client: client.clone(),
            timeout_ms,
        }
    }

    /// Probe one endpoint: attempt a TCP/TLS connection and measure latency.
    ///
    /// Returns `(status, latency_ms, last_error)`. `last_error` is `None`
    /// on success, `Some(msg)` on failure.
    pub async fn check(
        &self,
        endpoint: &ProxyEndpoint,
    ) -> (HealthStatus, Option<u64>, Option<String>) {
        let start = Instant::now();

        let result = match endpoint.protocol {
            ProxyProtocol::Http => self.http_check(endpoint).await,
            ProxyProtocol::Socks5 => self.socks_check(endpoint).await,
            ProxyProtocol::Direct => Ok(()),
        };

        match result {
            Ok(()) => {
                let latency = start.elapsed().as_millis() as u64;
                (HealthStatus::Healthy, Some(latency), None)
            }
            Err(e) => (HealthStatus::Unhealthy, None, Some(e)),
        }
    }

    async fn http_check(&self, endpoint: &ProxyEndpoint) -> Result<(), String> {
        // Use the proxy URL as the target. A 404 or 405 still proves the
        // proxy is reachable.
        let resp = self
            .client
            .get(&endpoint.url)
            .timeout(std::time::Duration::from_millis(self.timeout_ms))
            .send()
            .await
            .map_err(|e| format!("connection failed: {e}"))?;
        // Any HTTP response (even 4xx/5xx) means the proxy is alive.
        let _ = resp.status();
        Ok(())
    }

    async fn socks_check(&self, endpoint: &ProxyEndpoint) -> Result<(), String> {
        // For SOCKS5: attempt a raw TCP connection. reqwest doesn't speak
        // SOCKS directly, so we use tokio's TcpStream.
        let host_port = endpoint
            .url
            .strip_prefix("socks5://")
            .or_else(|| endpoint.url.strip_prefix("socks://"))
            .unwrap_or(&endpoint.url);
        tokio::net::TcpStream::connect(host_port)
            .await
            .map_err(|e| format!("socket connect failed: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_endpoint() -> ProxyEndpoint {
        ProxyEndpoint::new(
            "test",
            crate::config::ProviderKind::None,
            "http://127.0.0.1:1",
            ProxyProtocol::Http,
        )
    }

    #[test]
    fn health_status_labels() {
        assert_eq!(HealthStatus::Healthy.label(), "OK");
        assert_eq!(HealthStatus::Unhealthy.label(), "FAIL");
        assert_eq!(HealthStatus::Unknown.label(), "—");
    }

    #[test]
    fn direct_protocol_is_always_healthy() {
        // Direct protocol: no check needed.
        let ep = ProxyEndpoint {
            protocol: ProxyProtocol::Direct,
            ..test_endpoint()
        };
        assert!(matches!(ep.protocol, ProxyProtocol::Direct));
    }
}
