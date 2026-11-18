//! Global rate-limit coordination for the model client.
//!
//! One instance is shared by all agents. When any agent receives a
//! 429 response, the manager records the backoff window and blocks
//! all subsequent requests until the window expires — preventing
//! the "every agent retries independently" thundering-herd problem.

use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// A global rate-limit coordinator.
///
/// Shared via `Arc` between all agents. When a 429 is received, the
/// manager enters a cooldown state; all `acquire()` calls block until
/// the cooldown expires. The cooldown is the maximum of the server's
/// `retry_after_ms` and the internal backoff schedule.
pub struct RateLimitManager {
    /// The instant when the current cooldown expires.
    cooldown_until: tokio::sync::Mutex<Option<Instant>>,
    /// Notifier for waking blocked waiters.
    notify: Notify,
    /// The last 429's server-reported retry window (ms), if any.
    last_retry_after_ms: tokio::sync::Mutex<Option<u64>>,
    /// Consecutive 429 count (resets on a successful request).
    consecutive_429s: std::sync::atomic::AtomicU32,
    /// Whether any endpoint is currently in a rate-limited state.
    limited: std::sync::atomic::AtomicBool,
}

impl Default for RateLimitManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitManager {
    pub fn new() -> Self {
        Self {
            cooldown_until: tokio::sync::Mutex::new(None),
            notify: Notify::new(),
            last_retry_after_ms: tokio::sync::Mutex::new(None),
            consecutive_429s: std::sync::atomic::AtomicU32::new(0),
            limited: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Record that a 429 was received with the given server-reported
    /// retry window. All blocked `acquire()` calls will wait until
    /// `retry_after_ms` elapses.
    pub async fn record_rate_limit(&self, retry_after_ms: u64) {
        let count = self.consecutive_429s.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        *self.last_retry_after_ms.lock().await = Some(retry_after_ms);
        self.limited.store(true, std::sync::atomic::Ordering::SeqCst);

        // Scale the cooldown with consecutive 429s: 1st = server's window,
        // 2nd = 2× server's window, 3rd = 3×, etc. (capped at 120 s).
        let scaled = (retry_after_ms as u64 * count as u64).min(120_000);
        let until = Instant::now() + Duration::from_millis(scaled);
        *self.cooldown_until.lock().await = Some(until);

        tracing::warn!(
            "rate limit: 429 #{count}, retry-after {retry_after_ms}ms, cooldown {scaled}ms"
        );
    }

    /// Record a successful request: clears the consecutive-429 counter
    /// and the limited flag.
    pub fn record_success(&self) {
        self.consecutive_429s.store(0, std::sync::atomic::Ordering::SeqCst);
        self.limited.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Block until the current cooldown (if any) expires.
    ///
    /// Returns the actual wait duration (zero if no cooldown was active).
    pub async fn acquire(&self) -> Duration {
        loop {
            let until = *self.cooldown_until.lock().await;
            let now = Instant::now();
            match until {
                Some(target) if target > now => {
                    let wait = target - now;
                    // Set up a notification: when `target` is reached, we
                    // need to re-check. Use a tokio sleep + re-loop.
                    tokio::time::sleep(wait).await;
                    // Clear the cooldown so other waiters can proceed.
                    *self.cooldown_until.lock().await = None;
                    self.notify.notify_waiters();
                    continue;
                }
                _ => {
                    // No active cooldown.
                    let waited = 0;
                    return Duration::from_millis(waited);
                }
            }
        }
    }

    /// Whether the manager is currently in a rate-limited state.
    pub fn is_limited(&self) -> bool {
        self.limited.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The number of consecutive 429s received (before the last success).
    pub fn consecutive_429s(&self) -> u32 {
        self.consecutive_429s.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The last server-reported retry window, if any.
    pub async fn last_retry_after_ms(&self) -> Option<u64> {
        *self.last_retry_after_ms.lock().await
    }
}

// ── Per-endpoint rate-limit state ────────────────────────────────────

/// Tracks rate-limit state for a single proxy endpoint.
///
/// Each endpoint in the pool has its own `EndpointRateState` so that a
/// 429 on one endpoint does not block requests routed through a
/// different endpoint.
#[derive(Debug, Default)]
pub struct EndpointRateState {
    /// The instant when this endpoint's cooldown expires.
    pub cooldown_until: Option<Instant>,
    /// Consecutive 429s for this endpoint.
    pub consecutive_429s: u32,
}

impl EndpointRateState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this endpoint is currently in cooldown.
    pub fn is_in_cooldown(&self) -> bool {
        self.cooldown_until
            .is_some_and(|t| t > Instant::now())
    }

    /// Record a 429 for this endpoint.
    pub fn record_429(&mut self, retry_after_ms: u64) {
        self.consecutive_429s += 1;
        let scaled =
            (retry_after_ms * self.consecutive_429s as u64).min(120_000);
        self.cooldown_until = Some(Instant::now() + Duration::from_millis(scaled));
    }

    /// Record success: clear the cooldown.
    pub fn record_success(&mut self) {
        self.consecutive_429s = 0;
        self.cooldown_until = None;
    }
}

/// A map of endpoint-id → rate-limit state, shared across agents.
#[derive(Debug, Default)]
pub struct EndpointRateMap {
    states: std::collections::HashMap<uuid::Uuid, EndpointRateState>,
}

impl EndpointRateMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a 429 for the given endpoint.
    pub fn record_429(&mut self, id: &uuid::Uuid, retry_after_ms: u64) {
        self.states
            .entry(*id)
            .or_insert_with(EndpointRateState::new)
            .record_429(retry_after_ms);
    }

    /// Record success for the given endpoint.
    pub fn record_success(&mut self, id: &uuid::Uuid) {
        if let Some(s) = self.states.get_mut(id) {
            s.record_success();
        }
    }

    /// Whether the given endpoint is currently in cooldown.
    pub fn is_cooling_down(&self, id: &uuid::Uuid) -> bool {
        self.states
            .get(id)
            .is_some_and(|s| s.is_in_cooldown())
    }

    /// The number of endpoints currently in cooldown.
    pub fn cooling_down_count(&self) -> usize {
        self.states
            .values()
            .filter(|s| s.is_in_cooldown())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_rate_state_cooldown() {
        let mut state = EndpointRateState::new();
        assert!(!state.is_in_cooldown());
        state.record_429(1_000);
        assert!(state.is_in_cooldown());
        state.record_success();
        assert!(!state.is_in_cooldown());
    }

    #[test]
    fn endpoint_rate_map_tracks_per_endpoint() {
        let mut map = EndpointRateMap::new();
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();

        map.record_429(&id1, 5_000);
        assert!(map.is_cooling_down(&id1));
        assert!(!map.is_cooling_down(&id2));

        map.record_success(&id1);
        assert!(!map.is_cooling_down(&id1));
        assert_eq!(map.cooling_down_count(), 0);
    }

    #[tokio::test]
    async fn manager_records_and_clears_429s() {
        let mgr = RateLimitManager::new();
        assert_eq!(mgr.consecutive_429s(), 0);
        assert!(!mgr.is_limited());

        mgr.record_rate_limit(1_000).await;
        assert_eq!(mgr.consecutive_429s(), 1);
        assert!(mgr.is_limited());

        mgr.record_success();
        assert_eq!(mgr.consecutive_429s(), 0);
        assert!(!mgr.is_limited());
    }

    #[tokio::test]
    async fn manager_acquire_returns_zero_when_no_cooldown() {
        let mgr = RateLimitManager::new();
        let waited = mgr.acquire().await;
        assert_eq!(waited, Duration::ZERO);
    }
}
