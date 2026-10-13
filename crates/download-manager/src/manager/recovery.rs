//! Never lose a download: a download that failed because of the network or a server's passing trouble (no
//! connection, a timeout, a dropped connection, 5xx, 429) is tried again on its own, from its saved bytes.
//!
//! * **While the network is down** nothing is spent: the download waits ("Waiting for the network") and starts again
//!   as soon as the computer is back online.
//! * **While it is up**, the tries are spaced out (10 s, 30 s, 1 min, 2 min, 5 min, then every 10 min), at most
//!   [`MAX_TRIES`] times (about 3 hours); then it stays failed and Download Doctor explains it.
//! * **After the computer slept** (the loop finds a long gap in time), every waiting download is tried at once.
//! * Anything the user does wins: Retry tries now (and counts again from the start), Pause stops the tries and keeps
//!   the saved bytes, Cancel and Remove forget it. Errors that need the user (sign in, disk full, a broken link,
//!   another file) are never tried again on their own.
//!
//! In memory only: after the app restarts, a failed download waits for the user (it may have been days).

use std::time::UNIX_EPOCH;

use super::*;

/// Tries at most, while the network is up (offline time never counts).
pub(crate) const MAX_TRIES: u32 = 20;
/// The wait before try `n` (1-based); the last one repeats.
pub(crate) const DELAYS: [Duration; 6] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
    Duration::from_secs(600),
];
/// A gap this long between two turns of the one-second loop means the computer slept (or was frozen).
const WAKE_GAP: Duration = Duration::from_secs(30);

/// Error codes (`explain::error_code`) that are the network's or a server's passing trouble.
pub(crate) fn recoverable(code: &str) -> bool {
    matches!(code, "dns" | "timeout" | "connection" | "connection_reset" | "incomplete")
        || code.strip_prefix("http_").and_then(|c| c.parse::<u16>().ok()).is_some_and(|s| matches!(s, 408 | 425 | 429 | 500..=599))
}

/// One download waiting to be tried again.
#[derive(Debug, Clone)]
pub(super) struct Waiting {
    /// Tries already made on its own.
    pub tries: u32,
    pub at: Instant,
    /// The same moment for the UI (ms since 1970).
    pub at_ms: u64,
    /// Seen offline while waiting: it starts as soon as the network is back.
    pub offline: bool,
}

#[derive(Default)]
pub(super) struct Recovery {
    pub waiting: HashMap<i64, Waiting>,
    /// Tries made, kept while a try runs (a run that fails again continues the count).
    pub tries: HashMap<i64, u32>,
    /// The loop's last turn (both clocks: one of them may stop while the computer sleeps).
    last_turn: Option<(Instant, std::time::SystemTime)>,
}

/// The wait before try number `n` (1-based) given `delays` (the last one repeats).
pub(super) fn delay_for(delays: &[Duration], n: u32) -> Duration {
    let i = (n.max(1) as usize - 1).min(delays.len().saturating_sub(1));
    delays.get(i).copied().unwrap_or(Duration::from_secs(60))
}

/// Whether the time between two turns of the loop shows the computer slept: either clock jumped by `gap` or more.
pub(super) fn slept(prev: (Instant, std::time::SystemTime), now: (Instant, std::time::SystemTime), gap: Duration) -> bool {
    let mono = now.0.saturating_duration_since(prev.0);
    let wall = now.1.duration_since(prev.1).unwrap_or_default();
    mono.max(wall) >= gap
}

fn ms_from_now(d: Duration) -> u64 {
    std::time::SystemTime::now().checked_add(d).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64)
}

impl DownloadManager {
    fn recovery(&self) -> std::sync::MutexGuard<'_, Recovery> {
        self.inner.recovery.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A run of `id` failed with `message`: wait and try again if the cause can pass on its own.
    pub(super) fn recover_later(&self, id: i64, message: &str) {
        let code = crate::explain::error_code(message);
        let mut r = self.recovery();
        let tries = r.tries.remove(&id).unwrap_or(0);
        if !recoverable(&code) || tries >= self.inner.cfg.recovery_max_tries || self.inner.shutting_down.load(Ordering::Relaxed) {
            r.waiting.remove(&id);
            return;
        }
        let d = delay_for(&self.inner.cfg.recovery_delays, tries + 1);
        tracing::info!(id, code, try_ = tries + 1, wait_ms = d.as_millis() as u64, "trying again on its own later");
        r.waiting.insert(id, Waiting { tries, at: Instant::now() + d, at_ms: ms_from_now(d), offline: false });
    }

    /// The user acted on `id` (Retry, Pause, Cancel, Remove, Download again): its own tries stop and count anew.
    pub(super) fn forget_recovery(&self, id: i64) {
        let mut r = self.recovery();
        r.waiting.remove(&id);
        r.tries.remove(&id);
    }

    /// A run of `id` ended well (or was paused or cancelled): nothing more to try.
    pub(super) fn recovered(&self, id: i64) {
        self.forget_recovery(id);
    }

    /// The UI's view of `id`'s next try (`None`: not waiting).
    pub(super) fn auto_retry_of(&self, id: i64) -> Option<AutoRetryDto> {
        self.recovery().waiting.get(&id).map(|w| AutoRetryDto {
            at_ms: (!w.offline).then_some(w.at_ms),
            waiting_for_network: w.offline,
            tries: w.tries,
            max_tries: self.inner.cfg.recovery_max_tries,
        })
    }

    pub(super) fn recovering(&self) -> bool {
        !self.recovery().waiting.is_empty()
    }

    /// The computer woke up (or the network came back): every waiting download is tried as soon as it is online.
    pub fn system_resumed(&self) {
        let now = Instant::now();
        let mut r = self.recovery();
        for w in r.waiting.values_mut() {
            w.at = now;
            w.at_ms = ms_from_now(Duration::ZERO);
        }
        tracing::info!(waiting = r.waiting.len(), "the computer is back: trying waiting downloads now");
    }

    /// One turn of the queue loop. `online`: what the queue loop judged (`Some(false)` only once confirmed);
    /// `seen_offline`: the last look said offline (a try then waits for the next look, without saying so yet).
    pub(super) async fn recovery_turn(&self, online: Option<bool>, seen_offline: bool) {
        if self.inner.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let now = (Instant::now(), std::time::SystemTime::now());
        let woke = {
            let mut r = self.recovery();
            let woke = r.last_turn.is_some_and(|prev| slept(prev, now, WAKE_GAP));
            r.last_turn = Some(now);
            woke
        };
        if woke {
            self.system_resumed();
        }
        let (due, changed) = {
            let mut r = self.recovery();
            let mut changed = Vec::new();
            if online == Some(false) {
                // Offline: wait without spending a try, and start as soon as the network is back.
                for (id, w) in r.waiting.iter_mut() {
                    if !w.offline {
                        w.offline = true;
                        changed.push(*id);
                    }
                }
                (Vec::new(), changed)
            } else if seen_offline {
                (Vec::new(), changed)
            } else {
                let due: Vec<i64> = r.waiting.iter().filter(|(_, w)| w.offline || w.at <= now.0).map(|(id, _)| *id).collect();
                for id in &due {
                    if let Some(w) = r.waiting.remove(id) {
                        r.tries.insert(*id, w.tries + 1);
                    }
                }
                (due, changed)
            }
        };
        for id in changed {
            if let Ok(dto) = self.dto_for(id).await {
                self.emit(ManagerEvent::Updated { download: dto });
            }
        }
        for id in due {
            let tries = self.recovery().tries.get(&id).copied().unwrap_or(1);
            match self.load(id).await {
                Ok(rec) if rec.status == DownloadStatus::Failed && !self.is_active(id) => {
                    self.note_event(id, "info", &format!("Trying again on its own (try {tries} of {})", self.inner.cfg.recovery_max_tries))
                        .await;
                    if let Err(e) = self.start_inner(id).await {
                        tracing::warn!(id, error = %e, "could not try again on its own");
                        self.recovery().tries.remove(&id);
                    }
                }
                // Changed meanwhile (the user acted, or it was removed): nothing to do.
                _ => {
                    self.recovery().tries.remove(&id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_passing_troubles_are_tried_again() {
        for c in
            ["dns", "timeout", "connection", "connection_reset", "incomplete", "http_408", "http_429", "http_500", "http_503", "http_504"]
        {
            assert!(recoverable(c), "{c}");
        }
        for c in ["http_401", "http_403", "http_404", "http_410", "http_416", "disk_full", "access_denied", "source_changed", "web_page"] {
            assert!(!recoverable(c), "{c}");
        }
        for c in ["checksum_mismatch", "file_exists", "cancelled", "invalid_request", "redirect_refused", "http_x", "other", ""] {
            assert!(!recoverable(c), "{c}");
        }
    }

    #[test]
    fn waits_grow_then_repeat() {
        let secs: Vec<u64> = (1..=8).map(|n| delay_for(&DELAYS, n).as_secs()).collect();
        assert_eq!(secs, [10, 30, 60, 120, 300, 600, 600, 600]);
        assert_eq!(delay_for(&DELAYS, 0).as_secs(), 10);
        assert_eq!(delay_for(&[], 3), Duration::from_secs(60));
        let total: u64 = (1..=MAX_TRIES).map(|n| delay_for(&DELAYS, n).as_secs()).sum();
        assert!((2 * 3600..4 * 3600).contains(&total), "about 3 hours of trying: {total} s");
    }

    #[test]
    fn a_long_gap_on_either_clock_is_a_sleep() {
        let (i, w) = (Instant::now(), std::time::SystemTime::now());
        let s = Duration::from_secs;
        assert!(!slept((i, w), (i + s(1), w + s(1)), WAKE_GAP));
        // Linux: the monotonic clock stops while asleep, the wall clock does not.
        assert!(slept((i, w), (i + s(1), w + s(3600)), WAKE_GAP));
        // Windows: both go on.
        assert!(slept((i, w), (i + s(3600), w + s(3600)), WAKE_GAP));
        // The wall clock set back by hand: not a sleep.
        assert!(!slept((i, w + s(7200)), (i + s(1), w), WAKE_GAP));
    }
}
