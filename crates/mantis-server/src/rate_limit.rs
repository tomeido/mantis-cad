//! Small in-memory token buckets for protecting public write endpoints.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl Bucket {
    fn full(capacity: u32, now: Instant) -> Self {
        Self {
            tokens: f64::from(capacity),
            updated: now,
        }
    }

    fn take(&mut self, capacity: u32, per_minute: u32, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
        let refill_per_second = f64::from(per_minute) / 60.0;
        self.tokens = (self.tokens + elapsed * refill_per_second).min(f64::from(capacity));
        self.updated = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// A process-global bucket plus a bucket for each signing public key.
///
/// This is deliberately an application safeguard, not a DDoS boundary. The
/// platform edge remains responsible for connection-level abuse.
#[derive(Debug)]
pub struct WriteRateLimiter {
    started: Instant,
    global: Bucket,
    keys: BTreeMap<String, Bucket>,
    global_per_minute: u32,
    global_burst: u32,
    key_per_minute: u32,
    key_burst: u32,
}

impl WriteRateLimiter {
    pub fn new(
        global_per_minute: u32,
        global_burst: u32,
        key_per_minute: u32,
        key_burst: u32,
    ) -> Self {
        let now = Instant::now();
        Self {
            started: now,
            global: Bucket::full(global_burst, now),
            keys: BTreeMap::new(),
            global_per_minute,
            global_burst,
            key_per_minute,
            key_burst,
        }
    }

    pub fn allow(&mut self, public_key: &str) -> bool {
        self.allow_at(public_key, Instant::now())
    }

    fn allow_at(&mut self, public_key: &str, now: Instant) -> bool {
        // Evaluate the key bucket on a clone so a global rejection does not
        // consume the caller's per-key token.
        let mut key = self
            .keys
            .get(public_key)
            .cloned()
            .unwrap_or_else(|| Bucket::full(self.key_burst, now));
        if !key.take(self.key_burst, self.key_per_minute, now) {
            self.keys.insert(public_key.to_string(), key);
            return false;
        }
        if !self
            .global
            .take(self.global_burst, self.global_per_minute, now)
        {
            return false;
        }
        self.keys.insert(public_key.to_string(), key);
        self.prune(now);
        true
    }

    fn prune(&mut self, now: Instant) {
        if now.saturating_duration_since(self.started) < Duration::from_secs(300) {
            return;
        }
        self.started = now;
        self.keys.retain(|_, bucket| {
            now.saturating_duration_since(bucket.updated) < Duration::from_secs(600)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_key_and_global_bursts_then_refills() {
        let start = Instant::now();
        let mut limiter = WriteRateLimiter::new(4, 2, 2, 1);
        assert!(limiter.allow_at("a", start));
        assert!(!limiter.allow_at("a", start));
        assert!(limiter.allow_at("b", start));
        assert!(!limiter.allow_at("c", start));

        let later = start + Duration::from_secs(30);
        assert!(limiter.allow_at("a", later));
    }
}
