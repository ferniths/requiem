//! The centralized proxy pool.
//!
//! Owns all configured proxy endpoints, their health state, and the
//! selection strategy. The model client consults the pool before each
//! request to get the current endpoint; on failure the pool rotates to
//! the next healthy endpoint.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::{NetworkConfig, ProxyEndpoint, SelectionStrategy};
use crate::health::HealthChecker;
use crate::rate_limit::{EndpointRateMap, RateLimitManager};

/// The result of selecting an endpoint for a request.
#[derive(Debug, Clone)]
pub enum EndpointSelection {
    /// Use this proxy endpoint for the request.
    ViaProxy {
        endpoint: ProxyEndpoint,
        /// The proxy URL to configure the reqwest client with.
        proxy_url: String,
    },
    /// No proxy: connect directly to the model endpoint.
    Direct,
}

impl EndpointSelection {
    /// Whether this selection routes through a proxy.
    pub fn is_proxy(&self) -> bool {
        matches!(self, Self::ViaProxy { .. })
    }
}

/// The centralized proxy pool.
pub struct ProxyPool {
    config: Arc<RwLock<NetworkConfig>>,
    health_checker: Arc<HealthChecker>,
    rate_map: Arc<RwLock<EndpointRateMap>>,
    /// The round-robin index for `RoundRobin` strategy.
    rr_index: Arc<std::sync::atomic::AtomicUsize>,
    /// Global rate-limit coordinator shared across all agents.
    rate_limiter: Arc<RateLimitManager>,
}

impl ProxyPool {
    /// Create a pool from the current network config.
    pub fn new(config: NetworkConfig, health_checker: Arc<HealthChecker>) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            health_checker,
            rate_map: Arc::new(RwLock::new(EndpointRateMap::new())),
            rr_index: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            rate_limiter: Arc::new(RateLimitManager::new()),
        }
    }

    /// The global rate-limit coordinator, shared by all agents.
    ///
    /// Call [`RateLimitManager::acquire`] before each request to block
    /// during a global 429 cooldown; call [`RateLimitManager::record_rate_limit`]
    /// when a 429 is received.
    pub fn rate_limiter(&self) -> &Arc<RateLimitManager> {
        &self.rate_limiter
    }

    /// Select the next endpoint for a request, honouring the active
    /// strategy and skipping endpoints in rate-limit cooldown.
    pub async fn select(&self) -> EndpointSelection {
        let config = self.config.read().await;
        if !config.proxy_enabled {
            return EndpointSelection::Direct;
        }

        // Manual strategy: return the pinned endpoint if it exists.
        if config.strategy == SelectionStrategy::Manual {
            if let Some(ep) = config.pinned() {
                if !self.rate_map.read().await.is_cooling_down(&ep.id) {
                    return EndpointSelection::ViaProxy {
                        endpoint: ep.clone(),
                        proxy_url: ep.url.clone(),
                    };
                }
            }
            // Pinned endpoint is cooling down or absent — fall through to
            // round-robin over healthy endpoints.
        }

        let healthy = config.healthy_endpoints();
        if healthy.is_empty() {
            return EndpointSelection::Direct;
        }

        let rate_map = self.rate_map.read().await;
        let available: Vec<ProxyEndpoint> = healthy
            .iter()
            .filter(|ep| !rate_map.is_cooling_down(&ep.id))
            .map(|ep| (**ep).clone())
            .collect();
        drop(rate_map);

        if available.is_empty() {
            // All healthy endpoints are cooling down. Return direct rather
            // than block; the rate-limit manager will gate the actual
            // request.
            return EndpointSelection::Direct;
        }

        let ep = match config.strategy {
            SelectionStrategy::RoundRobin => {
                let idx = self
                    .rr_index
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                available[idx % available.len()].clone()
            }
            SelectionStrategy::LowestLatency => available
                .iter()
                .min_by_key(|e| e.last_latency_ms.unwrap_or(u64::MAX))
                .cloned()
                .unwrap_or_else(|| available[0].clone()),
            SelectionStrategy::HealthBased => available
                .iter()
                .min_by_key(|e| e.failure_count)
                .cloned()
                .unwrap_or_else(|| available[0].clone()),
            SelectionStrategy::Manual => available[0].clone(),
        };

        EndpointSelection::ViaProxy {
            proxy_url: ep.url.clone(),
            endpoint: ep,
        }
    }

    /// Record a successful request through the given endpoint.
    pub async fn record_success(&self, endpoint_id: &uuid::Uuid, latency_ms: u64) {
        self.rate_map.write().await.record_success(endpoint_id);
        let mut config = self.config.write().await;
        if let Some(ep) = config.endpoints.iter_mut().find(|e| e.id == *endpoint_id) {
            ep.failure_count = 0;
            ep.last_latency_ms = Some(latency_ms);
            ep.last_error = None;
            ep.last_success = Some(chrono::Utc::now().to_rfc3339());
        }
    }

    /// Record a failure for the given endpoint.
    pub async fn record_failure(&self, endpoint_id: &uuid::Uuid, error: &str) {
        let mut config = self.config.write().await;
        if let Some(ep) = config.endpoints.iter_mut().find(|e| e.id == *endpoint_id) {
            ep.failure_count += 1;
            ep.last_error = Some(error.to_string());
        }
    }

    /// Record a 429 rate-limit response for the given endpoint.
    pub async fn record_rate_limit(&self, endpoint_id: &uuid::Uuid, retry_after_ms: u64) {
        self.rate_map
            .write()
            .await
            .record_429(endpoint_id, retry_after_ms);
    }

    /// Run a health check against all enabled endpoints and update the
    /// config with the results.
    pub async fn health_check_all(
        &self,
    ) -> Vec<(ProxyEndpoint, crate::health::HealthStatus, Option<u64>)> {
        let config = self.config.read().await;
        let mut results = Vec::new();
        for ep in config.endpoints.iter().filter(|e| e.enabled) {
            let (status, latency, _time) = self.health_checker.check(ep).await;
            results.push((ep.clone(), status, latency));
        }

        // Update the config with new health data.
        let mut config = self.config.write().await;
        for (ep, status, latency) in &results {
            if let Some(stored) = config.endpoints.iter_mut().find(|e| e.id == ep.id) {
                match status {
                    crate::health::HealthStatus::Healthy => {
                        stored.failure_count = 0;
                        stored.last_latency_ms = *latency;
                        stored.last_error = None;
                    }
                    crate::health::HealthStatus::Unhealthy => {
                        stored.failure_count += 1;
                    }
                    crate::health::HealthStatus::Unknown => {}
                }
            }
        }
        drop(config);

        results
    }

    /// Add a new endpoint to the pool.
    pub async fn add_endpoint(&self, endpoint: ProxyEndpoint) {
        let mut config = self.config.write().await;
        config.endpoints.push(endpoint);
    }

    /// Remove an endpoint from the pool by id.
    pub async fn remove_endpoint(&self, endpoint_id: &uuid::Uuid) -> bool {
        let mut config = self.config.write().await;
        let before = config.endpoints.len();
        config.endpoints.retain(|e| e.id != *endpoint_id);
        config.endpoints.len() < before
    }

    /// Enable or disable proxy routing.
    pub async fn set_proxy_enabled(&self, enabled: bool) {
        let mut config = self.config.write().await;
        config.proxy_enabled = enabled;
    }

    /// Set the active selection strategy.
    pub async fn set_strategy(&self, strategy: SelectionStrategy) {
        let mut config = self.config.write().await;
        config.strategy = strategy;
    }

    /// Pin a specific endpoint for `Manual` strategy.
    pub async fn set_pinned(&self, endpoint_id: Option<uuid::Uuid>) {
        let mut config = self.config.write().await;
        config.pinned_endpoint = endpoint_id;
        if let Some(id) = &endpoint_id {
            if let Some(ep) = config.endpoints.iter_mut().find(|e| e.id == *id) {
                ep.pinned = true;
            }
        } else {
            for ep in config.endpoints.iter_mut() {
                ep.pinned = false;
            }
        }
    }

    /// Rotate to the next healthy endpoint (used by `/proxy rotate`).
    pub async fn rotate(&self) -> Option<ProxyEndpoint> {
        let config = self.config.read().await;
        let healthy: Vec<_> = config.healthy_endpoints().to_vec();
        if healthy.is_empty() {
            return None;
        }
        // Advance the round-robin index and return the next endpoint.
        let idx = self
            .rr_index
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % healthy.len();
        Some(healthy[idx].clone())
    }

    /// Get the current config snapshot.
    pub async fn config(&self) -> NetworkConfig {
        self.config.read().await.clone()
    }

    /// Persist the current config to the shared writer (called by the
    /// CLI after each mutation).
    pub async fn config_snapshot(&self) -> NetworkConfig {
        self.config.read().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NetworkConfig, ProviderKind, ProxyProtocol};
    use crate::health::HealthChecker;
    use reqwest::Client;

    fn test_pool() -> ProxyPool {
        let client = Client::new();
        let checker = Arc::new(HealthChecker::new(&client, 1_000));
        ProxyPool::new(NetworkConfig::default(), checker)
    }

    #[tokio::test]
    async fn direct_when_proxy_disabled() {
        let pool = test_pool();
        let sel = pool.select().await;
        assert!(!sel.is_proxy());
    }

    #[tokio::test]
    async fn select_returns_direct_when_no_endpoints() {
        let config = NetworkConfig {
            proxy_enabled: true,
            ..NetworkConfig::default()
        };
        let client = Client::new();
        let checker = Arc::new(HealthChecker::new(&client, 1_000));
        let pool = ProxyPool::new(config, checker);
        let sel = pool.select().await;
        assert!(!sel.is_proxy());
    }

    #[tokio::test]
    async fn select_returns_proxy_when_enabled_with_endpoint() {
        let mut config = NetworkConfig {
            proxy_enabled: true,
            ..NetworkConfig::default()
        };
        let ep = ProxyEndpoint::new(
            "test",
            ProviderKind::None,
            "http://proxy:8080",
            ProxyProtocol::Http,
        );
        config.endpoints.push(ep);
        let client = Client::new();
        let checker = Arc::new(HealthChecker::new(&client, 1_000));
        let pool = ProxyPool::new(config, checker);
        let sel = pool.select().await;
        assert!(sel.is_proxy());
    }

    #[tokio::test]
    async fn add_and_remove_endpoint() {
        let pool = test_pool();
        let ep = ProxyEndpoint::new(
            "test",
            ProviderKind::None,
            "http://new:8080",
            ProxyProtocol::Http,
        );
        let id = ep.id;
        pool.add_endpoint(ep).await;
        assert_eq!(pool.config().await.endpoints.len(), 1);
        assert!(pool.remove_endpoint(&id).await);
        assert_eq!(pool.config().await.endpoints.len(), 0);
    }

    #[tokio::test]
    async fn record_failure_increments_count() {
        let mut config = NetworkConfig {
            proxy_enabled: true,
            ..NetworkConfig::default()
        };
        let ep = ProxyEndpoint::new(
            "test",
            ProviderKind::None,
            "http://fail:8080",
            ProxyProtocol::Http,
        );
        let id = ep.id;
        config.endpoints.push(ep);
        let client = Client::new();
        let checker = Arc::new(HealthChecker::new(&client, 1_000));
        let pool = ProxyPool::new(config, checker);

        pool.record_failure(&id, "connection refused").await;
        let cfg = pool.config().await;
        assert_eq!(cfg.endpoints[0].failure_count, 1);

        pool.record_success(&id, 50).await;
        let cfg = pool.config().await;
        assert_eq!(cfg.endpoints[0].failure_count, 0);
    }
}
