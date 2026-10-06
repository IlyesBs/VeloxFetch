//! Download priorities and the slot scheduler that decides which downloads run.
//!
//! At most `max_slots` downloads run at once. When a slot frees up it goes to the waiting
//! download with the highest [`Priority`]; ties are served first-come, first-served.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::error::{DownloadError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Lowest = 0,
    Low = 1,
    #[default]
    Normal = 2,
    High = 3,
    Highest = 4,
}

impl Priority {
    pub fn as_i64(self) -> i64 {
        self as i64
    }

    pub fn from_i64(v: i64) -> Self {
        match v {
            i64::MIN..=0 => Self::Lowest,
            1 => Self::Low,
            2 => Self::Normal,
            3 => Self::High,
            _ => Self::Highest,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "lowest" => Self::Lowest,
            "low" => Self::Low,
            "normal" => Self::Normal,
            "high" => Self::High,
            "highest" => Self::Highest,
            _ => return None,
        })
    }
}

struct Waiter {
    key: u64,
    prio: Priority,
    seq: u64,
    tx: oneshot::Sender<()>,
}

struct Inner {
    max: usize,
    /// While set, nothing new is started; running downloads are unaffected and waiters keep waiting.
    paused: bool,
    running: usize,
    /// Arrival counter for ordinary waiters (starts high so `front_seq` always sorts before it).
    seq: u64,
    /// Arrival counter for waiters that were marked with [`SlotScheduler::prefer`].
    front_seq: u64,
    /// Keys whose next `acquire` jumps ahead of ordinary waiters of the same priority.
    preferred: HashSet<u64>,
    waiters: Vec<Waiter>,
}

impl Inner {
    /// Hand free slots to the best waiters (highest priority, then oldest).
    fn dispatch(&mut self) {
        while !self.paused && self.running < self.max {
            let Some(best) =
                self.waiters.iter().enumerate().max_by(|(_, a), (_, b)| a.prio.cmp(&b.prio).then(b.seq.cmp(&a.seq))).map(|(i, _)| i)
            else {
                return;
            };
            let w = self.waiters.remove(best);
            if w.tx.send(()).is_ok() {
                self.running += 1;
            } // else: the waiter went away; try the next one
        }
    }
}

pub struct SlotScheduler {
    inner: Mutex<Inner>,
    next_key: AtomicU64,
}

/// Holds one running slot; the slot is handed to the next waiter on drop.
pub struct SlotGuard {
    sched: Arc<SlotScheduler>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut i = self.sched.inner.lock().unwrap();
        i.running -= 1;
        i.dispatch();
    }
}

impl SlotScheduler {
    pub fn new(max_slots: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                max: max_slots.max(1),
                paused: false,
                running: 0,
                seq: 1 << 32,
                front_seq: 0,
                preferred: HashSet::new(),
                waiters: Vec::new(),
            }),
            next_key: AtomicU64::new(1 << 32), // keeps generated keys clear of database ids
        })
    }

    /// A unique key for downloads that have no id yet.
    pub fn next_key(&self) -> u64 {
        self.next_key.fetch_add(1, Ordering::Relaxed)
    }

    pub fn running(&self) -> usize {
        self.inner.lock().unwrap().running
    }

    pub fn queued(&self) -> usize {
        self.inner.lock().unwrap().waiters.len()
    }

    /// Change how many downloads may run at once (takes effect immediately for waiting ones).
    pub fn set_max_slots(&self, n: usize) {
        let mut i = self.inner.lock().unwrap();
        i.max = n.max(1);
        i.dispatch();
    }

    /// Make `key`'s next `acquire` go ahead of ordinary waiters of the same priority (still behind
    /// higher priorities). Used when the queue resumes downloads it had paused: they were running
    /// before the pause, so they should not queue up behind downloads that never started.
    pub fn prefer(&self, key: u64) {
        self.inner.lock().unwrap().preferred.insert(key);
    }

    pub fn max_slots(&self) -> usize {
        self.inner.lock().unwrap().max
    }

    /// Stop (`true`) or resume (`false`) handing out slots. Running downloads keep their slot;
    /// waiting ones stay queued in order until the gate reopens.
    pub fn set_paused(&self, paused: bool) {
        let mut i = self.inner.lock().unwrap();
        i.paused = paused;
        i.dispatch();
    }

    /// Is `key` still waiting for a slot (as opposed to running, or unknown)?
    pub fn is_waiting(&self, key: u64) -> bool {
        self.inner.lock().unwrap().waiters.iter().any(|w| w.key == key)
    }

    pub fn is_paused(&self) -> bool {
        self.inner.lock().unwrap().paused
    }

    /// Re-prioritise a *waiting* download. Returns false if `key` is not queued.
    pub fn set_priority(&self, key: u64, prio: Priority) -> bool {
        let mut i = self.inner.lock().unwrap();
        match i.waiters.iter_mut().find(|w| w.key == key) {
            Some(w) => {
                w.prio = prio;
                true
            }
            None => false,
        }
    }

    /// Wait for a running slot. `Err(Cancelled)` if `cancel` fires first (no slot is leaked).
    pub async fn acquire(self: &Arc<Self>, key: u64, prio: Priority, cancel: &CancellationToken) -> Result<SlotGuard> {
        let (tx, mut rx) = oneshot::channel();
        {
            let mut i = self.inner.lock().unwrap();
            let seq = if i.preferred.remove(&key) {
                i.front_seq += 1;
                i.front_seq
            } else {
                i.seq += 1;
                i.seq
            };
            i.waiters.push(Waiter { key, prio, seq, tx });
            i.dispatch();
        }
        tokio::select! {
            _ = &mut rx => Ok(SlotGuard { sched: self.clone() }),
            _ = cancel.cancelled() => {
                let mut i = self.inner.lock().unwrap();
                if rx.try_recv().is_ok() {
                    // Granted in the same instant we were cancelled: give the slot back.
                    i.running -= 1;
                    i.dispatch();
                } else {
                    i.waiters.retain(|w| w.key != key);
                }
                Err(DownloadError::Cancelled)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    #[test]
    fn priority_ordering_and_conversion() {
        assert!(Priority::Highest > Priority::High && Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low && Priority::Low > Priority::Lowest);
        assert_eq!(Priority::default(), Priority::Normal);
        for p in [Priority::Lowest, Priority::Low, Priority::Normal, Priority::High, Priority::Highest] {
            assert_eq!(Priority::from_i64(p.as_i64()), p);
        }
        assert_eq!(Priority::parse("HIGH"), Some(Priority::High));
    }

    #[tokio::test]
    async fn slots_go_to_the_highest_priority_then_fifo() {
        let s = SlotScheduler::new(1);
        let c = CancellationToken::new();
        let first = s.acquire(1, Priority::Normal, &c).await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for (key, prio) in
            [(10, Priority::Low), (11, Priority::Normal), (12, Priority::Highest), (13, Priority::Normal), (14, Priority::Lowest)]
        {
            let (s, c, order) = (s.clone(), c.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let g = s.acquire(key, prio, &c).await.unwrap();
                order.lock().unwrap().push(key);
                drop(g);
            }));
            settle().await; // fix arrival order
        }
        assert_eq!((s.running(), s.queued()), (1, 5));
        drop(first);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![12, 11, 13, 10, 14]);
    }

    #[tokio::test]
    async fn raising_priority_of_a_waiter_reorders_the_queue() {
        let s = SlotScheduler::new(1);
        let c = CancellationToken::new();
        let first = s.acquire(1, Priority::Normal, &c).await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for key in [20u64, 21, 22] {
            let (s, c, order) = (s.clone(), c.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let g = s.acquire(key, Priority::Low, &c).await.unwrap();
                order.lock().unwrap().push(key);
                drop(g);
            }));
            settle().await;
        }
        assert!(s.set_priority(22, Priority::High));
        assert!(!s.set_priority(999, Priority::High));
        drop(first);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![22, 20, 21]);
    }

    #[tokio::test]
    async fn cancelled_waiters_are_skipped_and_slots_never_leak() {
        let s = SlotScheduler::new(1);
        let g1 = s.acquire(1, Priority::Normal, &CancellationToken::new()).await.unwrap();
        let cancel = CancellationToken::new();
        let (s2, c2) = (s.clone(), cancel.clone());
        let waiter = tokio::spawn(async move { s2.acquire(2, Priority::Highest, &c2).await.map(|_| ()) });
        settle().await;
        cancel.cancel();
        assert!(matches!(waiter.await.unwrap(), Err(DownloadError::Cancelled)));
        assert_eq!(s.queued(), 0);
        drop(g1);
        // The freed slot is usable: nothing was leaked by the cancelled waiter.
        let _g = tokio::time::timeout(Duration::from_secs(1), s.acquire(3, Priority::Low, &CancellationToken::new()))
            .await
            .expect("slot available")
            .unwrap();
        assert_eq!(s.running(), 1);
    }

    #[tokio::test]
    async fn max_slots_can_grow_at_runtime() {
        let s = SlotScheduler::new(1);
        let c = CancellationToken::new();
        let _a = s.acquire(1, Priority::Normal, &c).await.unwrap();
        let (s2, c2) = (s.clone(), c.clone());
        let t = tokio::spawn(async move { s2.acquire(2, Priority::Normal, &c2).await.map(|_| ()) });
        settle().await;
        assert_eq!(s.queued(), 1);
        s.set_max_slots(2);
        t.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn paused_gate_holds_waiters_and_releases_them_in_order() {
        let s = SlotScheduler::new(2);
        s.set_paused(true);
        let c = CancellationToken::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for (key, prio) in [(1u64, Priority::Low), (2, Priority::High)] {
            let (s, c, order) = (s.clone(), c.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let g = s.acquire(key, prio, &c).await.unwrap();
                order.lock().unwrap().push(key);
                tokio::time::sleep(Duration::from_millis(50)).await;
                drop(g);
            }));
            settle().await;
        }
        assert_eq!((s.running(), s.queued()), (0, 2), "nothing starts while paused");
        assert!(s.is_paused());
        s.set_paused(false);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(order.lock().unwrap().len(), 2);
        assert!(!s.is_paused());
    }

    #[tokio::test]
    async fn pausing_the_gate_does_not_disturb_running_slots_and_waiters_can_still_cancel() {
        let s = SlotScheduler::new(1);
        let c = CancellationToken::new();
        let running = s.acquire(1, Priority::Normal, &c).await.unwrap();
        s.set_paused(true);
        assert_eq!(s.running(), 1);
        let cancel = CancellationToken::new();
        let (s2, c2) = (s.clone(), cancel.clone());
        let w = tokio::spawn(async move { s2.acquire(2, Priority::Normal, &c2).await.map(|_| ()) });
        settle().await;
        cancel.cancel();
        assert!(matches!(w.await.unwrap(), Err(DownloadError::Cancelled)));
        drop(running); // frees the slot but the gate is closed: no dispatch, no panic
        assert_eq!((s.running(), s.queued()), (0, 0));
    }

    #[tokio::test]
    async fn preferred_waiters_jump_ahead_of_ordinary_ones_of_the_same_priority_only() {
        let s = SlotScheduler::new(1);
        let c = CancellationToken::new();
        let first = s.acquire(1, Priority::Normal, &c).await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        s.prefer(31);
        s.prefer(32);
        let mut tasks = Vec::new();
        // 30 arrives first (ordinary), then 31 and 32 (preferred), then 33 (ordinary, higher priority).
        for (key, prio) in [(30u64, Priority::Normal), (31, Priority::Normal), (32, Priority::Normal), (33, Priority::High)] {
            let (s, c, order) = (s.clone(), c.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let g = s.acquire(key, prio, &c).await.unwrap();
                order.lock().unwrap().push(key);
                drop(g);
            }));
            settle().await;
        }
        drop(first);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![33, 31, 32, 30], "priority first, then preferred (in arrival order), then the rest");
    }

    #[tokio::test]
    async fn prefer_is_consumed_once() {
        let s = SlotScheduler::new(1);
        let c = CancellationToken::new();
        s.prefer(5);
        drop(s.acquire(5, Priority::Normal, &c).await.unwrap());
        let hold = s.acquire(1, Priority::Normal, &c).await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for key in [6u64, 5] {
            // 5 acquires again: no longer preferred, so it queues behind 6.
            let (s, c, order) = (s.clone(), c.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let g = s.acquire(key, Priority::Normal, &c).await.unwrap();
                order.lock().unwrap().push(key);
                drop(g);
            }));
            settle().await;
        }
        drop(hold);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![6, 5]);
    }
}
