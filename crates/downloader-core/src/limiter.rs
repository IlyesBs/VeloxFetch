//! Bandwidth limiting: token buckets, combined hierarchically.
//!
//! ```text
//! global (e.g. 50 MB/s)  +  category (optional)  +  per-download (e.g. 20 MB/s)
//! ```
//! A chunk must fit under *every* bucket that applies to it, so the tightest one wins.
//! Limits can be changed while downloads are running.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::error::{DownloadError, Result};

const MIN_BURST: f64 = 32.0 * 1024.0;
/// A wait re-checks this often whether a limit changed (lifted or raised): then it stops at once.
const RECHECK: Duration = Duration::from_millis(100);

struct Bucket {
    /// Bytes per second; `None` = unlimited.
    rate: Option<u64>,
    /// May go negative: that is "debt" the caller must wait out.
    tokens: f64,
    last: Instant,
}

fn burst(rate: u64) -> f64 {
    (rate as f64 * 0.25).max(MIN_BURST)
}

/// Token bucket. Refills continuously at `rate` bytes/s with a burst of ~250 ms worth of data.
pub struct TokenBucket {
    inner: Mutex<Bucket>,
    /// Bumped by every `set_rate`, so connections waiting under an old limit notice the change.
    changes: AtomicU64,
}

impl TokenBucket {
    pub fn new(rate: Option<u64>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Bucket { rate, tokens: rate.map_or(0.0, burst), last: Instant::now() }),
            changes: AtomicU64::new(0),
        })
    }

    pub fn rate(&self) -> Option<u64> {
        self.inner.lock().unwrap().rate
    }

    /// Change the limit (`None` = unlimited). Takes effect immediately for running downloads.
    pub fn set_rate(&self, rate: Option<u64>) {
        let mut b = self.inner.lock().unwrap();
        b.tokens = match (b.rate, rate) {
            (_, None) => 0.0,
            (None, Some(r)) => burst(r),
            (Some(_), Some(r)) => b.tokens.min(burst(r)),
        };
        b.rate = rate;
        b.last = Instant::now();
        self.changes.fetch_add(1, Ordering::Relaxed);
    }

    /// Take `n` bytes and return how long the caller must wait before using them.
    fn reserve(&self, n: usize) -> Duration {
        let mut b = self.inner.lock().unwrap();
        let Some(rate) = b.rate.filter(|r| *r > 0) else { return Duration::ZERO };
        let now = Instant::now();
        let refill = now.duration_since(b.last).as_secs_f64() * rate as f64;
        b.tokens = (b.tokens + refill).min(burst(rate));
        b.last = now;
        b.tokens -= n as f64;
        if b.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-b.tokens / rate as f64)
        }
    }
}

/// The set of buckets a single chunk must pass through.
#[derive(Clone, Default)]
pub struct Throttle {
    buckets: Vec<Arc<TokenBucket>>,
}

impl Throttle {
    pub fn new(buckets: Vec<Arc<TokenBucket>>) -> Self {
        Self { buckets }
    }

    /// The tightest limit in force right now, if any.
    pub fn min_rate(&self) -> Option<u64> {
        self.buckets.iter().filter_map(|b| b.rate().filter(|r| *r > 0)).min()
    }

    /// Whether a limit, not the network, is what holds `throughput` (bytes/s) back: it runs close to the
    /// tightest limit. A limit far above what the line gives changes nothing and does not count.
    pub fn is_holding(&self, throughput: f64) -> bool {
        self.min_rate().is_some_and(|r| throughput >= r as f64 * 0.8)
    }

    /// Wait until `n` bytes may be used. Every bucket is debited, and the *longest* wait applies
    /// (waits do not add up), so the most restrictive limit governs. Cancellation aborts the wait.
    /// A limit changed while waiting (e.g. lifted) ends the wait at once: the debt was owed to the old limit,
    /// and `set_rate` has already settled the bucket for the new one.
    pub async fn take(&self, n: usize, cancel: &CancellationToken) -> Result<()> {
        let seen: Vec<u64> = self.buckets.iter().map(|b| b.changes.load(Ordering::Relaxed)).collect();
        let wait = self.buckets.iter().map(|b| b.reserve(n)).max().unwrap_or(Duration::ZERO);
        if wait.is_zero() {
            return Ok(());
        }
        let until = tokio::time::Instant::now() + wait;
        loop {
            let left = until.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                _ = tokio::time::sleep(left.min(RECHECK)) => {}
            }
            if self.buckets.iter().zip(&seen).any(|(b, s)| b.changes.load(Ordering::Relaxed) != *s) {
                return Ok(());
            }
        }
    }
}

/// Shared limits: one global bucket plus named categories (e.g. "video", "iso"), and the "smart" bucket
/// that the app's Smart throttle tightens while a game or a call is running (unlimited otherwise).
pub struct RateLimits {
    global: Arc<TokenBucket>,
    categories: Mutex<HashMap<String, Arc<TokenBucket>>>,
    smart: Arc<TokenBucket>,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self { global: TokenBucket::new(None), categories: Mutex::new(HashMap::new()), smart: TokenBucket::new(None) }
    }
}

impl RateLimits {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_global(&self, rate: Option<u64>) {
        self.global.set_rate(rate);
    }

    pub fn global(&self) -> Option<u64> {
        self.global.rate()
    }

    pub fn set_category(&self, name: &str, rate: Option<u64>) {
        let mut c = self.categories.lock().unwrap();
        match c.get(name) {
            Some(b) => b.set_rate(rate),
            None => {
                c.insert(name.to_string(), TokenBucket::new(rate));
            }
        }
    }

    /// The Smart throttle's cap (`None` = off). Separate from the user's own global limit, so turning it
    /// off restores exactly what the user had. Applies at once to running downloads.
    pub fn set_smart(&self, rate: Option<u64>) {
        self.smart.set_rate(rate);
    }

    pub fn smart(&self) -> Option<u64> {
        self.smart.rate()
    }

    pub fn category(&self, name: &str) -> Option<u64> {
        self.categories.lock().unwrap().get(name).and_then(|b| b.rate())
    }

    /// Throttle for one download: global + its category (if any) + its own bucket.
    pub fn throttle_for(&self, category: Option<&str>, per_download: Arc<TokenBucket>) -> Throttle {
        let mut v = vec![self.global.clone(), self.smart.clone()];
        if let Some(name) = category {
            let mut c = self.categories.lock().unwrap();
            v.push(c.entry(name.to_string()).or_insert_with(|| TokenBucket::new(None)).clone());
        }
        v.push(per_download);
        Throttle::new(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_limit_only_holds_the_speed_when_the_speed_reaches_it() {
        let none = Throttle::new(vec![TokenBucket::new(None)]);
        assert!(!none.is_holding(1e9), "no limit: never held");
        let t = Throttle::new(vec![TokenBucket::new(Some(10_000_000)), TokenBucket::new(Some(1_000_000))]);
        assert_eq!(t.min_rate(), Some(1_000_000), "the tightest limit governs");
        assert!(t.is_holding(950_000.0), "running at the limit: held by it");
        assert!(!t.is_holding(300_000.0), "far below the limit: the network is what holds it");
    }

    #[tokio::test]
    async fn lifting_a_limit_wakes_connections_that_are_waiting_under_it() {
        let limits = RateLimits::new();
        limits.set_smart(Some(100_000));
        let t = limits.throttle_for(None, TokenBucket::new(None));
        let c = CancellationToken::new();
        t.take(25_000, &c).await.unwrap(); // the burst
        let waiter = {
            let (t, c) = (t.clone(), c.clone());
            tokio::spawn(async move {
                let s = Instant::now();
                t.take(1_000_000, &c).await.unwrap(); // ten seconds of debt at the old limit
                s.elapsed()
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        limits.set_smart(None);
        let waited = waiter.await.unwrap();
        assert!(waited < Duration::from_millis(800), "the waiting connection noticed the lift: {waited:?}");
    }

    #[tokio::test]
    async fn the_smart_cap_applies_to_running_throttles_and_lifts_at_once() {
        let limits = RateLimits::new();
        let t = limits.throttle_for(None, TokenBucket::new(None)); // made before the cap is set
        limits.set_smart(Some(100_000));
        assert_eq!(limits.smart(), Some(100_000));
        assert_eq!(limits.global(), None, "the user's own limit is untouched");
        let slow = drain(&t, 100_000, 10_000).await;
        assert!(slow >= Duration::from_millis(600), "capped: {slow:?}");
        limits.set_smart(None);
        let fast = drain(&t, 10_000_000, 64_000).await;
        assert!(fast < Duration::from_millis(50), "lifted at once: {fast:?}");
    }

    async fn drain(t: &Throttle, total: usize, chunk: usize) -> Duration {
        let c = CancellationToken::new();
        let start = Instant::now();
        let mut left = total;
        while left > 0 {
            let n = chunk.min(left);
            t.take(n, &c).await.unwrap();
            left -= n;
        }
        start.elapsed()
    }

    #[tokio::test]
    async fn unlimited_is_instant_and_limited_is_paced() {
        let free = Throttle::new(vec![TokenBucket::new(None)]);
        assert!(drain(&free, 50_000_000, 64 * 1024).await < Duration::from_millis(100));

        // 1 MiB at 512 KiB/s with a 128 KiB burst -> ~1.75 s.
        let t = Throttle::new(vec![TokenBucket::new(Some(512 * 1024))]);
        let d = drain(&t, 1024 * 1024, 32 * 1024).await;
        assert!(d >= Duration::from_millis(1500) && d <= Duration::from_millis(2600), "{d:?}");
    }

    #[tokio::test]
    async fn tightest_bucket_wins_and_waits_do_not_add() {
        // global 4 MiB/s, per-download 512 KiB/s: should behave like 512 KiB/s, not slower.
        let t = Throttle::new(vec![TokenBucket::new(Some(4 << 20)), TokenBucket::new(Some(512 * 1024))]);
        let d = drain(&t, 1024 * 1024, 32 * 1024).await;
        assert!(d >= Duration::from_millis(1500) && d <= Duration::from_millis(2600), "{d:?}");
    }

    #[tokio::test]
    async fn rate_can_change_while_running() {
        let b = TokenBucket::new(Some(64 * 1024));
        let t = Throttle::new(vec![b.clone()]);
        let c = CancellationToken::new();
        t.take(32 * 1024, &c).await.unwrap(); // burst
        t.take(32 * 1024, &c).await.unwrap();
        b.set_rate(None);
        let start = Instant::now();
        drain(&t, 100_000_000, 1 << 20).await;
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn cancellation_interrupts_the_wait() {
        let t = Throttle::new(vec![TokenBucket::new(Some(1024))]);
        let c = CancellationToken::new();
        t.take(32 * 1024, &c).await.unwrap(); // eats the burst
        let c2 = c.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.cancel();
        });
        let start = Instant::now();
        assert!(matches!(t.take(64 * 1024, &c).await, Err(DownloadError::Cancelled)));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn registry_builds_hierarchies() {
        let l = RateLimits::new();
        l.set_global(Some(50_000_000));
        l.set_category("video", Some(10_000_000));
        assert_eq!((l.global(), l.category("video"), l.category("none")), (Some(50_000_000), Some(10_000_000), None));
        let per = TokenBucket::new(Some(20_000_000));
        // global + smart + category + the download's own; without a category: three.
        assert_eq!(l.throttle_for(Some("video"), per.clone()).buckets.len(), 4);
        assert_eq!(l.throttle_for(None, per).buckets.len(), 3);
        l.set_category("video", None);
        assert_eq!(l.category("video"), None);
    }
}
