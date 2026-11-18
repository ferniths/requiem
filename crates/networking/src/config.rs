//! Network configuration: proxy endpoints, provider abstractions, and
//! selection strategies. All types are serde-serializable for TOML
//! persistence in `~/.requiem/config.toml`.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Protocol ─────────────────────────────────────────────────────────

/// The transport protocol a proxy endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProxyProtocol {
    /// No proxy: connect directly to the model endpoint.
    #[default]
    Direct,
    /// HTTP/HTTPS CONNECT tunnel.
    Http,
    /// SOCKS version 5.
    Socks5,
}

impl fmt::Display for ProxyProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Direct => "direct",
            Self::Http => "http",
            Self::Socks5 => "socks5",
        };
        write!(f, "{s}")
    }
}

// ── Selection strategy ───────────────────────────────────────────────

/// How the proxy pool picks the next endpoint for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SelectionStrategy {
    /// Cycle through healthy endpoints in order.
    #[default]
    RoundRobin,
    /// Pick the endpoint with the lowest measured latency.
    LowestLatency,
    /// Pick the endpoint with the best health score.
    HealthBased,
    /// Always use the manually-pinned endpoint (if set).
    Manual,
}

impl fmt::Display for SelectionStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::RoundRobin => "round-robin",
            Self::LowestLatency => "lowest-latency",
            Self::HealthBased => "health-based",
            Self::Manual => "manual",
        };
        write!(f, "{s}")
    }
}

// ── Provider kind ────────────────────────────────────────────────────

/// The class of proxy provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// No provider: direct connection or user-entered custom endpoint.
    #[default]
    None,
    /// A free-tier provider with documented public API.
    Free,
    /// A paid provider with API credentials.
    Paid,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::None => "none",
            Self::Free => "free",
            Self::Paid => "paid",
        };
        write!(f, "{s}")
    }
}

// ── Proxy endpoint ───────────────────────────────────────────────────

/// One configured proxy endpoint in the pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyEndpoint {
    /// Stable identifier (UUID v4).
    pub id: Uuid,
    /// Human-readable provider name (e.g. `"custom"`, `"openrouter-free"`).
    pub provider: String,
    /// The provider class.
    pub kind: ProviderKind,
    /// The proxy URL (e.g. `http://user:pass@host:port`, `socks5://host:port`).
    pub url: String,
    /// The transport protocol.
    pub protocol: ProxyProtocol,
    /// Whether this endpoint is enabled for routing.
    #[serde(default)]
    pub enabled: bool,
    /// Whether this endpoint is manually pinned (used by `Manual` strategy).
    #[serde(default)]
    pub pinned: bool,
    /// The last measured round-trip latency in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_latency_ms: Option<u64>,
    /// Consecutive failure count (resets on success).
    #[serde(default)]
    pub failure_count: u32,
    /// The last error message, if the most recent check failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// The timestamp (RFC 3339) of the last successful health check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success: Option<String>,
    /// Known usage limit (requests per minute), if the provider reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_rpm: Option<u32>,
}

impl ProxyEndpoint {
    /// Create a new endpoint with a fresh UUID.
    pub fn new(provider: &str, kind: ProviderKind, url: &str, protocol: ProxyProtocol) -> Self {
        Self {
            id: Uuid::new_v4(),
            provider: provider.to_string(),
            kind,
            url: url.to_string(),
            protocol,
            enabled: true,
            pinned: false,
            last_latency_ms: None,
            failure_count: 0,
            last_error: None,
            last_success: None,
            usage_limit_rpm: None,
        }
    }

    /// Whether this endpoint is currently healthy (no consecutive failures).
    pub fn is_healthy(&self) -> bool {
        self.enabled && self.failure_count < 3
    }

    /// A redacted display string that never leaks credentials.
    pub fn redacted_url(&self) -> String {
        // Strip userinfo from the URL for display.
        if let Some(pos) = self.url.find("://") {
            let after_scheme = &self.url[pos + 3..];
            if let Some(at) = after_scheme.find('@') {
                let host_part = &after_scheme[at + 1..];
                let scheme = &self.url[..pos];
                return format!("{scheme}://{host_part}");
            }
        }
        self.url.clone()
    }
}

// ── Network config ────────────────────────────────────────────────────

/// The complete networking section of REQUIEM's TOML config.
///
/// Embedded in `Config` as `network`. All fields have serde defaults so
/// existing configs without this section deserialize with `Direct`
/// protocol and an empty pool — zero behaviour change for existing users.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// All configured proxy endpoints.
    #[serde(default)]
    pub endpoints: Vec<ProxyEndpoint>,
    /// The active selection strategy.
    #[serde(default)]
    pub strategy: SelectionStrategy,
    /// The id of the manually-pinned endpoint (used when strategy is Manual).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_endpoint: Option<Uuid>,
    /// Whether proxy routing is enabled (false = direct connection only).
    #[serde(default)]
    pub proxy_enabled: bool,
    /// Health-check interval in milliseconds.
    #[serde(default = "default_health_interval")]
    pub health_check_ms: u64,
    /// Connection timeout in milliseconds for proxy health checks.
    #[serde(default = "default_proxy_timeout")]
    pub proxy_timeout_ms: u64,
}

fn default_health_interval() -> u64 {
    30_000
}
fn default_proxy_timeout() -> u64 {
    5_000
}

impl NetworkConfig {
    /// The currently-pinned endpoint, if strategy is Manual and the id is set.
    pub fn pinned(&self) -> Option<&ProxyEndpoint> {
        if self.strategy != SelectionStrategy::Manual {
            return None;
        }
        self.pinned_endpoint.as_ref().and_then(|id| {
            self.endpoints
                .iter()
                .find(|e| e.id == *id && e.pinned && e.enabled)
        })
    }

    /// All healthy, enabled endpoints in round-robin order.
    pub fn healthy_endpoints(&self) -> Vec<&ProxyEndpoint> {
        self.endpoints
            .iter()
            .filter(|e| e.enabled && e.is_healthy())
            .collect()
    }

    /// Whether the pool has at least one usable endpoint.
    pub fn has_usable_endpoint(&self) -> bool {
        self.proxy_enabled && !self.healthy_endpoints().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_direct() {
        let config = NetworkConfig::default();
        assert!(!config.proxy_enabled);
        assert!(config.endpoints.is_empty());
        assert_eq!(config.strategy, SelectionStrategy::RoundRobin);
        assert!(!config.has_usable_endpoint());
    }

    #[test]
    fn healthy_endpoint_list_filters_unhealthy() {
        let mut config = NetworkConfig::default();
        config.proxy_enabled = true;
        let mut good = ProxyEndpoint::new("test", ProviderKind::None, "http://good:8080", ProxyProtocol::Http);
        good.failure_count = 0;
        let mut bad = ProxyEndpoint::new("test", ProviderKind::None, "http://bad:8080", ProxyProtocol::Http);
        bad.failure_count = 5;
        config.endpoints.push(good);
        config.endpoints.push(bad);
        assert_eq!(config.healthy_endpoints().len(), 1);
    }

    #[test]
    fn pinned_returns_only_when_manual_strategy() {
        let mut config = NetworkConfig::default();
        let mut ep = ProxyEndpoint::new("pinned", ProviderKind::None, "http://pin:8080", ProxyProtocol::Http);
        ep.pinned = true;
        let id = ep.id;
        config.endpoints.push(ep);
        config.pinned_endpoint = Some(id);
        // Not manual → no pin.
        assert!(config.pinned().is_none());
        config.strategy = SelectionStrategy::Manual;
        assert!(config.pinned().is_some());
    }

    #[test]
    fn redacted_url_strips_credentials() {
        let ep = ProxyEndpoint::new(
            "test",
            ProviderKind::None,
            "http://user:secret@proxy:8080",
            ProxyProtocol::Http,
        );
        assert_eq!(ep.redacted_url(), "http://proxy:8080");
        // No credentials → unchanged.
        let ep2 = ProxyEndpoint::new(
            "test",
            ProviderKind::None,
            "http://plain:8080",
            ProxyProtocol::Http,
        );
        assert_eq!(ep2.redacted_url(), "http://plain:8080");
    }

    #[test]
    fn endpoint_deserialization_tolerates_missing_fields() {
        let raw = r#"{ "id": "00000000-0000-0000-0000-000000000000", "provider": "x", "kind": "none", "url": "http://x:1", "protocol": "http", "enabled": true }"#;
        let ep: ProxyEndpoint = serde_json::from_str(raw).unwrap();
        assert!(ep.last_latency_ms.is_none());
        assert_eq!(ep.failure_count, 0);
    }
}
