//! REQUIEM networking subsystem.
//!
//! Proxy pool, rate-limit coordination, and network configuration for the
//! shared 9router model client. All agents route through the same
//! networking layer — proxy selection, health tracking, and global
//! rate-limit backpressure are coordinated here, not per-agent.

pub mod config;
pub mod pool;
pub mod provider;
pub mod rate_limit;
pub mod health;

pub use config::{NetworkConfig, ProviderKind, ProxyEndpoint, ProxyProtocol, SelectionStrategy};
pub use pool::{EndpointSelection, ProxyPool};
pub use provider::ProxyProvider;
pub use rate_limit::RateLimitManager;
pub use health::{HealthStatus, HealthChecker};
