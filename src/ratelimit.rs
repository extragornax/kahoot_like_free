use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_REQUESTS: u32 = 10;

/// Simple per-key fixed-window rate limiter (in-memory). Keyed by client IP.
#[derive(Clone)]
pub struct RateLimiter {
    hits: Arc<DashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            hits: Arc::new(DashMap::new()),
        }
    }

    /// Records a hit for `key`; returns true if within the limit, false if over.
    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut entry = self.hits.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) > WINDOW {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= MAX_REQUESTS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_limit_then_blocks() {
        let rl = RateLimiter::new();
        for _ in 0..MAX_REQUESTS {
            assert!(rl.check("1.2.3.4"));
        }
        assert!(!rl.check("1.2.3.4")); // over the limit
        assert!(rl.check("5.6.7.8")); // a different IP is unaffected
    }
}
