//! Adaptive connection count.
//!
//! * [`AdaptiveController`]: pure decision logic. Each interval it sees throughput, error and
//!   latency and answers "use this many connections now". It doubles (16 -> 32 -> 64) while extra
//!   connections still pay off, and backs off (64 -> 32 -> 16) on errors or when throughput really
//!   collapses. New connections get a settling interval before they are judged (a fresh connection
//!   is still handshaking and in TCP slow start), and a speed limit in force freezes the decisions:
//!   a capped throughput says nothing about the network.
//! * [`AdaptiveGate`]: a concurrency limit that can be raised and lowered while in flight.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone)]
pub struct AdaptiveConfig {
    pub enabled: bool,
    /// How often throughput / errors / latency are sampled and a decision is made.
    pub interval: Duration,
    /// A doubling must improve throughput by at least this fraction, or it is reverted.
    pub min_gain: f64,
    /// While holding, throughput below `best * degrade_ratio` for 2 intervals steps down.
    pub degrade_ratio: f64,
    /// Latency above `baseline * latency_factor` counts as congestion.
    pub latency_factor: f64,
    /// Errors (retries) within one interval that trigger an immediate step down.
    pub error_threshold: u32,
    /// Intervals to hold after a step down / plateau before probing upward again.
    pub cooldown_intervals: u32,
    /// Intervals skipped right after a raise, while the new connections open and speed up.
    pub settle_intervals: u32,
    /// Bad intervals in a row (below `best * degrade_ratio`) before stepping down.
    pub degrade_intervals: u32,
    /// How much the first climb multiplies the connections per step (4 -> 16 -> 64); later probes double.
    pub growth: usize,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(1),
            min_gain: 0.05,
            degrade_ratio: 0.50,
            latency_factor: 3.0,
            error_threshold: 3,
            cooldown_intervals: 15,
            settle_intervals: 1,
            degrade_intervals: 3,
            growth: 4,
        }
    }
}

/// What was measured during one interval.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    /// Bytes per second across all connections.
    pub throughput: f64,
    /// Retries / failures during the interval.
    pub errors: u32,
    /// Mean time-to-first-byte of requests that started in the interval.
    pub latency: Option<Duration>,
    /// A speed limit was in force: throughput is capped by it, not by the network.
    pub limited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Ramping,
    Holding,
}

#[derive(Debug)]
pub struct AdaptiveController {
    cfg: AdaptiveConfig,
    limit: usize,
    ceiling: usize,
    /// Highest limit we are currently allowed to ramp to (lowered by errors / plateaus).
    soft_ceiling: usize,
    phase: Phase,
    /// True when `limit` was just raised and its throughput has not been judged yet.
    raised: bool,
    /// Samples still to skip while just-added connections settle.
    settle: u32,
    /// The first climb is over (later climbs are probes, in small steps).
    probed: bool,
    prev_limit: usize,
    prev_throughput: f64,
    best_throughput: f64,
    cooldown: u32,
    low_streak: u32,
    base_latency: Option<Duration>,
    last_throttle_drop: Option<Instant>,
}

impl AdaptiveController {
    pub fn new(cfg: AdaptiveConfig, start: usize, ceiling: usize) -> Self {
        let ceiling = ceiling.max(1);
        let start = start.clamp(1, ceiling);
        Self {
            cfg,
            limit: start,
            ceiling,
            soft_ceiling: ceiling,
            phase: Phase::Ramping,
            raised: false,
            settle: 0,
            probed: false,
            prev_limit: start,
            prev_throughput: 0.0,
            best_throughput: 0.0,
            cooldown: 0,
            low_streak: 0,
            base_latency: None,
            last_throttle_drop: None,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    fn step_down(&mut self) -> usize {
        (self.limit / 2).max(1)
    }

    fn hold(&mut self) {
        self.phase = Phase::Holding;
        self.cooldown = self.cfg.cooldown_intervals;
        self.raised = false;
        self.settle = 0;
        self.low_streak = 0;
    }

    /// Multiply the limit (up to `cap`) and give the new connections time to settle before judging them.
    fn raise(&mut self, throughput: f64, cap: usize, factor: usize) -> Option<usize> {
        self.prev_limit = self.limit;
        self.prev_throughput = throughput;
        self.limit = (self.limit * factor.max(2)).min(cap);
        self.raised = true;
        self.settle = self.cfg.settle_intervals;
        Some(self.limit)
    }

    fn lower_to(&mut self, new: usize, throughput: f64) -> Option<usize> {
        self.probed = true;
        self.soft_ceiling = new; // do not immediately climb back into the trouble
        self.limit = new;
        self.best_throughput = throughput;
        self.hold();
        Some(new)
    }

    /// The server explicitly told us to slow down (HTTP 429 / 503): halve the connection count now,
    /// without waiting for the next sampling interval, and stop ramping back into the limit.
    /// Bursts of throttle responses are debounced to one step down per `interval`.
    pub fn on_throttled(&mut self, now: Instant) -> Option<usize> {
        if self.limit <= 1 {
            return None;
        }
        if self.last_throttle_drop.is_some_and(|t| now.duration_since(t) < self.cfg.interval) {
            return None;
        }
        self.last_throttle_drop = Some(now);
        let new = self.step_down();
        let best = self.best_throughput;
        self.lower_to(new, best)
    }

    /// Feed one interval's measurements; returns the new connection limit if it changed.
    pub fn observe(&mut self, s: Sample) -> Option<usize> {
        if s.throughput <= 0.0 {
            return None; // idle / waiting: nothing to learn
        }
        // Latency baseline = best (lowest) mean seen. More connections naturally wait a little longer for
        // their first byte, so a slow first byte alone is not congestion: only together with collapsed throughput.
        let slow = match (s.latency, self.base_latency) {
            (Some(l), Some(b)) => l.as_secs_f64() > b.as_secs_f64() * self.cfg.latency_factor,
            _ => false,
        };
        if let Some(l) = s.latency {
            self.base_latency = Some(self.base_latency.map_or(l, |b| b.min(l)));
        }
        let collapsed = self.best_throughput > 0.0 && s.throughput < self.best_throughput * self.cfg.degrade_ratio;

        // 1. Errors, or congestion: back off immediately.
        if (s.errors >= self.cfg.error_threshold || (slow && collapsed && !s.limited)) && self.limit > 1 {
            let new = self.step_down();
            return self.lower_to(new, s.throughput);
        }
        // 2. A speed limit caps the throughput: it tells nothing about the network, keep what we have.
        if s.limited {
            self.low_streak = 0;
            return None;
        }

        match self.phase {
            Phase::Ramping => {
                if self.settle > 0 {
                    self.settle -= 1; // the new connections are still opening: do not judge them yet
                    return None;
                }
                if self.raised && s.throughput < self.prev_throughput * (1.0 + self.cfg.min_gain) {
                    // The extra connections did not pay off: revert and settle at the plateau.
                    let back = self.prev_limit;
                    self.limit = back;
                    self.soft_ceiling = back;
                    self.best_throughput = self.prev_throughput.max(s.throughput);
                    self.hold();
                    return Some(back);
                }
                self.raised = false;
                self.best_throughput = self.best_throughput.max(s.throughput);
                if self.limit >= self.soft_ceiling {
                    self.best_throughput = s.throughput;
                    self.hold();
                    return None;
                }
                // The first climb takes big steps (it starts small to be gentle with servers that cap connections).
                let factor = if self.probed { 2 } else { self.cfg.growth };
                self.raise(s.throughput, self.soft_ceiling, factor)
            }
            Phase::Holding => {
                if collapsed {
                    self.low_streak += 1;
                } else {
                    self.low_streak = 0;
                    self.best_throughput = self.best_throughput.max(s.throughput);
                }
                if self.low_streak >= self.cfg.degrade_intervals && self.limit > 1 {
                    let new = self.step_down();
                    return self.lower_to(new, s.throughput);
                }
                self.cooldown = self.cooldown.saturating_sub(1);
                if self.cooldown == 0 && self.limit < self.ceiling {
                    // Conditions may have changed: probe upward again.
                    self.soft_ceiling = self.ceiling;
                    self.phase = Phase::Ramping;
                    self.best_throughput = s.throughput;
                    self.probed = true;
                    return self.raise(s.throughput, self.ceiling, 2);
                }
                None
            }
        }
    }
}

/// Concurrency limit that can change while permits are held.
///
/// Raising adds permits at once. Lowering takes effect as running work finishes: surplus permits
/// are destroyed instead of returned, so already-running segments are never interrupted.
pub struct AdaptiveGate {
    sem: Arc<Semaphore>,
    limit: AtomicUsize,
    pending_forget: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
}

pub struct GatePermit {
    permit: Option<OwnedSemaphorePermit>,
    gate: Arc<AdaptiveGate>,
}

impl AdaptiveGate {
    pub fn new(limit: usize) -> Arc<Self> {
        let limit = limit.max(1);
        Arc::new(Self {
            sem: Arc::new(Semaphore::new(limit)),
            limit: AtomicUsize::new(limit),
            pending_forget: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::SeqCst)
    }

    /// Most connections that were ever in use at the same time.
    pub fn peak_active(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    pub async fn acquire(self: &Arc<Self>) -> GatePermit {
        let permit = self.sem.clone().acquire_owned().await.expect("gate semaphore is never closed");
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        GatePermit { permit: Some(permit), gate: self.clone() }
    }

    pub fn set_limit(&self, new: usize) {
        let new = new.max(1);
        let cur = self.limit.swap(new, Ordering::SeqCst);
        if new > cur {
            let mut add = new - cur;
            // Growth first cancels shrinks that have not been applied yet.
            #[allow(deprecated)] // fetch_update = try_update on newer Rust; the old name builds everywhere
            let _ = self.pending_forget.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |p| {
                let cancel = p.min(add);
                add -= cancel;
                Some(p - cancel)
            });
            if add > 0 {
                self.sem.add_permits(add);
            }
        } else if new < cur {
            let need = cur - new;
            let forgotten = self.sem.forget_permits(need);
            self.pending_forget.fetch_add(need - forgotten, Ordering::SeqCst);
        }
    }
}

impl Drop for GatePermit {
    fn drop(&mut self) {
        self.gate.active.fetch_sub(1, Ordering::SeqCst);
        #[allow(deprecated)] // fetch_update = try_update on newer Rust; the old name builds everywhere
        let shrink = self.gate.pending_forget.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |p| p.checked_sub(1)).is_ok();
        if let Some(p) = self.permit.take() {
            if shrink {
                p.forget();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AdaptiveConfig {
        AdaptiveConfig { cooldown_intervals: 3, settle_intervals: 0, degrade_intervals: 2, growth: 2, ..Default::default() }
    }

    fn s(tp: f64) -> Sample {
        Sample { throughput: tp, errors: 0, latency: None, limited: false }
    }

    /// Throughput model: linear scaling up to `knee` connections, flat after.
    fn tp(limit: usize, knee: usize) -> f64 {
        10.0 * limit.min(knee) as f64
    }

    #[test]
    fn ramps_up_while_it_pays_off_to_the_ceiling() {
        let mut c = AdaptiveController::new(cfg(), 1, 16);
        let mut seen = vec![c.limit()];
        for _ in 0..8 {
            if let Some(n) = c.observe(s(tp(c.limit(), 64))) {
                seen.push(n);
            }
        }
        assert_eq!(seen, vec![1, 2, 4, 8, 16]);
        assert_eq!(c.limit(), 16);
    }

    #[test]
    fn stops_at_the_plateau_and_reverts_useless_connections() {
        let mut c = AdaptiveController::new(cfg(), 2, 16);
        let mut seen = vec![c.limit()];
        for _ in 0..3 {
            if let Some(n) = c.observe(s(tp(c.limit(), 4))) {
                seen.push(n);
            }
        }
        // 2 -> 4 (gain), 4 -> 8 (no gain) -> back to 4, then hold.
        assert_eq!(seen, vec![2, 4, 8, 4]);
        assert_eq!(c.limit(), 4);
        for _ in 0..2 {
            assert_eq!(c.observe(s(tp(4, 4))), None, "holding at the plateau");
        }
    }

    #[test]
    fn errors_step_down_16_8_4_and_block_regrowth() {
        let mut c = AdaptiveController::new(cfg(), 16, 16);
        let bad = Sample { throughput: 100.0, errors: 5, latency: None, limited: false };
        assert_eq!(c.observe(bad), Some(8));
        assert_eq!(c.observe(bad), Some(4));
        // Calm again: it must not immediately climb back into the failing range.
        assert_eq!(c.observe(s(100.0)), None);
        assert_eq!(c.limit(), 4);
    }

    #[test]
    fn throttle_responses_halve_immediately_debounced_and_block_regrowth() {
        let mut c = AdaptiveController::new(cfg(), 16, 16);
        let t0 = Instant::now();
        assert_eq!(c.on_throttled(t0), Some(8));
        // A burst of 429s from the requests already in flight must not collapse us to 1.
        for ms in [1, 50, 400, 900] {
            assert_eq!(c.on_throttled(t0 + Duration::from_millis(ms)), None);
        }
        assert_eq!(c.on_throttled(t0 + Duration::from_millis(1500)), Some(4), "still throttled a full interval later");
        assert_eq!(c.on_throttled(t0 + Duration::from_secs(3)), Some(2));
        assert_eq!(c.on_throttled(t0 + Duration::from_secs(5)), Some(1));
        assert_eq!(c.on_throttled(t0 + Duration::from_secs(9)), None, "cannot go below 1");
        assert_eq!(c.observe(s(100.0)), None, "does not ramp straight back up");
        assert_eq!(c.limit(), 1);
    }

    #[test]
    fn deteriorating_throughput_steps_down_after_bad_intervals_in_a_row() {
        let mut c = AdaptiveController::new(cfg(), 8, 8);
        assert_eq!(c.observe(s(100.0)), None); // at ceiling: settle
        assert_eq!(c.observe(s(30.0)), None); // first bad interval
        assert_eq!(c.observe(s(30.0)), Some(4)); // second: step down
    }

    #[test]
    fn latency_spike_with_collapsed_throughput_counts_as_congestion() {
        let mut c = AdaptiveController::new(cfg(), 8, 16);
        let at = |tp, ms| Sample { throughput: tp, errors: 0, latency: Some(Duration::from_millis(ms)), limited: false };
        assert!(c.observe(at(100.0, 20)).is_some()); // ramps
        assert_eq!(c.observe(at(30.0, 200)), Some(8), "10x latency and the speed collapsed -> halve from 16");
    }

    #[test]
    fn a_slower_first_byte_alone_is_not_congestion() {
        // More connections each wait longer for their first byte; while the speed holds, keep them.
        let mut c = AdaptiveController::new(cfg(), 16, 64);
        let at = |tp, ms| Sample { throughput: tp, errors: 0, latency: Some(Duration::from_millis(ms)), limited: false };
        assert_eq!(c.observe(at(100.0, 20)), Some(32));
        assert_eq!(c.observe(at(190.0, 120)), Some(64), "6x latency but the speed doubled: keep ramping");
        assert_eq!(c.observe(at(370.0, 150)), None);
        assert_eq!(c.limit(), 64);
    }

    #[test]
    fn new_connections_settle_before_they_are_judged() {
        // Real connections open slowly: the first interval after a raise shows little gain.
        let mut c = AdaptiveController::new(AdaptiveConfig { settle_intervals: 1, ..cfg() }, 16, 64);
        assert_eq!(c.observe(s(100.0)), Some(32));
        assert_eq!(c.observe(s(102.0)), None, "still opening: not reverted");
        assert_eq!(c.observe(s(190.0)), Some(64), "judged once settled: it paid off");
        assert_eq!(c.observe(s(200.0)), None);
        assert_eq!(c.observe(s(380.0)), None, "at the ceiling");
        assert_eq!(c.limit(), 64);
    }

    #[test]
    fn the_first_climb_takes_big_steps_to_64() {
        let mut c = AdaptiveController::new(AdaptiveConfig::default(), 4, 64);
        let mut seen = vec![c.limit()];
        for _ in 0..8 {
            if let Some(n) = c.observe(s(tp(c.limit(), 64))) {
                seen.push(n);
            }
        }
        assert_eq!(seen, vec![4, 16, 64], "4 -> 16 -> 64, each judged after settling");
    }

    #[test]
    fn a_speed_limit_freezes_the_decisions() {
        let mut c = AdaptiveController::new(cfg(), 64, 64);
        assert_eq!(c.observe(s(1000.0)), None); // at ceiling: hold, best = 1000
        let capped = Sample { throughput: 50.0, errors: 0, latency: None, limited: true };
        for _ in 0..10 {
            assert_eq!(c.observe(capped), None, "a low capped speed is not a reason to drop connections");
        }
        assert_eq!(c.limit(), 64);
    }

    #[test]
    fn idle_samples_change_nothing_and_it_probes_upward_after_cooldown() {
        let mut c = AdaptiveController::new(cfg(), 4, 8);
        assert_eq!(c.observe(s(0.0)), None);
        // 4 -> 8 gains nothing -> revert to 4 and hold for 3 intervals, then probe again.
        c.observe(s(40.0));
        assert_eq!(c.observe(s(40.0)), Some(4));
        assert_eq!(c.observe(s(40.0)), None);
        assert_eq!(c.observe(s(40.0)), None);
        assert_eq!(c.observe(s(40.0)), Some(8), "re-probe after the cooldown");
    }

    #[tokio::test]
    async fn gate_limits_concurrency_and_resizes_live() {
        let g = AdaptiveGate::new(2);
        let a = g.acquire().await;
        let b = g.acquire().await;
        assert!(tokio::time::timeout(Duration::from_millis(50), g.acquire()).await.is_err());
        g.set_limit(3); // raise: one more may enter right away
        let c = g.acquire().await;
        assert_eq!(g.peak_active(), 3);

        g.set_limit(1); // lower while 3 are held: shrink is applied as they finish
        drop(a);
        drop(b);
        assert!(tokio::time::timeout(Duration::from_millis(50), g.acquire()).await.is_err(), "still 1 running");
        drop(c);
        let d = g.acquire().await; // now exactly one slot exists
        assert!(tokio::time::timeout(Duration::from_millis(50), g.acquire()).await.is_err());
        drop(d);
        g.set_limit(4);
        let _x = (g.acquire().await, g.acquire().await, g.acquire().await, g.acquire().await);
    }
}
