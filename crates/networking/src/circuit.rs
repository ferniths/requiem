//! Shared provider circuit breaker.
//!
//! When a provider or model route keeps failing with *permanent* errors
//! (an upstream 404, a missing model, an auth rejection) the retry loop in
//! [`ModelClient`] would otherwise keep re-sending the same doomed request
//! on behalf of every agent, flooding the gateway with warnings. The
//! circuit breaker records a bounded number of consecutive hard failures
//! per provider and, once tripped, *stops admitting new requests* until a
//! cooldown elapses, at which point it allows a single probe to test
//! recovery. Successful probes close the circuit again; failed probes
//! re-open it.
//!
//! One transient hiccup never trips the breaker: only consecutive
//! permanent failures count, and any success in between resets the
//! counter. This means the breaker never permanently disables a provider
//! on account of a single blip.
//!
//! The breaker is provider-scoped (keyed by a stable provider id) and is
//! shared across all agents through a single `Arc<ModelClient>`, so a
//! failure discovered by one agent immediately shields the others.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Whether the circuit is currently admitting requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Requests flow normally.
    Closed,
    /// The breaker is open; requests are refused until a cooldown elapses.
    Open,
    /// Cooldown elapsed; one probe is allowed through to test recovery.
    HalfOpen,
}

impl BreakerState {
    /// A short, UI-friendly label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half-open",
        }
    }
}

/// Configuration for a [`CircuitBreaker`].
#[derive(Debug, Clone)]
pub struct BreakerConfig {
    /// Number of consecutive permanent failures before the circuit opens.
    pub failure_threshold: u32,
    /// How long to stay open before allowing a recovery probe.
    pub cooldown: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            cooldown: Duration::from_secs(30),
        }
    }
}

/// A result from asking the breaker whether a request may proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The request may proceed. The caller must report the outcome via
    /// [`CircuitBreaker::record_success`] or [`CircuitBreaker::record_failure`].
    Allow,
    /// The breaker is open and the cooldown has not elapsed; the request is
    /// refused without being sent.
    Refused { wait_ms: u64 },
}

/// A per-provider circuit breaker, safe to share across tasks via `Arc`.
pub struct CircuitBreaker {
    config: BreakerConfig,
    inner: Mutex<Inner>,
}

struct Inner {
    state: BreakerState,
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    last_failure_at: Option<Instant>,
}

/// Acquire the breaker's lock, recovering from poisoning.
///
/// Every operation on the breaker holds the lock only across infallible,
/// non-fallible mutations, so a poisoned lock (a prior panic) is safe to
/// recover from: the state is always consistent on release.
fn lock(inner: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(|p| p.into_inner())
}

impl CircuitBreaker {
    /// Create a breaker with the default configuration.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::with_config(BreakerConfig::default()))
    }

    /// Create a breaker with an explicit configuration.
    pub fn with_config(config: BreakerConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(Inner {
                state: BreakerState::Closed,
                consecutive_failures: 0,
                opened_at: None,
                last_failure_at: None,
            }),
        }
    }

    /// Ask whether a new request for this provider may proceed.
    ///
    /// A call from the `HalfOpen` state (cooldown elapsed) transitions the
    /// circuit to `HalfOpen` and allows exactly one probe. While `Open` and
    /// the cooldown has not elapsed, the request is refused and the caller
    /// is told how long to wait.
    pub fn allow_request(&self) -> Admission {
        let mut g = lock(&self.inner);
        if g.state == BreakerState::Closed {
            return Admission::Allow;
        }
        // Open: check the cooldown.
        if let Some(opened_at) = g.opened_at {
            let elapsed = opened_at.elapsed();
            if elapsed < self.config.cooldown {
                let wait_ms = (self.config.cooldown - elapsed).as_millis() as u64;
                return Admission::Refused { wait_ms };
            }
            // Cooldown elapsed: allow a single recovery probe.
            g.state = BreakerState::HalfOpen;
            return Admission::Allow;
        }
        Admission::Allow
    }

    /// Record a successful request. Closes the circuit and resets the
    /// failure counter.
    pub fn record_success(&self) {
        let mut g = lock(&self.inner);
        g.consecutive_failures = 0;
        g.state = BreakerState::Closed;
        g.opened_at = None;
        g.last_failure_at = None;
    }

    /// Record a failed request.
    ///
    /// `permanent` marks a hard failure (upstream 404, missing model, auth
    /// rejection, refused-by-breaker). Only *consecutive permanent*
    /// failures count toward the threshold; a transient failure does not
    /// advance the trip counter (it is the retry loop's job to handle
    /// those).
    pub fn record_failure(&self, permanent: bool) {
        let mut g = lock(&self.inner);
        g.last_failure_at = Some(Instant::now());
        if !permanent {
            // Transient failures are retried by the client; they should not
            // trip the breaker. Reset the consecutive-permanent counter so a
            // single blip does not accumulate.
            g.consecutive_failures = 0;
            return;
        }
        g.consecutive_failures += 1;
        if g.consecutive_failures >= self.config.failure_threshold && g.state != BreakerState::Open
        {
            g.state = BreakerState::Open;
            g.opened_at = Some(Instant::now());
        }
    }

    /// The current circuit state.
    pub fn state(&self) -> BreakerState {
        let g = lock(&self.inner);
        // If the breaker is open but the cooldown has elapsed, report the
        // effective (half-open) state so callers see the real readiness.
        if g.state == BreakerState::Open {
            if let Some(opened_at) = g.opened_at {
                if opened_at.elapsed() >= self.config.cooldown {
                    return BreakerState::HalfOpen;
                }
            }
        }
        g.state
    }

    /// The most recent failure time, if any.
    pub fn last_failure(&self) -> Option<Instant> {
        lock(&self.inner).last_failure_at
    }

    /// Consecutive permanent failures recorded since the last success.
    pub fn consecutive_failures(&self) -> u32 {
        lock(&self.inner).consecutive_failures
    }

    /// How long the breaker will stay open from now (0 when closed or
    /// half-open).
    pub fn remaining_open_ms(&self) -> u64 {
        let g = lock(&self.inner);
        if g.state != BreakerState::Open {
            return 0;
        }
        match g.opened_at {
            Some(opened_at) => {
                (self.config.cooldown - opened_at.elapsed()).as_millis() as u64
            }
            None => self.config.cooldown.as_millis() as u64,
        }
    }

    /// A short, single-line description suitable for a UI notice.
    pub fn describe(&self) -> String {
        match self.state() {
            BreakerState::Closed => "provider circuit closed (healthy)".to_string(),
            BreakerState::HalfOpen => "provider circuit half-open (probing recovery)".to_string(),
            BreakerState::Open => format!(
                "provider circuit open · refusing new requests · retry in {}s",
                self.remaining_open_ms() / 1000
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn closed_allows_requests() {
        let cb = CircuitBreaker::new();
        assert_eq!(cb.allow_request(), Admission::Allow);
        assert_eq!(cb.state(), BreakerState::Closed);
    }

    #[test]
    fn transient_failures_do_not_trip() {
        let cb = CircuitBreaker::new();
        for _ in 0..10 {
            cb.record_failure(false);
        }
        assert_eq!(
            cb.state(),
            BreakerState::Closed,
            "transient blips must not open"
        );
        assert_eq!(cb.consecutive_failures(), 0);
    }

    #[test]
    fn success_resets_counter() {
        let cb = CircuitBreaker::new();
        cb.record_failure(true);
        cb.record_failure(true);
        cb.record_success();
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.state(), BreakerState::Closed);
    }

    #[test]
    fn permanent_failures_trip_breaker() {
        let cb = CircuitBreaker::with_config(BreakerConfig {
            failure_threshold: 3,
            cooldown: Duration::from_millis(50),
        });
        for _ in 0..3 {
            cb.record_failure(true);
        }
        assert_eq!(cb.state(), BreakerState::Open);
        // While open and within the cooldown, requests are refused.
        let admission = cb.allow_request();
        assert!(matches!(admission, Admission::Refused { wait_ms } if wait_ms > 0));
    }

    #[test]
    fn half_open_probe_after_cooldown() {
        let cb = CircuitBreaker::with_config(BreakerConfig {
            failure_threshold: 1,
            cooldown: Duration::from_millis(30),
        });
        cb.record_failure(true);
        assert_eq!(cb.state(), BreakerState::Open);
        thread::sleep(Duration::from_millis(40));
        // After the cooldown the state becomes half-open and one probe is
        // allowed through.
        assert_eq!(cb.state(), BreakerState::HalfOpen);
        assert_eq!(cb.allow_request(), Admission::Allow);
        // A successful probe closes the circuit.
        cb.record_success();
        assert_eq!(cb.state(), BreakerState::Closed);
    }

    #[test]
    fn failed_probe_reopens() {
        let cb = CircuitBreaker::with_config(BreakerConfig {
            failure_threshold: 1,
            cooldown: Duration::from_millis(30),
        });
        cb.record_failure(true);
        thread::sleep(Duration::from_millis(40));
        let _ = cb.allow_request(); // half-open probe allowed
        cb.record_failure(true);
        assert_eq!(cb.state(), BreakerState::Open, "failed probe re-opens");
    }

    #[test]
    fn shared_via_arc_is_thread_safe() {
        let cb = Arc::new(CircuitBreaker::with_config(BreakerConfig {
            failure_threshold: 4,
            cooldown: Duration::from_secs(1),
        }));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let cb = Arc::clone(&cb);
            handles.push(thread::spawn(move || {
                for _ in 0..20 {
                    let _ = cb.allow_request();
                    cb.record_failure(true);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(cb.state(), BreakerState::Open);
    }

    #[test]
    fn describe_reflects_state() {
        let cb = CircuitBreaker::with_config(BreakerConfig {
            failure_threshold: 1,
            cooldown: Duration::from_secs(60),
        });
        assert!(cb.describe().contains("closed"));
        cb.record_failure(true);
        assert!(cb.describe().contains("open"));
    }
}
