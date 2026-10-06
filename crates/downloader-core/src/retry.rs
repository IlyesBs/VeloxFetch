//! Retry engine: error classification, exponential backoff with jitter, `Retry-After`.

use std::future::Future;
use std::io::ErrorKind;
use std::time::Duration;

use http_client::HttpError;
use tokio_util::sync::CancellationToken;

use crate::error::{DownloadError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Transient: try again automatically.
    Retryable,
    /// Retrying cannot help (404, bad URL, resource changed, checksum mismatch...).
    NonRetryable,
    /// Needs the user: credentials, disk full, permissions, existing file...
    UserActionRequired,
}

pub fn classify(e: &DownloadError) -> ErrorClass {
    use ErrorClass::*;
    match e {
        DownloadError::Http(h) => match h {
            HttpError::Status { status, .. } => match status {
                401 | 403 | 407 => UserActionRequired,
                408 | 425 | 429 | 500 | 502 | 503 | 504 => Retryable,
                _ => NonRetryable,
            },
            other if other.is_retryable() => Retryable,
            _ => NonRetryable,
        },
        DownloadError::Io(io) => match io.kind() {
            ErrorKind::StorageFull | ErrorKind::QuotaExceeded | ErrorKind::PermissionDenied => UserActionRequired,
            ErrorKind::Interrupted | ErrorKind::TimedOut | ErrorKind::WouldBlock => Retryable,
            _ => NonRetryable,
        },
        DownloadError::ShortBody { .. } => Retryable, // connection dropped mid-body
        DownloadError::FileExists(_) => UserActionRequired,
        _ => NonRetryable,
    }
}

/// The server asked us to slow down (HTTP 429 / 503). That is a signal to use fewer connections,
/// not evidence that this particular request is broken.
pub fn is_throttle(e: &DownloadError) -> bool {
    matches!(e, DownloadError::Http(HttpError::Status { status: 429 | 503, .. }))
}

fn retry_after(e: &DownloadError) -> Option<Duration> {
    match e {
        DownloadError::Http(HttpError::Status { retry_after, .. }) => *retry_after,
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Retries for ordinary transient failures (timeouts, resets, 5xx...).
    pub max_retries: u32,
    /// Extra, separate budget for throttling responses (429 / 503): a rate-limited segment keeps
    /// waiting its turn while the connection count is reduced, instead of burning `max_retries`.
    pub max_throttle_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    /// Upper bound honoured for a server's `Retry-After`.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            max_throttle_retries: 40,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(300),
        }
    }
}

impl RetryPolicy {
    pub fn none() -> Self {
        Self { max_retries: 0, ..Self::default() }
    }

    /// Delay before retry number `attempt` (0-based): exponential backoff with
    /// "equal jitter" (half fixed, half random), raised to `Retry-After` if larger.
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let exp = self.base_delay.saturating_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX)).min(self.max_delay);
        let half = exp / 2;
        let jitter = Duration::from_nanos(fastrand::u64(0..=half.as_nanos().min(u64::MAX as u128) as u64));
        let d = half + jitter;
        match retry_after {
            Some(ra) => d.max(ra.min(self.max_retry_after)),
            None => d,
        }
    }
}

/// Run `op` until it succeeds, fails with a non-retryable error, or the retry budget is spent.
/// `on_retry(next_attempt, &error, delay)` fires before each sleep. Cancellation aborts the wait.
pub async fn with_retry<T, F, Fut, R>(policy: &RetryPolicy, cancel: &CancellationToken, mut on_retry: R, mut op: F) -> Result<T>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T>>,
    R: FnMut(u32, &DownloadError, Duration),
{
    let mut attempt = 0;
    let mut throttled = 0;
    loop {
        let e = match op(attempt).await {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        if cancel.is_cancelled() || matches!(e, DownloadError::Cancelled) {
            return Err(e);
        }
        if classify(&e) != ErrorClass::Retryable {
            return Err(e);
        }
        let throttle = is_throttle(&e);
        if (throttle && throttled >= policy.max_throttle_retries) || (!throttle && attempt >= policy.max_retries) {
            return Err(e);
        }
        // Throttled requests back off on their own (capped) curve, honoring Retry-After.
        let step = if throttle { throttled.min(6) } else { attempt };
        let d = policy.delay(step, retry_after(&e));
        on_retry(attempt + throttled + 1, &e, d);
        tokio::select! {
            _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
            _ = tokio::time::sleep(d) => {}
        }
        if throttle {
            throttled += 1;
        } else {
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn status(s: u16) -> DownloadError {
        DownloadError::Http(HttpError::Status { status: s, retry_after: None })
    }

    #[test]
    fn classification() {
        assert_eq!(classify(&status(503)), ErrorClass::Retryable);
        assert_eq!(classify(&status(429)), ErrorClass::Retryable);
        assert_eq!(classify(&status(404)), ErrorClass::NonRetryable);
        assert_eq!(classify(&status(401)), ErrorClass::UserActionRequired);
        assert_eq!(classify(&DownloadError::ShortBody { index: 0, got: 1, expected: 2 }), ErrorClass::Retryable);
        assert_eq!(classify(&DownloadError::Io(std::io::Error::from(ErrorKind::StorageFull))), ErrorClass::UserActionRequired);
        assert_eq!(classify(&DownloadError::ChecksumMismatch { expected: "a".into(), actual: "b".into() }), ErrorClass::NonRetryable);
    }

    #[test]
    fn backoff_grows_is_capped_and_honors_retry_after() {
        let p = RetryPolicy {
            max_retries: 10,
            max_throttle_retries: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
            max_retry_after: Duration::from_secs(60),
        };
        for _ in 0..200 {
            let d0 = p.delay(0, None);
            assert!(d0 >= Duration::from_millis(50) && d0 <= Duration::from_millis(100), "{d0:?}");
            let d3 = p.delay(3, None);
            assert!(d3 >= Duration::from_millis(400) && d3 <= Duration::from_millis(800), "{d3:?}");
            let big = p.delay(40, None);
            assert!(big >= Duration::from_millis(500) && big <= Duration::from_secs(1), "{big:?}");
            assert!(p.delay(0, Some(Duration::from_secs(7))) >= Duration::from_secs(7));
            assert!(p.delay(0, Some(Duration::from_secs(9999))) <= Duration::from_secs(60));
        }
    }

    #[tokio::test]
    async fn throttling_has_its_own_budget() {
        let p = RetryPolicy {
            max_retries: 1,
            max_throttle_retries: 6,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            ..Default::default()
        };
        let c = CancellationToken::new();
        // Five 429s in a row would blow a max_retries=1 budget, but throttling is budgeted separately.
        let n = AtomicU32::new(0);
        let r = with_retry(
            &p,
            &c,
            |_, _, _| {},
            |_| async {
                if n.fetch_add(1, Ordering::SeqCst) < 5 {
                    Err(status(429))
                } else {
                    Ok(())
                }
            },
        )
        .await;
        assert!(r.is_ok());
        // ...and it is still finite.
        let n = AtomicU32::new(0);
        let r: Result<()> = with_retry(
            &p,
            &c,
            |_, _, _| {},
            |_| async {
                n.fetch_add(1, Ordering::SeqCst);
                Err(status(503))
            },
        )
        .await;
        assert!(r.is_err() && n.load(Ordering::SeqCst) == 7);
        assert!(is_throttle(&status(429)) && is_throttle(&status(503)) && !is_throttle(&status(500)));
    }

    #[tokio::test]
    async fn retries_then_succeeds_and_stops_on_non_retryable() {
        let p = RetryPolicy { base_delay: Duration::from_millis(1), max_delay: Duration::from_millis(2), ..Default::default() };
        let c = CancellationToken::new();
        let n = AtomicU32::new(0);
        let seen = AtomicU32::new(0);
        let v = with_retry(
            &p,
            &c,
            |_, _, _| {
                seen.fetch_add(1, Ordering::SeqCst);
            },
            |_| async {
                if n.fetch_add(1, Ordering::SeqCst) < 3 {
                    Err(status(503))
                } else {
                    Ok(7)
                }
            },
        )
        .await
        .unwrap();
        assert_eq!((v, n.load(Ordering::SeqCst), seen.load(Ordering::SeqCst)), (7, 4, 3));

        let n = AtomicU32::new(0);
        let r: Result<()> = with_retry(
            &p,
            &c,
            |_, _, _| {},
            |_| async {
                n.fetch_add(1, Ordering::SeqCst);
                Err(status(404))
            },
        )
        .await;
        assert!(r.is_err() && n.load(Ordering::SeqCst) == 1);

        let n = AtomicU32::new(0);
        let p2 = RetryPolicy { max_retries: 2, ..p };
        let r: Result<()> = with_retry(
            &p2,
            &c,
            |_, _, _| {},
            |_| async {
                n.fetch_add(1, Ordering::SeqCst);
                Err(status(500))
            },
        )
        .await;
        assert!(r.is_err() && n.load(Ordering::SeqCst) == 3);
    }
}
