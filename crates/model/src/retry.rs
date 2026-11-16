//! Retry policy with exponential backoff and jitter.
//!
//! Transient errors (connection failures, 429/5xx, timeouts) are retried
//! up to `max_retries` times with exponential backoff. Non-transient
//! errors (4xx bad request, invalid model, context-length overflow) are
//! surfaced immediately without retrying.
//!
//! Jitter is produced with a small in-crate LCG so the crate has no
//! external RNG dependency.

use std::time::Duration;

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

impl RetryPolicy {
    /// Compute the delay before attempt `n` (0-indexed: n=0 is the first
    /// retry, n=1 the second, etc.).
    pub fn delay_for_attempt(&self, n: u32) -> Duration {
        let exp = self.backoff_factor.powi(n as i32);
        let base = self.base_delay_ms as f64 * exp;
        let cap = self.max_delay_ms as f64;
        let base = base.min(cap);
        let jitter = if self.jitter {
            // LCG: a cheap, deterministic-enough pseudo-random in [0.5, 1.5].
            let seed = (n as u64).wrapping_mul(0x9E3779B97F4A7C15);
            let frac = ((seed & 0xFFFF) as f64 / 0xFFFF as f64) * 1.0 + 0.5;
            base * frac.min(1.5)
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
