//! Provider abstraction for proxy services.
//!
//! Each provider knows how to discover its endpoints, authenticate, and
//! report health. The pool stores provider metadata; the concrete
//! discovery/validation logic lives here behind a trait object so new
//! providers can be added without changing the pool.

use std::fmt;

use crate::config::{ProviderKind, ProxyEndpoint, ProxyProtocol};
use serde::{Deserialize, Serialize};

/// A proxy service provider (free-tier, paid, or custom).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyProvider {
    /// Human-readable name shown in the UI.
    pub name: String,
    /// The provider class.
    pub kind: ProviderKind,
    /// Whether this provider supports automatic endpoint discovery.
    pub auto_discover: bool,
    /// Whether this provider requires API credentials.
    pub requires_auth: bool,
    /// Documentation URL for the provider's API.
    pub docs_url: Option<String>,
    /// Known usage restrictions (free-tier limits, etc.).
    pub restrictions: Vec<String>,
}

impl ProxyProvider {
    /// Create a provider descriptor.
    pub fn new(name: &str, kind: ProviderKind, auto_discover: bool, requires_auth: bool) -> Self {
        Self {
            name: name.to_string(),
            kind,
            auto_discover,
            requires_auth,
            docs_url: None,
            restrictions: Vec::new(),
        }
    }

    /// Set the documentation URL.
    pub fn with_docs(mut self, url: &str) -> Self {
        self.docs_url = Some(url.to_string());
        self
    }

    /// Add a usage restriction note.
    pub fn with_restriction(mut self, note: &str) -> Self {
        self.restrictions.push(note.to_string());
        self
    }

    /// Build a `ProxyEndpoint` from this provider's base configuration.
    pub fn make_endpoint(&self, url: &str, protocol: ProxyProtocol) -> ProxyEndpoint {
        ProxyEndpoint::new(&self.name, self.kind, url, protocol)
    }
}

impl fmt::Display for ProxyProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.name, self.kind)
    }
}
//
// These are *descriptors* only — they document known providers so the
// setup wizard can present them to the user. The actual endpoint URLs
// and credentials are user-supplied. No automatic discovery is
// performed against untrusted third-party APIs.

/// A custom (user-entered) proxy. No provider-specific metadata.
pub fn custom() -> ProxyProvider {
    ProxyProvider::new("Custom", ProviderKind::None, false, false)
}

/// A generic free-tier HTTP proxy service.
pub fn free_tier() -> ProxyProvider {
    ProxyProvider::new("Free-tier", ProviderKind::Free, false, false)
        .with_restriction("Free proxies may have lower reliability and bandwidth caps")
        .with_restriction("API traffic is routed through a third-party service")
}

/// A paid proxy service (e.g. a dedicated residential/rotating proxy API).
pub fn paid() -> ProxyProvider {
    ProxyProvider::new("Paid", ProviderKind::Paid, false, true)
        .with_restriction("Requires API credentials")
        .with_restriction("Billed by the provider")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_provider_is_none_kind() {
        let p = custom();
        assert_eq!(p.kind, ProviderKind::None);
        assert!(!p.auto_discover);
        assert!(!p.requires_auth);
    }

    #[test]
    fn free_tier_has_restrictions() {
        let p = free_tier();
        assert!(!p.restrictions.is_empty());
    }

    #[test]
    fn paid_requires_auth() {
        let p = paid();
        assert!(p.requires_auth);
    }

    #[test]
    fn make_endpoint_inherits_provider_name() {
        let p = custom();
        let ep = p.make_endpoint("http://1.2.3.4:8080", ProxyProtocol::Http);
        assert_eq!(ep.provider, "Custom");
        assert_eq!(ep.kind, ProviderKind::None);
    }
}
