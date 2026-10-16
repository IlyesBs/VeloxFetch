//! Network usage (issue #45): every byte VeloxFetch receives from the network (downloads and videos) is counted, and
//! saved per local day in the database (`persistence`, table `network_usage`) at most every [`SAVE_EVERY`] and when the
//! app closes. It belongs to no download: deleting downloads or clearing the history keeps it, and an update or a
//! reinstall keeps the database, so the dashboard's "Network usage" never starts again from 0. A save that fails keeps
//! its bytes for the next one.

use std::sync::atomic::{AtomicU64, Ordering};

use persistence::NetworkUsage;

use super::*;

/// How often the bytes received are saved (and the saved numbers read back).
pub(super) const SAVE_EVERY: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct Usage {
    /// Received, not saved yet.
    pending: AtomicU64,
    saved: Mutex<Saved>,
    /// One save at a time.
    saving: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Saved {
    numbers: NetworkUsage,
    /// The day and month `numbers` were read for (`YYYY-MM-DD`, `YYYY-MM`).
    day: String,
    month: String,
    at: Option<Instant>,
}

/// `saved` (read for `saved_day` / `saved_month`) plus `pending`, as seen on `day` / `month`: a new day starts at the
/// bytes not saved yet, a new month likewise.
pub(super) fn add_pending(saved: NetworkUsage, saved_day: &str, saved_month: &str, pending: u64, day: &str, month: &str) -> NetworkUsage {
    NetworkUsage {
        total: saved.total.saturating_add(pending),
        today: if saved_day == day { saved.today.saturating_add(pending) } else { pending },
        month: if saved_month == month { saved.month.saturating_add(pending) } else { pending },
    }
}

impl DownloadManager {
    /// Bytes just received from the network (a download's or a video's).
    pub(super) fn count_received(&self, bytes: u64) {
        if bytes > 0 {
            self.inner.session_bytes.fetch_add(bytes, Ordering::Relaxed);
            self.inner.usage.pending.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// Today's date and month by the app's clock (the computer's local time).
    fn usage_day(&self) -> (String, String) {
        let now = (self.inner.cfg.clock.0)();
        (now.format("%Y-%m-%d").to_string(), now.format("%Y-%m").to_string())
    }

    /// Save what was received and read the numbers back: when [`SAVE_EVERY`] passed or the day changed, or always with
    /// `force` (the app closing). Never waits for another save unless `force`.
    pub(super) async fn save_usage(&self, force: bool) {
        let (day, month) = self.usage_day();
        let usage = &self.inner.usage;
        let _one = if force {
            usage.saving.lock().await
        } else {
            let due = {
                let s = usage.saved.lock().unwrap();
                s.at.is_none_or(|t| t.elapsed() >= SAVE_EVERY) || s.day != day
            };
            match (due, usage.saving.try_lock()) {
                (true, Ok(g)) => g,
                _ => return,
            }
        };
        let bytes = usage.pending.swap(0, Ordering::Relaxed);
        let store = self.inner.store.clone();
        let (d, m) = (day.clone(), month.clone());
        match blocking(move || {
            store.add_network_usage(&d, bytes)?;
            store.network_usage(&d, &m)
        })
        .await
        {
            Ok(numbers) => *usage.saved.lock().unwrap() = Saved { numbers, day, month, at: Some(Instant::now()) },
            Err(e) => {
                // Kept for the next save (the add and the read are one call: a failed read after a good add is not
                // possible to tell apart, so the read alone failing would count twice; adds are what fail in practice).
                usage.pending.fetch_add(bytes, Ordering::Relaxed);
                usage.saved.lock().unwrap().at = Some(Instant::now());
                tracing::warn!(error = %e, "network usage not saved: tried again soon");
            }
        }
    }

    /// The network usage now: saved plus not saved yet.
    pub(super) fn network_usage_now(&self) -> NetworkUsage {
        let (day, month) = self.usage_day();
        let s = self.inner.usage.saved.lock().unwrap();
        add_pending(s.numbers, &s.day, &s.month, self.inner.usage.pending.load(Ordering::Relaxed), &day, &month)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_not_saved_yet_count_and_a_new_day_or_month_starts_from_them() {
        let saved = NetworkUsage { total: 1000, today: 100, month: 400 };
        assert_eq!(
            add_pending(saved, "2026-10-15", "2026-10", 50, "2026-10-15", "2026-10"),
            NetworkUsage { total: 1050, today: 150, month: 450 }
        );
        assert_eq!(
            add_pending(saved, "2026-10-15", "2026-10", 50, "2026-10-16", "2026-10"),
            NetworkUsage { total: 1050, today: 50, month: 450 }
        );
        assert_eq!(
            add_pending(saved, "2026-10-31", "2026-10", 50, "2026-11-01", "2026-11"),
            NetworkUsage { total: 1050, today: 50, month: 50 }
        );
        assert_eq!(add_pending(NetworkUsage { total: u64::MAX, today: 0, month: 0 }, "", "", 5, "d", "m").total, u64::MAX);
    }
}
