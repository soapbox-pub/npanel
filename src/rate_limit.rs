//! Token buckets, used to meter certificate authorization.

use std::time::Instant;

use indexmap::IndexMap;

/// How many tokens a key gets, and how long a spent bucket takes to refill.
#[derive(Clone, Copy, Debug)]
pub struct RateLimit {
    /// Tokens a key starts with, and the most it can bank.
    pub capacity: f64,
    /// Milliseconds in which an empty bucket refills completely.
    pub interval_ms: f64,
}

struct Bucket {
    tokens: f64,
    /// When `tokens` was last recomputed, in ms.
    updated: f64,
}

/// A token bucket per key, with a bounded number of keys.
///
/// The limit is passed to [`RateLimiter::take`] rather than held here so that
/// a setting an admin changes on the dashboard applies to the next request.
///
/// Keys can come from outside — a hostname someone else chose — so the map is
/// capped. Buckets that have refilled to capacity are indistinguishable from
/// ones that never existed and are dropped first; past that, the oldest go.
pub struct RateLimiter {
    buckets: IndexMap<String, Bucket>,
    max_keys: usize,
    now: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl RateLimiter {
    pub fn new(max_keys: usize) -> Self {
        let epoch = Instant::now();
        Self { buckets: IndexMap::new(), max_keys, now: Box::new(move || epoch.elapsed().as_secs_f64() * 1000.0) }
    }

    #[cfg(test)]
    pub fn with_clock(max_keys: usize, now: impl Fn() -> f64 + Send + Sync + 'static) -> Self {
        Self { buckets: IndexMap::new(), max_keys, now: Box::new(now) }
    }

    /// Spend one token for `key`, or return false when the bucket is empty.
    pub fn take(&mut self, key: &str, limit: RateLimit) -> bool {
        let now = (self.now)();

        let Some(bucket) = self.buckets.get_mut(key) else {
            self.evict(limit, now);
            self.buckets.insert(key.to_string(), Bucket { tokens: limit.capacity - 1.0, updated: now });
            return true;
        };

        let tokens = refill(bucket, limit, now);
        bucket.updated = now;
        if tokens < 1.0 {
            // Store the partial token so a denied call still banks what it accrued.
            bucket.tokens = tokens;
            return false;
        }

        bucket.tokens = tokens - 1.0;
        true
    }

    /// Number of keys currently tracked.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    fn evict(&mut self, limit: RateLimit, now: f64) {
        if self.buckets.len() < self.max_keys {
            return;
        }

        self.buckets.retain(|_, bucket| refill(bucket, limit, now) < limit.capacity);

        // Insertion-ordered, so this drops the keys first seen.
        while self.buckets.len() >= self.max_keys {
            self.buckets.shift_remove_index(0);
        }
    }
}

/// A bucket's token count brought up to date, capped at capacity.
fn refill(bucket: &Bucket, limit: RateLimit, now: f64) -> f64 {
    let elapsed = (now - bucket.updated).max(0.0);
    (bucket.tokens + (elapsed / limit.interval_ms) * limit.capacity).min(limit.capacity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    #[test]
    fn spends_and_refills() {
        let clock = Arc::new(AtomicU64::new(0));
        let c = clock.clone();
        let mut limiter = RateLimiter::with_clock(10, move || c.load(Ordering::SeqCst) as f64);
        let limit = RateLimit { capacity: 2.0, interval_ms: 1000.0 };

        assert!(limiter.take("a", limit));
        assert!(limiter.take("a", limit));
        assert!(!limiter.take("a", limit));
        assert!(limiter.take("b", limit));

        clock.store(500, Ordering::SeqCst);
        assert!(limiter.take("a", limit));
        assert!(!limiter.take("a", limit));
    }

    #[test]
    fn caps_keys() {
        let mut limiter = RateLimiter::with_clock(2, || 0.0);
        let limit = RateLimit { capacity: 5.0, interval_ms: 1000.0 };
        limiter.take("a", limit);
        limiter.take("b", limit);
        limiter.take("c", limit);
        assert_eq!(limiter.len(), 2);
    }
}
