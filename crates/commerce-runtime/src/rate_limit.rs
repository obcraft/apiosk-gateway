//! Per-buyer fixed-window rate limiter.
//!
//! Pilot scope: keep one buyer (or one runaway agent) from drowning out the
//! gateway for everyone else. The limiter is keyed on
//! `(connect_token_id | client_ip, api_slug)` so each connected buyer gets
//! its own bucket per upstream API. Counts live in-memory via `moka` with a
//! TTL equal to the window length, which gives us natural reset semantics
//! without a background sweeper.
//!
//! This is deliberately a fixed-window counter, not a true sliding window:
//! one Postgres-free atomic increment per request and zero new dependencies
//! beyond what the gateway already pulls in. If the gateway moves to
//! multi-instance deployments, swap the in-memory cache for a Redis or
//! Postgres-advisory-lock backend behind the same trait.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;

/// Per-key counter cell. Wrapped in `Arc` so the cache hands out cheap clones.
type Counter = Arc<AtomicU32>;

/// Configuration for a single rate-limit policy.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitPolicy {
    pub max_requests: u32,
    pub window: Duration,
}

impl RateLimitPolicy {
    pub const fn per_minute(max_requests: u32) -> Self {
        Self {
            max_requests,
            window: Duration::from_secs(60),
        }
    }
}

/// Default policy if no env override is set. 60 requests / minute / buyer /
/// API is generous for the pilot but blocks an obvious runaway loop.
pub const DEFAULT_PER_TOKEN_POLICY: RateLimitPolicy = RateLimitPolicy::per_minute(60);

/// Default policy applied to unauthenticated traffic, keyed on client IP.
/// Smaller because public endpoints shouldn't carry production load anyway.
pub const DEFAULT_PER_IP_POLICY: RateLimitPolicy = RateLimitPolicy::per_minute(30);

#[derive(Clone)]
pub struct RateLimiter {
    counters: Cache<String, Counter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitDecision {
    Allowed { remaining: u32 },
    Throttled { retry_after: Duration },
}

impl RateLimiter {
    /// Create a new in-memory limiter. `capacity` caps the number of distinct
    /// keys we track at once; once exceeded, oldest entries are evicted.
    pub fn new(capacity: u64, max_window: Duration) -> Self {
        let counters = Cache::builder()
            .max_capacity(capacity)
            .time_to_live(max_window)
            .build();
        Self { counters }
    }

    /// Check (and atomically increment) the bucket for `key` against `policy`.
    /// First call in a new window seeds the cache entry with TTL = policy.window,
    /// so the counter resets automatically once the window expires.
    pub async fn check(&self, key: &str, policy: RateLimitPolicy) -> RateLimitDecision {
        let counter = self
            .counters
            .get_with(key.to_string(), async { Arc::new(AtomicU32::new(0)) })
            .await;

        // `fetch_add` returns the previous value, so the post-increment count
        // is `previous + 1`. We compare that against the limit.
        let post = counter.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        if post > policy.max_requests {
            // Once over the limit, leave the counter pinned so a flood of
            // retries cannot reset the window by churning entries.
            return RateLimitDecision::Throttled {
                retry_after: policy.window,
            };
        }
        RateLimitDecision::Allowed {
            remaining: policy.max_requests.saturating_sub(post),
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        // 100k distinct keys * 60s window = ~10 MB at peak, well within budget.
        Self::new(100_000, Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allows_up_to_limit_then_throttles() {
        let limiter = RateLimiter::new(1_000, Duration::from_secs(60));
        let policy = RateLimitPolicy::per_minute(3);

        for i in 0..3 {
            match limiter.check("token-a:weather", policy).await {
                RateLimitDecision::Allowed { remaining } => {
                    assert_eq!(remaining, 3 - i - 1);
                }
                other => panic!("expected Allowed at iteration {i}, got {other:?}"),
            }
        }

        match limiter.check("token-a:weather", policy).await {
            RateLimitDecision::Throttled { retry_after } => {
                assert_eq!(retry_after, Duration::from_secs(60));
            }
            other => panic!("expected Throttled, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn buckets_are_independent() {
        let limiter = RateLimiter::new(1_000, Duration::from_secs(60));
        let policy = RateLimitPolicy::per_minute(1);

        assert!(matches!(
            limiter.check("token-a:weather", policy).await,
            RateLimitDecision::Allowed { .. }
        ));
        assert!(matches!(
            limiter.check("token-b:weather", policy).await,
            RateLimitDecision::Allowed { .. }
        ));
        assert!(matches!(
            limiter.check("token-a:weather", policy).await,
            RateLimitDecision::Throttled { .. }
        ));
        assert!(matches!(
            limiter.check("token-a:other", policy).await,
            RateLimitDecision::Allowed { .. }
        ));
    }
}
