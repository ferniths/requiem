//! Retry policy with exponential backoff and jitter.
//!
//! Transient errors (connection failures, 429/5xx, timeouts) are retried
//! up to `max_retries` times with exponential backoff. Non-transient
//! errors (4xx bad request, invalid model, context-length overflow) are
//! surfaced immediately without retrying.
//!
//! Jitter is produced with a thread-seeded LCG so concurrent callers
//! receive independent delays, preventing synchronized retry bursts.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use requiem_core::error::RequiemError;

/// A retry configuration.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (excluding the first attempt).
    pub max_retries: u32,
    /// Base delay in milliseconds before the first retry.
    pub base_delay_ms: u64,
    /// Multiplier applied to the delay on each subsequent retry.
    pub backoff_factor: f64,
    /// Upper bound on a single delay in milliseconds.
    pub max_delay_ms: u64,
    /// Whether to add random jitter (prevents thundering herds).
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 500,
            backoff_factor: 2.0,
            max_delay_ms: 30_000,
            jitter: true,
        }
    }
}

/// Generate a jitter factor in [0.5, 1.5] using a thread-and-time-seeded
/// LCG so that concurrent callers at the same attempt number receive
/// different delays.
fn jitter_factor() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let tid = std::thread::current().id();
    // Mix in a hash of the thread ID for per-thread independence.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    tid.hash(&mut h);
    let seed = nanos ^ h.finish();
    // 64-bit LCG (xorshift-lite): mix the seed, take a fraction.
    let x = seed
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add(seed >> 32);
    let frac = ((x & 0xFFFF) as f64 / 0xFFFF as f64) * 1.0 + 0.5;
    frac.min(1.5)
}

impl RetryPolicy {
    /// Compute the delay before attempt `n` (0-indexed: n=0 is the first
    /// retry, n=1 the second, etc.).
    pub fn delay_for_attempt(&self, n: u32) -> Duration {
        let exp = self.backoff_factor.powi(n as i32);
        let base = self.base_delay_ms as f64 * exp;
        let cap = self.max_delay_ms as f64;
        let base = base.min(cap);
        let jitter = if self.jitter {
            base * jitter_factor()
        } else {
            base
        };
        Duration::from_millis(jitter as u64)
    }

    /// Whether attempt `n` is within the retry budget.
    pub fn should_retry(&self, n: u32) -> bool {
        n < self.max_retries
    }
}

/// Build a `RetryPolicy` with a custom max-retries value.
pub fn from_max_retries(max_retries: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        ..Default::default()
    }
}

/// Compute the backoff delay for a specific retryable error, honoring the
/// server-reported `retry_after_ms` for rate-limit responses.
///
/// For `RateLimited` errors the server's `retry_after_ms` is used as the
/// base delay, clamped to `[base_delay_ms, max_delay_ms]`, with jitter
/// applied to prevent thundering herds. For all other transient errors
/// the standard exponential backoff schedule applies.
pub fn delay_for_error(policy: &RetryPolicy, attempt: u32, err: &RequiemError) -> Duration {
    match err {
        RequiemError::RateLimited { retry_after_ms } => {
            let requested = *retry_after_ms;
            let base = policy.base_delay_ms;
            let cap = policy.max_delay_ms;
            let clamped = requested.clamp(base, cap);
            if policy.jitter {
                Duration::from_millis((clamped as f64 * jitter_factor()) as u64)
            } else {
                Duration::from_millis(clamped)
            }
        }
        _ => policy.delay_for_attempt(attempt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_jitter_policy() -> RetryPolicy {
        RetryPolicy {
            jitter: false,
            ..Default::default()
        }
    }

    #[test]
    fn rate_limited_honors_server_backoff() {
        let policy = no_jitter_policy();
        let err = RequiemError::RateLimited {
            retry_after_ms: 5_000,
        };
        let delay = delay_for_error(&policy, 0, &err);
        // 5000ms is within [500, 30000], so it passes through unchanged.
        assert_eq!(delay.as_millis(), 5_000);
    }

    #[test]
    fn rate_limited_clamped_to_max() {
        let policy = no_jitter_policy();
        let err = RequiemError::RateLimited {
            retry_after_ms: 99_999,
        };
        let delay = delay_for_error(&policy, 0, &err);
        assert_eq!(delay.as_millis(), 30_000);
    }

    #[test]
    fn rate_limited_clamped_to_min() {
        let policy = no_jitter_policy();
        let err = RequiemError::RateLimited { retry_after_ms: 10 };
        let delay = delay_for_error(&policy, 0, &err);
        assert_eq!(delay.as_millis(), 500);
    }

    #[test]
    fn rate_limited_with_jitter_is_in_range() {
        let policy = RetryPolicy::default(); // jitter: true
        let err = RequiemError::RateLimited {
            retry_after_ms: 5_000,
        };
        let delay = delay_for_error(&policy, 0, &err);
        // With jitter in [0.5, 1.5], 5000ms becomes [2500, 7500].
        assert!(
            delay.as_millis() >= 2_500 && delay.as_millis() <= 7_500,
            "jittered delay was {}ms",
            delay.as_millis()
        );
    }

    #[test]
    fn connection_error_uses_backoff_schedule() {
        let policy = no_jitter_policy();
        let err = RequiemError::Connection("refused".into());
        let delay0 = delay_for_error(&policy, 0, &err);
        let delay1 = delay_for_error(&policy, 1, &err);
        // Without jitter: base 500ms, 500ms, 1000ms...
        assert_eq!(delay0.as_millis(), 500);
        assert_eq!(delay1.as_millis(), 1000);
    }

    #[test]
    fn jitter_produces_independent_delays() {
        // Two consecutive jittered calls for the same base should differ
        // (the seed includes nanosecond time + thread ID, so collisions
        // are vanishingly unlikely in a test).
        let policy = RetryPolicy::default();
        let err = RequiemError::RateLimited {
            retry_after_ms: 5_000,
        };
        let d1 = delay_for_error(&policy, 0, &err);
        let d2 = delay_for_error(&policy, 0, &err);
        // Both must be in [2500, 7500].
        assert!(d1.as_millis() >= 2_500 && d1.as_millis() <= 7_500);
        assert!(d2.as_millis() >= 2_500 && d2.as_millis() <= 7_500);
        // In practice they will differ; we can't assert that with 100%
        // certainty, but the probability of a collision is ~1/65536.
    }
}
